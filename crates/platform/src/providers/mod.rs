//! Provider registry（docs/specs/rust-cli/platform.md §3）：内置 + 个人配置 + 账号 overlay → 模型目录与
//! 端点模板。
//!
//! 来源与 Node 独立 CLI 相同：内置配置是随包的一份（或 `ZCODE_BUILTIN_PROVIDER_CONFIG_FILE`），个人配置是
//! `~/.zcode/v2/provider_config.json`，账号 overlay 由 `credentials.json`、个人订阅与团队列表、权益算出（`account.rs`、
//! `personal/`、`team/`，工作线 F47、F49、F50）；团队 provider 的模型按可用的团队项目平铺（`catalog.rs`，platform.md
//! §9.3）。
//!
//! 热更新：没有后台轮询任务。每次取目录或端点时先比三个文件的元数据（`ChangeGate`，元数据不变时每分钟
//! 兜底读一次，与 Host 的轮询规则相同，docs/specs/rust-provider-config-poll.md），有变化才读；读到的内容
//! 签名没变就沿用上一份快照，变了才重新解析并把版本加一。个人订阅与团队查询同时进行，途中账号输入变了，这一轮
//! 丢弃：不记录元数据、不发布（platform.md §5.7、§9.7）。
//!
//! 并发：不持锁跨 await。每次读取先取一个递增序号，只有序号更大的结果能覆盖已发布的快照，迟到的旧读取
//! 不会盖掉新读取（与 net 的出口策略热替换同一规则）。
//!
//! 外部账号层（platform.md §12，App 的 v5 模式）：注入了它时账号输入只来自它的格子——不读 `credentials.json`、不查个人订阅
//! 与团队，团队不平铺；格子的版本变了也要重读。格子里的一份依据的内置配置（摘要）与这里读到的不同时不用它：已发布过就沿用已发布的
//! 那份，不发布「这份内置 + 别的内置上算的账号层」（22 §15.4，照 Node 的 basedOn 门）。

pub(crate) mod catalog;
pub(crate) mod resolve;
pub(crate) mod rules;
pub(crate) mod site;

use rust_cli_base::sync::LockUnpoisoned;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use std::collections::{BTreeMap, HashMap};

use futures_util::future::{join, join_all};
use rust_cli_base::hash::sha256_hex;
use rust_cli_base::include_lf;
use rust_cli_base::units;
use rust_cli_fs::{ChangeGate, FileStamp};
use rust_cli_kernel_api::external_accounts::ExternalAccounts;
use rust_cli_kernel_api::model::ModelConnection;
use rust_cli_kernel_api::provider::CatalogConnection;
use rust_cli_paths::StoragePath;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::account::{self, AccountView};
use crate::credentials::{CredentialStore, Credentials};
use crate::personal::PersonalPlans;
use crate::team::{TeamAccount, TeamPlan, TeamPlans};
use catalog::Snapshot;

/// 元数据不变时两次读取之间的最长间隔（Host `FULL_READ_EVERY`）。
const FULL_READ_EVERY: Duration = Duration::from_secs(60);
/// 单个配置文件的上限（旧 Rust CLI 的 registry 相同）。
const MAX_CONFIG_BYTES: usize = 8 * units::MIB;
/// 随包的内置 Provider 配置：复制自仓库的 `config/provider/zcode-builtin.json`（测试校验两者相同）。
pub(crate) const BUNDLED_BUILTIN: &str = include_lf!("../../assets/zcode-builtin.json");
/// 个人配置不存在时的内容（Node 同样按空配置处理）。
const EMPTY_PERSONAL: &[u8] = br#"{"schemaVersion":1,"config":{}}"#;

/// 已发布的一份解析结果。
#[derive(Debug, Default)]
pub(crate) struct Current {
    pub revision: u64,
    pub snapshot: Snapshot,
    signature: Vec<u8>,
}

struct Published {
    seq: u64,
    current: Arc<Current>,
}

/// 账号的联网查询：个人订阅（F50）与团队列表、权益（F47、F49）。
pub(crate) struct Plans {
    pub personal: Arc<PersonalPlans>,
    pub teams: Arc<TeamPlans>,
}

/// 个人 provider 配置从哪来：本机的文件（TUI、`-p`、App），或者一份只在内存里的配置（serve 每一轮由握手给出，
/// docs/specs/rust-cli/multi-tenant.md §6；不落盘，不随文件变化）。
enum Personal {
    File(StoragePath),
    Inline(Arc<[u8]>),
}

