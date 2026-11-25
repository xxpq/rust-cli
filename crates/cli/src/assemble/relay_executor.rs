//! 执行中转的组装根（docs/specs/rust-cli/remote-exec.md §29.3）：relay-connect 的连接器装成 machines 的 `Endpoint`，
//! 再经 [`remote_primary`] 作为会话的主 env 连上。
//!
//! 没有部署这一步：沙盒由宿主拉起，执行器在沙盒镜像里、由中转的网关拉起。所以部署器是 machines 的 `Ready`，连接器
//! 不读部署结果（§29.10 的 Q4、Q6）。

use std::sync::Arc;

use anyhow::{Context, Result};
use rust_cli_env_local::LocalEnvironment;
use rust_cli_exec_transport::{Connector, LinkError};
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_machines::{Endpoint, Provisioned, Provisioner, Ready};
use rust_cli_net::Egress;
use rust_cli_relay_connect::{Relay, RelayTarget};

use crate::assemble::deployment::RelaySource;
use crate::assemble::remote_primary;

struct RelayEndpoint {
    connector: Arc<Relay>,
    ready: Ready,
}

impl Endpoint for RelayEndpoint {
    fn describe(&self) -> String {
        self.connector.describe()
    }

    fn provisioner(&self) -> &dyn Provisioner {
        &self.ready
    }

    fn connector(&self, _provisioned: &Provisioned) -> Result<Arc<dyn Connector>, LinkError> {
        Ok(self.connector.clone())
    }
}

/// 会话的主 env 是执行中转后面那个沙盒里的执行器。服务票在解析部署时已经取好（配置文件的 `token_env` 指的环境变量，
/// 或 serve 握手的 `exec_token`，`deployment::Resolved`）。
pub(crate) async fn primary(
    relay: &RelaySource,
    controller: Arc<LocalEnvironment>,
    egress: Arc<Egress>,
) -> Result<Arc<dyn EnvironmentRegistry>> {
    let target = RelayTarget {
        url: relay.url.clone(),
        token: relay.token.clone(),
        user: relay.user.clone(),
        up_url: relay.up_url.clone(),
    };
    let endpoint = Arc::new(RelayEndpoint {
        connector: Arc::new(Relay::new(egress, target)),
        ready: Ready(Provisioned {
            program: String::new(),
            args: Vec::new(),
        }),
    });
    remote_primary::connect(endpoint, controller)
        .await
        .with_context(|| format!("connecting to the sandbox executor at {}", relay.url))
}
