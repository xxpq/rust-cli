//! 凭据值交给别的进程加解密（docs/specs/rust-cli/platform.md §13.13）：App 里系统凭据库由 Main 独占，rust 的凭据库不自己取密钥，
//! 每个值的加密与解密经这个端口（接 App 的入口经 Host 实现它，注入 platform）。主密钥不出 Main。

use async_trait::async_trait;
use rust_cli_base::secret::RedactedString;

/// 一次加解密没做成的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CipherFailure {
    /// 值解不开（格式不对、校验不过）：坏值当作没有。
    Decrypt,
    /// 加解密方不可用（它的凭据库不可用、没提供这个服务、链路断了）：存储错误，不是登录失效。文字给用户看。
    Unavailable(String),
}

/// 每个值一次请求；等待在调用方的任务里，丢掉即放弃。
#[async_trait]
pub trait CredentialCipher: Send + Sync {
    /// 明文 → 存进凭据文件的值。
    async fn encrypt(&self, plain: &RedactedString) -> Result<String, CipherFailure>;
    /// 凭据文件里的值 → 明文。
    async fn decrypt(&self, sealed: &str) -> Result<RedactedString, CipherFailure>;
}