pub(crate) struct Registry {
    builtin: Option<StoragePath>,
    personal: Personal,
    credentials: CredentialStore,
    plans: Plans,
    gate: Mutex<ChangeGate>,
    published: Mutex<Published>,
    next_seq: AtomicU64,
    /// 外部账号层（App 的 v5 模式）：有它时账号输入只来自它。
    external: Option<ExternalAccounts>,
    /// 上一次读成的那一轮用的外部账号层版本（与 `gate` 记的文件元数据同时记）：格子换代了就要重读。
    read_layer: Mutex<Option<u64>>,
}

/// 读到的三份输入。
struct Inputs {
    builtin: Vec<u8>,
    personal: Vec<u8>,
    account: Value,
    /// 这一轮用的外部账号层版本（没有外部账号层或格子还空着时为 None）。
    layer: Option<u64>,
    /// 格子里的一份依据的不是这份内置配置（摘要不同）：`account` 按没有账号层算；已发布过时不用这一轮（22 §15.4）。
    mismatched: bool,
}

impl Registry {
    pub fn new(
        builtin: Option<StoragePath>,
        personal: StoragePath,
        credentials: CredentialStore,
        plans: Plans,
    ) -> Self {
        Self {
            builtin,
            personal: Personal::File(personal),
            credentials,
            plans,
            gate: Mutex::new(ChangeGate::new(FULL_READ_EVERY)),
            published: Mutex::new(Published {
                seq: 0,
                current: Arc::new(Current::default()),
            }),
            next_seq: AtomicU64::new(1),
            external: None,
            read_layer: Mutex::new(None),
        }
    }

    /// 个人配置换成内存里的一份（serve）：之后不读个人配置文件。
    #[must_use]
    pub fn with_inline_personal(mut self, config: Vec<u8>) -> Self {
        self.personal = Personal::Inline(config.into());
        self
    }

    /// 注入外部账号层（platform.md §12）。
    #[must_use]
    pub fn with_external(mut self, accounts: ExternalAccounts) -> Self {
        self.external = Some(accounts);
        self
    }

    /// 格子的现版本（没有外部账号层时 None）。
    fn layer_version(&self) -> Option<u64> {
        self.external
            .as_ref()
            .and_then(ExternalAccounts::current)
            .map(|facts| facts.version)
    }

