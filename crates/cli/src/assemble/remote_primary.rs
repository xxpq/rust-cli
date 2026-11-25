//! 主 env 在远端执行器上（deployment.md §19.1；remote-exec.md §29.3）：端点经连接状态机连上，包成 env-remote 的
//! `MachineEnv`，放进只有主 env 的注册表。SSH（`ssh_executor`）与执行中转（`relay_executor`）只在端点上不同，这一段
//! 只写一份。

use std::sync::Arc;
use std::time::Duration;

use rust_cli_env_local::LocalEnvironment;
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, Reconnect};
use rust_cli_kernel_api::controller_services::ControllerServices;
use rust_cli_kernel_api::env_registry::{EnvironmentRegistry, PrimaryRegistry};
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_machines::{Endpoint, Machine, MachineError};

/// 远端执行器的重连：0.5 s 起翻倍、最多 60 s、次数不限，直到连上、被拒绝（连接器标成 `Rejected`）或用户断开（远端执行
/// F96，remote-exec.md §23.4；R2 原来是最多 15 s、12 次）。SSH 的主 env、机器清单里的 SSH 机器（R3）与执行中转的主 env
/// （F142）都用它。
pub(crate) const RECONNECT: Reconnect = Reconnect {
    attempts: None,
    delay: Duration::from_millis(500),
    max_delay: Duration::from_secs(60),
};

/// 连上 `endpoint`，把它作为会话的主 env；controller 的服务（取网页）仍是本机的 `controller`。连接的状态机随注册表活着。
pub(crate) async fn connect(
    endpoint: Arc<dyn Endpoint>,
    controller: Arc<LocalEnvironment>,
) -> Result<Arc<dyn EnvironmentRegistry>, MachineError> {
    let instance = format!("{:016x}", rust_cli_base::random::u64());
    let mut options = ClientOptions::new(crate::VERSION, instance);
    options.reconnect = RECONNECT;
    let machine = Arc::new(Machine::new(endpoint, options));
    let client = machine.connect().await?;
    // 主 env 的名字是身份（`primary`），不是端点的地址（deployment.md §23）。
    let remote = Arc::new(RemoteEnvironment::new(client));
    Ok(Arc::new(RemotePrimary {
        inner: PrimaryRegistry::new(remote.machine(), ControllerServices::new(controller)),
        _machine: machine,
    }))
}

/// 主 env 在远端执行器上的注册表加上它的连接状态机（状态机订阅连接状态，要和注册表活得一样久）。
struct RemotePrimary {
    inner: PrimaryRegistry,
    _machine: Arc<Machine>,
}

impl EnvironmentRegistry for RemotePrimary {
    fn primary_machine(&self) -> MachineEnv {
        self.inner.primary_machine()
    }

    fn controller(&self) -> ControllerServices {
        self.inner.controller()
    }
}
