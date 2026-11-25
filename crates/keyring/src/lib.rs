//! 系统凭据库的唯一入口（docs/specs/rust-cli/platform.md §13.3、§13.8）：macOS 钥匙串、Windows 凭据管理器、
//! Linux Secret Service。
//!
//! 只存取一个条目（服务名 + 账号名 → 文本值），不认识凭据的格式与密钥规则（那些在 platform）。条目经 `keyring-core` 与
//! 各平台的 store 存取：与 Node 的 `@napi-rs/keyring` 用同一组 store，两边读写同一个条目（条目的名字、值的编码、
//! 搜索与新建的规则都由 store 定，这里不再自己算）。每次调用新建一个 store：Linux 上是一条新的 D-Bus 连接，与 Node
//! 每次调用相同，连接断了也不用重连。store 的调用都是阻塞的（macOS 可能等用户回答系统弹框；Linux 的 zbus 阻塞接口在
//! 它自己的运行时里跑），所以整个调用放进阻塞线程。

use keyring_core::api::CredentialStoreApi;
use keyring_core::{Entry, Error};

#[cfg(target_os = "macos")]
use apple_native_keyring_store::keychain::Store;
#[cfg(windows)]
use windows_native_keyring_store::Store;
#[cfg(target_os = "linux")]
use zbus_secret_service_keyring_store::Store;

/// 凭据库读不出、写不进（锁着、拒绝授权、没有服务、条目不唯一、值不是文本）。原因只用于日志，不含条目的值。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("system credential store: {0}")]
pub struct KeyringError(pub String);

/// 用户域的缺省钥匙串 / 凭据管理器 / Secret Service 里的这个条目（还不读写）。
fn entry(service: &str, account: &str) -> Result<Entry, Error> {
    Store::new()?.build(service, account, None)
}

/// 一个条目的值；没有这个条目时为 `None`。
pub async fn read(service: &str, account: &str) -> Result<Option<String>, KeyringError> {
    let (service, account) = (service.to_owned(), account.to_owned());
    blocking(move || match entry(&service, &account)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(Error::NoEntry) => Ok(None),
        Err(error) => Err(error),
    })
    .await
}

/// 写一个条目：已有就换值，没有就建。
pub async fn write(service: &str, account: &str, value: &str) -> Result<(), KeyringError> {
    let (service, account, value) = (service.to_owned(), account.to_owned(), value.to_owned());
    blocking(move || entry(&service, &account)?.set_password(&value)).await
}

/// 在阻塞线程里调用 store。线程里的工作结束前进程在关，也当作凭据库不可用。
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, KeyringError> {
    rust_cli_base::blocking(work)
        .await
        .map_err(|error| KeyringError(error.to_string()))?
        .map_err(|error| KeyringError(error.to_string()))
}
