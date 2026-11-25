//! 真实执行环境；远端只注入执行器端口，禁止借用 Harness 的 Git/Fetch。
use async_trait::async_trait;
use rust_cli::cloud::{self, CloudPorts, CloudSessionConfig};
use rust_cli_base::clock::SystemClock;
use rust_cli_base::secret::RedactedString;
use rust_cli_env_local::{LocalEnvironment, ShellConfig};
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, ExecClient};
use rust_cli_exec_transport::stdio::{ChildCommand, StdioConnector};
use rust_cli_kernel_api::controller_services::{ControllerServices, Fetch};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_registry::{EnvironmentRegistry, PrimaryRegistry};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::model::ModelPort;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{ArtifactStore, Store};
use rust_cli_kernel_api::testing::{MemoryArtifacts, environments, selection};
use rust_cli_machines::Provisioner;
use rust_cli_node_store::{NodeIds, NodeStore};
use rust_cli_paths::{StoragePaths, StorageSettings};
use rust_cli_pg_store::PgDatabase;
use rust_cli_pg_store::{StoreAccessContext, StoreProject};
use rust_cli_runtime::{ContextConfig, Defaults, Limits, LocalKernel, Ports, Surface};
use rust_cli_ssh_connect::{SshConnector, SshProvisioner};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
pub enum Mode {
    Local,
    Remote,
    Ssh,
}
struct UnavailableServices;
#[async_trait]
impl Fetch for UnavailableServices {}

pub async fn database() -> PgDatabase {
    PgDatabase::connect_loopback_test(
        &RedactedString::new(
            std::env::var("ZCODE_TEST_POSTGRES_URL").expect("run test:rust-cli:pg"),
        ),
        4,
    )
    .await
    .unwrap()
}

pub struct Space {
    pub root: PathBuf,
    pub home: PathBuf,
    pub workspace: WorkspaceRef,
    pub observer: Arc<dyn Store>,
    /// 输入账本（PG 的会话库自己）；本机库没有。
    pub journal: Option<Arc<dyn rust_cli_kernel_api::input_journal::InputJournal>>,
    pub artifacts: Arc<dyn ArtifactStore>,
    pub environments: Arc<dyn EnvironmentRegistry>,
    pub clock: Arc<SystemClock>,
    pub ids: Arc<NodeIds>,
    pub pg: Option<PgDatabase>,
    pub user_id: String,
    client: Option<ExecClient>,
    sshd: Option<crate::sshd::Sshd>,
    _dir: tempfile::TempDir,
}
impl Space {
    pub async fn new(mode: Mode, pg: Option<PgDatabase>, label: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let mut root = base.join("workspace");
        let home = base.join("executor-home");
        let temp = base.join("temp");
        let node_home = base.join("legacy-store");
        for path in [&root, &home, &temp, &node_home] {
            std::fs::create_dir_all(path).unwrap();
        }
        let shell_env = vec![
            ("HOME".into(), home.to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("SHELL".into(), "/bin/bash".into()),
            ("TMPDIR".into(), temp.to_string_lossy().into_owned()),
        ];
        let local = Arc::new(
            LocalEnvironment::new(Some(home.clone())).with_shell(ShellConfig {
                scratch: temp.join("sessions"),
                ledger: temp.join("ledger"),
                bash: Some(PathBuf::from("/bin/bash")),
                bwrap: None,
                embedded: None,
                env: shell_env.clone().into(),
            }),
        );
        let (environments, client, sshd): (Arc<dyn EnvironmentRegistry>, _, _) = match mode {
            Mode::Local => (environments(local), None, None),
            Mode::Remote => {
                let connector = StdioConnector::new(ChildCommand {
                    program: PathBuf::from(env!("CARGO_BIN_EXE_zcode-rs")),
                    args: vec!["executor".into(), "--stdio".into(), "--standalone".into()],
                    env: shell_env,
                    cwd: Some(root.clone()),
                    kill_on_drop: false,
                });
                let client = ExecClient::connect(
                    Arc::new(connector),
                    ClientOptions::new("pg-mode-test", label),
                )
                .await
                .unwrap();
                let machine = Arc::new(RemoteEnvironment::new(client.clone())).machine();
                (
                    Arc::new(PrimaryRegistry::new(
                        machine,
                        ControllerServices::new(Arc::new(UnavailableServices)),
                    )),
                    Some(client),
                    None,
                )
            }
            Mode::Ssh => {
                let artifact = PathBuf::from(
                    std::env::var_os("ZCODE_TEST_SSH_ARTIFACT")
                        .expect("PG SSH acceptance requires ZCODE_TEST_SSH_ARTIFACT"),
                );
                assert!(artifact.is_file(), "SSH executor artifact is missing");
                let sshd = crate::sshd::Sshd::start();
                sshd.wait_ready().await;
                let (ok, _, error) = sshd.ssh("mkdir -p /tmp/zcode-pg-workspace");
                assert!(ok, "{error}");
                root = PathBuf::from("/tmp/zcode-pg-workspace");
                let provisioned =
                    SshProvisioner::new(sshd.target(), crate::sshd::wanted(), vec![artifact])
                        .provision()
                        .await
                        .unwrap();
                let client = ExecClient::connect(
                    Arc::new(SshConnector::new(sshd.target(), &provisioned).unwrap()),
                    ClientOptions::new(crate::sshd::VERSION, "pg-ssh-test"),
                )
                .await
                .unwrap();
                assert_eq!(client.welcome().unwrap().os, "linux");
                let machine = Arc::new(RemoteEnvironment::new(client.clone())).machine();
                (
                    Arc::new(PrimaryRegistry::new(
                        machine,
                        ControllerServices::new(Arc::new(UnavailableServices)),
                    )),
                    Some(client),
                    Some(sshd),
                )
            }
        };
        let workspace = WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new(root.to_string_lossy()),
        };
        let clock = Arc::new(SystemClock::utc());
        let ids = Arc::new(NodeIds::new(clock.clone()));
        let user_id = format!("{label}-{}", base.file_name().unwrap().to_string_lossy());
        let mut journal: Option<Arc<dyn rust_cli_kernel_api::input_journal::InputJournal>> = None;
        let (observer, artifacts): (Arc<dyn Store>, Arc<dyn ArtifactStore>) = if let Some(pg) = &pg
        {
            let access = Self::access(&user_id);
            let store = pg
                .bind(
                    access,
                    vec![StoreProject {
                        id: "project".into(),
                        workspace: workspace.clone(),
                    }],
                    clock.clone(),
                    ids.clone(),
                )
                .unwrap();
            let store = Arc::new(store);
            journal = Some(store.clone());
            (store, Arc::new(MemoryArtifacts::default()))
        } else {
            let paths = StoragePaths::resolve(&StorageSettings::default(), &node_home, &node_home);
            let store = Arc::new(NodeStore::new(paths, ids.clone()));
            (store.clone(), store)
        };
        Self {
            root,
            home,
            workspace,
            observer,
            journal,
            artifacts,
            environments,
            clock,
            ids,
            pg,
            user_id,
            client,
            sshd,
            _dir: dir,
        }
    }
    fn access(user_id: &str) -> StoreAccessContext {
        StoreAccessContext::verified(user_id, u64::MAX).unwrap()
    }