    /// 当前快照；文件有变化时先重新读取。
    pub async fn current(&self) -> Arc<Current> {
        let mut stamps = Vec::new();
        if let Personal::File(personal) = &self.personal {
            stamps.push(FileStamp::of(personal.as_path()).await);
        }
        // 有外部账号层时不读凭据，也就不看它的元数据
        if self.external.is_none() {
            stamps.push(FileStamp::of(self.credentials.file().as_path()).await);
        }
        if let Some(builtin) = &self.builtin {
            stamps.push(FileStamp::of(builtin.as_path()).await);
        }
        let now = Instant::now();
        let fresh = self.published.lock_unpoisoned().current.revision > 0;
        let layer_moved = self.layer_version() != *self.read_layer.lock_unpoisoned();
        if fresh && !layer_moved && !self.gate.lock_unpoisoned().should_read(&stamps, now) {
            return self.published.lock_unpoisoned().current.clone();
        }
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let inputs = match self.read().await {
            Ok(inputs) => inputs,
            Err(error) => {
                // 读失败不记录元数据，下次照常重读；已有的快照继续生效。
                tracing::warn!(%error, "cannot read the provider configuration");
                return self.published.lock_unpoisoned().current.clone();
            }
        };
        // 修 bug（A3-DIGEST 的一致性场景查出）：以前读完就记下「已读到」的元数据与格子版本，再去解析（在阻塞线程上，有 await）。这段时间里
        // 并发的另一次取用看到「没变」，拿到旧快照：Host 推了账号层（ACK 之后）马上发送时，缺省模型的重选与回合的取端点就是这样两次取用，
        // 回合按旧组合取端点，刚授权的模型报不可用。现在这一轮的结果发布（或确认与已发布的相同）之后才记下；在这之前到的取用自己读，
        // 发布照旧按序号（迟到的旧读取不覆盖新的）。
        let layer = inputs.layer;
        let settle = |current: Arc<Current>| {
            self.gate.lock_unpoisoned().record(stamps, now);
            *self.read_layer.lock_unpoisoned() = layer;
            current
        };
        // 账号层依据的内置配置不是这一份（Host 换了 Active 而这里还读着旧的，或反过来，22 §15.5）：沿用已发布的那份整份，等文件或格子
        // 再变（记下两者，不变就不再读）。还没发布过时照常发布这一轮（按没有账号层算），个人的 provider 照常可用。
        if inputs.mismatched {
            let published = self.published.lock_unpoisoned().current.clone();
            if published.revision > 0 {
                return settle(published);
            }
        }
        let signature = signature(&inputs);
        let unchanged = self.published.lock_unpoisoned().current.clone();
        if unchanged.signature == signature {
            return settle(unchanged);
        }
        let snapshot = match rust_cli_base::blocking(move || parse(&inputs)).await {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(error)) => {
                tracing::warn!(%error, "invalid provider configuration");
                Snapshot::default()
            }
            Err(error) => {
                // 没有结果：不记下，下次照常重读
                tracing::warn!(%error, "resolving the provider configuration failed");
                return self.published.lock_unpoisoned().current.clone();
            }
        };
        let current = {
            let mut published = self.published.lock_unpoisoned();
            if seq > published.seq && published.current.signature != signature {
                published.seq = seq;
                published.current = Arc::new(Current {
                    revision: published.current.revision + 1,
                    snapshot,
                    signature,
                });
            }
            published.current.clone()
        };
        settle(current)
    }

    /// 已发布的快照，不比元数据、不重读（同步的模型上限查询用，修复线 F61）：配置的变化由下一次 [`Self::current`]
    /// （取目录或端点）发现，所以最多晚一步。还没读过时是空快照。
    pub fn published(&self) -> Arc<Current> {
        self.published.lock_unpoisoned().current.clone()
    }

    /// 内置配置的 `config`（登录时找个人 Coding Plan provider）。
    pub async fn builtin_config(&self) -> Result<Value, String> {
        let bytes = self.read_builtin().await?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("builtin provider config: {error}"))?;
        Ok(value["config"].clone())
    }

    async fn read_builtin(&self) -> Result<Vec<u8>, String> {
        Ok(match &self.builtin {
            None => BUNDLED_BUILTIN.as_bytes().to_vec(),
            Some(file) => rust_cli_fs::read_optional(file.as_path())
                .await
                .map_err(|error| format!("builtin provider config: {error}"))?
                .ok_or_else(|| {
                    format!(
                        "builtin provider config {} is missing",
                        file.as_path().display()
                    )
                })?,
        })
    }

    async fn read(&self) -> Result<Inputs, String> {
        let builtin = self.read_builtin().await?;
        let personal = match &self.personal {
            Personal::File(path) => rust_cli_fs::read_optional(path.as_path())
                .await
                .map_err(|error| format!("personal provider config: {error}"))?
                .unwrap_or_else(|| EMPTY_PERSONAL.to_vec()),
            Personal::Inline(config) => config.to_vec(),
        };
        if builtin.len() > MAX_CONFIG_BYTES || personal.len() > MAX_CONFIG_BYTES {
            return Err("provider config exceeds 8 MiB".to_owned());
        }
        if let Some(external) = &self.external {
            // 账号层以 Host 为准（platform.md §12.2）：不读凭据、不联网，团队不平铺。只用依据这份内置配置算的那一份（摘要相同）
            let facts = external.current();
            let digest = sha256_hex(&builtin);
            let mismatched = facts
                .as_ref()
                .is_some_and(|facts| facts.layer.builtin_digest != digest);
            if let Some(facts) = facts.as_ref().filter(|_| mismatched) {
                // 只在文件或格子变了之后的那一次读取里出现，不是消息流
                tracing::info!(
                    builtin = %digest,
                    layer = %facts.layer.builtin_digest,
                    "the account layer was computed from another builtin config"
                );
            }
            let layer = facts
                .as_ref()
                .filter(|_| !mismatched)
                .map(|facts| account::external_overlay(&facts.layer.grants))
                .unwrap_or_default();
            return Ok(Inputs {
                builtin,
                personal,
                account: json!({"providers": layer.providers, "states": layer.states}),
                layer: facts.map(|facts| facts.version),
                mismatched,
            });
        }
        let builtin_value: Value = serde_json::from_slice(&builtin).unwrap_or(Value::Null);
        let builtin_config = &builtin_value["config"];
        let credentials = self.account_credentials().await;
        let accounts = account::team_accounts(builtin_config, credentials.as_ref());
        let individuals = account::personal_accounts(builtin_config, credentials.as_ref());
        // 个人订阅与团队、两个 family 都同时查（团队的 pricing、customerInfo 与订阅详情也是同时发的，platform.md
        // §5.7、§9.2）：第一次取目录的等待是各条查询的最大值，不是总和。
        let (rounds, checks) = join(
            join_all(
                accounts
                    .iter()
                    .map(|account| self.plans.teams.refresh(account)),
            ),
            join_all(
                individuals
                    .iter()
                    .map(|account| self.plans.personal.refresh(account)),
            ),
        )
        .await;
        if rounds.iter().any(|round| round.networked) || checks.iter().any(|check| check.networked)
        {
            // App `accountProviderConnectionResolver`：查询可能跨越换账号，旧身份的结果不能发布给新身份。
            let after = self.account_credentials().await;
            if account::team_accounts(builtin_config, after.as_ref()) != accounts
                || account::personal_accounts(builtin_config, after.as_ref()) != individuals
            {
                return Err("the account changed during the plan queries".to_owned());
            }
        }
        let teams: HashMap<String, Vec<TeamPlan>> = accounts
            .iter()
            .zip(rounds)
            .map(|(account, round)| {
                let plans = self.plans.teams.commit(account, round);
                (account.provider.clone(), plans)
            })
            .collect();
        let entitled: HashMap<String, bool> = individuals
            .iter()
            .zip(checks)
            .map(|(account, check)| {
                let entitled = self.plans.personal.commit(account, check);
                (account.provider.clone(), entitled)
            })
            .collect();
        let view = AccountView {
            credentials: credentials.as_ref(),
            personal: &entitled,
            teams: &teams,
        };
        let layer = account::overlay(builtin_config, &view);
        let connections = connections(&accounts, &teams);
        let account = json!({"providers": layer.providers, "connections": connections});
        Ok(Inputs {
            builtin,
            personal,
            account,
            layer: None,
            mismatched: false,
        })
    }
}

