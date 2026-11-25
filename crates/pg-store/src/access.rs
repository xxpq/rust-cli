//! 会话库的可信身份与项目绑定（pg-store.md §2）：只有云端的会话库用，所以在本 crate，不在内核的契约里。

use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::store::StoreError;
use serde::{Deserialize, Serialize};

/// 只由已完成账号鉴权的组装根构造；不能从用户 JSON 反序列化此类型。
#[derive(Debug, Clone)]
pub struct StoreAccessContext {
    tenant_key: String,
    user_id: String,
    expires_at_ms: u64,
}

impl StoreAccessContext {
    pub fn verified(user_id: &str, expires_at_ms: u64) -> Result<Self, StoreError> {
        if user_id.trim().is_empty() || expires_at_ms == 0 {
            return Err(StoreError::Forbidden {
                operation: "invalid user identity".into(),
            });
        }
        Ok(Self {
            // 旧键是组织/个人信息的十六进制摘要；前缀避免把旧共享数据误认成某个用户的数据。
            tenant_key: format!("user:{user_id}"),
            user_id: user_id.into(),
            expires_at_ms,
        })
    }

    pub fn tenant_key(&self) -> &str {
        &self.tenant_key
    }
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    pub fn check(&self, now_ms: u64) -> Result<(), StoreError> {
        if now_ms >= self.expires_at_ms {
            return Err(StoreError::Forbidden {
                operation: "expired store authorization".into(),
            });
        }
        Ok(())
    }
}

/// 控制面提供的稳定项目 ID，目录只作为执行信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreProject {
    pub id: String,
    pub workspace: WorkspaceRef,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_identity_is_stable_distinct_and_expires() {
        let first = StoreAccessContext::verified("alice", 1_000).unwrap();
        let renewed = StoreAccessContext::verified("alice", 2_000).unwrap();
        let other = StoreAccessContext::verified("bob", 1_000).unwrap();
        assert_eq!(first.user_id(), "alice");
        assert_eq!(first.tenant_key(), "user:alice");
        assert_eq!(first.tenant_key(), renewed.tenant_key());
        assert_ne!(first.tenant_key(), other.tenant_key());
        // 旧隔离键只有十六进制字符，新命名空间不能误认旧组织里的数据。
        let old_hash_as_user = StoreAccessContext::verified(&"a".repeat(64), 1_000).unwrap();
        assert_ne!(old_hash_as_user.tenant_key(), "a".repeat(64));
        assert!(first.check(999).is_ok());
        assert!(first.check(1_000).is_err());
    }

    #[test]
    fn empty_user_or_expiry_is_rejected() {
        for user in ["", " ", "\t\n"] {
            assert!(StoreAccessContext::verified(user, 1_000).is_err());
        }
        assert!(StoreAccessContext::verified("alice", 0).is_err());
    }
}