    pub fn host(&self, model: Arc<dyn ModelPort>) -> LocalKernel {
        let defaults = Defaults {
            selection: selection().into(),
            mode: PermissionMode::BypassPermissions,
            max_turns: Some(8),
            search_tools: false,
            task_tools: Some(false),
            limits: Limits::default(),
            additional_directories: Vec::new(),
        };
        if let Some(pg) = &self.pg {
            cloud::kernel(
                pg,
                CloudSessionConfig {
                    access: Self::access(&self.user_id),
                    projects: vec![StoreProject {
                        id: "project".into(),
                        workspace: self.workspace.clone(),
                    }],
                    defaults,
                    context: ContextConfig::new(Surface::Print),
                },
                CloudPorts {
                    model,
                    environments: self.environments.clone(),
                    artifacts: self.artifacts.clone(),
                    clock: self.clock.clone(),
                    ids: self.ids.clone(),
                    uuids: self.ids.clone(),
                },
            )
            .unwrap()
        } else {
            LocalKernel::with_context(
                Ports {
                    model,
                    environments: self.environments.clone(),
                    store: self.observer.clone(),
                    artifacts: self.artifacts.clone(),
                    clock: self.clock.clone(),
                    ids: self.ids.clone(),
                },
                defaults,
                ContextConfig::new(Surface::Print),
            )
        }
    }
    pub async fn close(&self) {
        if let Some(client) = &self.client {
            client.close().await;
        }
    }
    pub fn file_text(&self, name: &str) -> String {
        let path = self.root.join(name);
        if let Some(sshd) = &self.sshd {
            let (ok, text, error) =
                crate::sshd::docker(&["exec", &sshd.name, "cat", path.to_str().unwrap()]);
            assert!(ok, "{error}");
            text
        } else {
            std::fs::read_to_string(path).unwrap()
        }
    }
    pub fn file_exists(&self, name: &str) -> bool {
        let path = self.root.join(name);
        if let Some(sshd) = &self.sshd {
            crate::sshd::docker(&["exec", &sshd.name, "test", "-f", path.to_str().unwrap()]).0
        } else {
            path.exists()
        }
    }
    pub fn assert_no_executor_session_database(&self) {
        if let Some(sshd) = &self.sshd {
            let (ok, files, error) =
                crate::sshd::docker(&["exec", &sshd.name, "find", "/root", "-name", "db.sqlite"]);
            assert!(ok, "{error}");
            assert!(
                files.trim().is_empty(),
                "executor created a session database: {files}"
            );
            return;
        }
        fn visit(path: &std::path::Path) {
            for file in std::fs::read_dir(path).unwrap() {
                let file = file.unwrap();
                let path = file.path();
                if path.is_dir() {
                    visit(&path)
                } else {
                    assert_ne!(
                        file.file_name(),
                        "db.sqlite",
                        "executor created a session database at {path:?}"
                    );
                }
            }
        }
        visit(&self.home);
    }
}

pub fn id() -> SessionId {
    SessionId::new("same-session-id")
}