impl Registry {
    /// 凭据（读不到或损坏时为空：账号 provider 一律不授权，fail-closed，不影响其他 provider）。
    async fn account_credentials(&self) -> Option<Credentials> {
        match self.credentials.read().await {
            Ok(credentials) => Some(credentials),
            Err(error) => {
                tracing::warn!(%error, "account providers are unavailable");
                None
            }
        }
    }

    /// 团队项目已过期 / 未分配的提示（platform.md §9.2）。给了 `provider` 时只看它（不是团队 provider 时没有），
    /// 给了 `team` 时只看这个团队项目；都没给时按 provider 与列表的顺序取第一个。
    pub async fn team_notice(
        &self,
        provider: Option<&str>,
        team: Option<&ModelConnection>,
    ) -> Option<String> {
        // 有外部账号层时团队的状态归 Host（它的界面给提示），这里不读凭据
        if self.external.is_some() {
            return None;
        }
        let builtin = self.builtin_config().await.ok()?;
        let credentials = self.account_credentials().await;
        account::team_accounts(&builtin, credentials.as_ref())
            .iter()
            .filter(|account: &&TeamAccount| provider.is_none_or(|p| p == account.provider))
            .find_map(|account| {
                let reason = self.plans.teams.notice(account, team)?;
                crate::team::notice_text(reason, &self.plans.teams.manage_url(account.family))
            })
    }
}

/// 每个团队 provider 可用的团队项目（目录项的 `connection`，platform.md §9.3）。有序：它进快照的签名。
fn connections(
    accounts: &[TeamAccount],
    teams: &HashMap<String, Vec<TeamPlan>>,
) -> BTreeMap<String, Vec<CatalogConnection>> {
    accounts
        .iter()
        .filter_map(|account| {
            let plans = teams.get(&account.provider)?;
            let entries = plans
                .iter()
                .map(|plan| CatalogConnection {
                    id: plan.id.clone(),
                    vendor: account.family.display_name().to_owned(),
                    name: plan.name.clone(),
                })
                .collect();
            Some((account.provider.clone(), entries))
        })
        .collect()
}

fn signature(inputs: &Inputs) -> Vec<u8> {
    let mut hash = Sha256::new();
    hash.update(&inputs.builtin);
    hash.update([0]);
    hash.update(&inputs.personal);
    hash.update([0]);
    hash.update(inputs.account.to_string().as_bytes());
    hash.finalize().to_vec()
}

fn parse(inputs: &Inputs) -> Result<Snapshot, String> {
    let builtin: Value = serde_json::from_slice(&inputs.builtin)
        .map_err(|error| format!("builtin provider config: {error}"))?;
    let personal: Value = serde_json::from_slice(&inputs.personal)
        .map_err(|error| format!("personal provider config: {error}"))?;
    if builtin["schemaVersion"] != 1 || personal["schemaVersion"] != 1 {
        return Err("unsupported provider config schema".to_owned());
    }
    let object = |key: &str| inputs.account[key].as_object().cloned().unwrap_or_default();
    // 只有外部账号层给 `states`（当前连接，platform.md §12.2）；自己算的 overlay 不写它（F49）
    let layer = resolve::AccountLayer {
        providers: object("providers"),
        states: object("states"),
    };
    let connections: HashMap<String, Vec<CatalogConnection>> =
        serde_json::from_value(inputs.account["connections"].clone()).unwrap_or_default();
    let resolved = resolve::resolve(&builtin["config"], &personal["config"], Some(&layer));
    Ok(catalog::snapshot(&resolved, &connections))
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
