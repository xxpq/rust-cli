//! Host↔CLI 链路上的 topic 名（01 §17.1，修订 1；`packages/protocol/src/topics.ts` 的 `CLI_AGENT_TOPIC`、`CLI_PLUGINS_TOPIC`、
//! `cliWorkspaceTopic`、`parseCliWorkspaceTopic`）：进程级是 `agent` 与用户级的插件管理 `catalog/plugins`；工作区级的 topic 带工作区段
//! `ws/<encodeURIComponent(key)>/<name>`。

use std::path::Path;

use rust_cli_base::js::uri::{decode_uri_component, encode_uri_component};

/// 进程级的 agent topic。
pub const AGENT_TOPIC: &str = "agent";
/// 用户级的插件管理（protocol-v5.md §72；24-plugins.md §2）：插件包、市场、写操作是用户级的库存，没有工作区段。
pub const PLUGINS_TOPIC: &str = "catalog/plugins";
/// 工作区级 topic 名里的插件管理（`ws/<key>/catalog/plugins`）：这个工作区的一层与生效的启用。
pub const WORKSPACE_PLUGINS: &str = "catalog/plugins";
/// 工作区级 topic 名里的 agent（`ws/<key>/agent`）。
pub const WORKSPACE_AGENT: &str = "agent";
/// 工作区级 topic 名里的会话列表（`ws/<key>/sessions`，protocol-v5.md §14）。
pub const WORKSPACE_SESSIONS: &str = "sessions";
/// 工作区级 topic 名里待回答的请求安全校验的索引（`ws/<key>/verifications`，protocol-v5.md §31；`CLI_VERIFICATIONS_TOPIC`）。
pub const WORKSPACE_VERIFICATIONS: &str = "verifications";
/// 工作区级 topic 名里一个会话的前缀（`ws/<key>/session/<sid>`，protocol-v5.md §15.2）。
const WORKSPACE_SESSION_PREFIX: &str = "session/";
const WORKSPACE_PREFIX: &str = "ws/";

/// 本端提供的 topic。`key` 是解码后的工作区（本地工作区的绝对路径，F145 §4.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Topic {
    Agent,
    /// 用户级的插件管理（§72）。
    Plugins,
    /// 一个工作区的就绪。
    WorkspaceAgent {
        key: String,
    },
    /// 一个工作区的会话列表（Host 的内部 feed，03 §4.1）。
    WorkspaceSessions {
        key: String,
    },
    /// 一个会话（protocol-v5.md §15.2）：`sid` 不编码（会话 id 里没有 `/`）。
    WorkspaceSession {
        key: String,
        sid: String,
    },
    /// 一个工作区里待回答的请求安全校验（protocol-v5.md §35.2）：订它不组装工作区。
    WorkspaceVerifications {
        key: String,
    },
    /// 一个工作区的插件管理（§72）：订它不组装工作区。
    WorkspacePlugins {
        key: String,
    },
}

/// 订阅一个 topic 不成：本端不提供（`precondition.notFound`）或工作区段不合法（`precondition.invalid`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicError {
    NotFound,
    Invalid(String),
}

pub fn parse(topic: &str) -> Result<Topic, TopicError> {
    if topic == AGENT_TOPIC {
        return Ok(Topic::Agent);
    }
    if topic == PLUGINS_TOPIC {
        return Ok(Topic::Plugins);
    }
    let Some(rest) = topic.strip_prefix(WORKSPACE_PREFIX) else {
        return Err(TopicError::NotFound);
    };
    let Some((segment, name)) = rest.split_once('/') else {
        return Err(TopicError::NotFound);
    };
    let session = name
        .strip_prefix(WORKSPACE_SESSION_PREFIX)
        .filter(|sid| !sid.is_empty() && !sid.contains('/'));
    let named = [
        WORKSPACE_AGENT,
        WORKSPACE_SESSIONS,
        WORKSPACE_VERIFICATIONS,
        WORKSPACE_PLUGINS,
    ];
    if segment.is_empty() || (session.is_none() && !named.contains(&name)) {
        return Err(TopicError::NotFound);
    }
    let key = decode_uri_component(segment).ok_or_else(|| {
        TopicError::Invalid(format!(
            "the workspace segment {segment:?} is not percent-encoded UTF-8"
        ))
    })?;
    local_workspace(&key).map_err(TopicError::Invalid)?;
    Ok(match session {
        Some(sid) => Topic::WorkspaceSession {
            key,
            sid: sid.to_owned(),
        },
        None if name == WORKSPACE_AGENT => Topic::WorkspaceAgent { key },
        None if name == WORKSPACE_VERIFICATIONS => Topic::WorkspaceVerifications { key },
        None if name == WORKSPACE_PLUGINS => Topic::WorkspacePlugins { key },
        None => Topic::WorkspaceSessions { key },
    })
}

/// 一个会话的 topic 名（`ws/<key>/session/<sid>`）。
pub fn session_topic(key: &str, sid: &str) -> String {
    workspace_topic(key, &format!("{WORKSPACE_SESSION_PREFIX}{sid}"))
}

/// 共享进程只服务本地工作区，键就是工作区的绝对路径（shared-workspaces.md §4.1）。topic 段与查询的 `target.workspaceKey`
/// 同一条规则。
pub fn local_workspace(key: &str) -> Result<(), String> {
    if Path::new(key).is_absolute() {
        Ok(())
    } else {
        Err(format!(
            "the workspace {key:?} is not an absolute local path"
        ))
    }
}

/// 工作区级 topic 的名字（编码与 JS 的 `encodeURIComponent` 相同）。
pub fn workspace_topic(key: &str, name: &str) -> String {
    format!("{WORKSPACE_PREFIX}{}/{name}", encode_uri_component(key))
}

#[cfg(test)]
#[path = "topics_tests.rs"]
mod tests;
