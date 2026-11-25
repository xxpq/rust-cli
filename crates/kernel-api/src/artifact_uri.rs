//! Artifact 的位置 `zcode-artifact://<encodeURIComponent(会话)>/<encodeURIComponent(artifact)>`（Node `NodeToolArtifactStore` 的 uri 与
//! `parseArtifactUri`）：写进 file part、前端给出的上传附件的引用与上传的结果，前端也从输入里读回它
//! （docs/specs/rust-cli/session-lifecycle.md §10.40）。写法与读法只在这里，存储与前端给出的是同一个字符串。

use rust_cli_base::js::uri::{decode_uri_component, encode_uri_component};

use crate::ids::{ArtifactId, SessionId};
use crate::persisted_output::ARTIFACT_SCHEME;

/// 一个 Artifact 的 URI。
pub fn uri(session: &SessionId, id: &ArtifactId) -> String {
    format!(
        "{ARTIFACT_SCHEME}{}/{}",
        encode_uri_component(session.as_str()),
        encode_uri_component(id.as_str())
    )
}

/// URI 里的会话与 artifact id（Node 取 URL 的 host 与去掉开头 `/` 的 path，各自解码）。不是这个 scheme、某一段为空或解不出时为 None。
pub fn parse(uri: &str) -> Option<(SessionId, ArtifactId)> {
    let (session, id) = uri.strip_prefix(ARTIFACT_SCHEME)?.split_once('/')?;
    let (session, id) = (
        decode_uri_component(session)?,
        decode_uri_component(id.trim_start_matches('/'))?,
    );
    (!session.is_empty() && !id.is_empty()).then(|| (SessionId::new(session), ArtifactId::new(id)))
}

/// 文本里出现的每个 Artifact 引用（Node `inspectSessionDeletion` 的扫描：`zcode-artifact://<会话>/<artifact>`，两段都到 `"`、`\\`、空白为止，
/// artifact 另到 `/`、`?`、`#` 为止）：解不出的一段跳过（Node 在这里抛错）。永久删除按它找树外会话对附件的引用（02 §2.6）。
pub fn references(text: &str) -> Vec<(SessionId, String)> {
    let stops = |byte: u8| byte == b'"' || byte == b'\\' || byte.is_ascii_whitespace();
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(ARTIFACT_SCHEME) {
        rest = &rest[at + ARTIFACT_SCHEME.len()..];
        let session_end = rest
            .bytes()
            .position(|byte| byte == b'/' || stops(byte))
            .unwrap_or(rest.len());
        let (session, after) = rest.split_at(session_end);
        let Some(tail) = after.strip_prefix('/') else {
            continue;
        };
        let artifact_end = tail
            .bytes()
            .position(|byte| stops(byte) || matches!(byte, b'/' | b'?' | b'#'))
            .unwrap_or(tail.len());
        if session.is_empty() || artifact_end == 0 {
            continue;
        }
        if let (Some(session), Some(artifact)) = (
            decode_uri_component(session),
            decode_uri_component(&tail[..artifact_end]),
        ) {
            found.push((SessionId::new(session), artifact));
        }
    }
    found
}

#[cfg(test)]
#[path = "artifact_uri_tests.rs"]
mod tests;
