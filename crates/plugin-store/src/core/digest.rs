//! 包的摘要与包根的标记（plugin-store.md §6.2）。
//!
//! 摘要是 sha256：按相对路径排序，每一项依次喂「种类 + 相对路径（`/` 分隔）+ \0 + 内容长度（8 字节小端）+ 内容」（目录没有
//! 内容，链接喂目标的文字）。标记文件自己不算。为什么要标记：云上要按摘要校验包在不在、对不对（PLUGIN-CLOUD S3），以后再补
//! 就要重写全部已装的包（A16）。只有 rust 建的目录有标记，孤儿清理靠它分辨（§6.3）。

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// 包根的标记文件名。
pub const PACKAGE_MARKER: &str = ".zcode-package.json";
/// 标记文件的格式版本。
const MARKER_VERSION: u64 = 1;
/// 摘要写进标记时的前缀。
const DIGEST_PREFIX: &str = "sha256:";
/// 目录名里用摘要的前几位 hex。
pub const DIR_DIGEST_LEN: usize = 12;

/// 参与摘要的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item<'a> {
    Dir,
    File(&'a [u8]),
    Link(&'a str),
}

/// 一组 `(相对路径, 项)` 的摘要（hex）。调用方给的顺序不要紧，这里排序。
pub fn digest(items: &[(String, Item<'_>)]) -> String {
    let mut sorted: Vec<&(String, Item<'_>)> = items
        .iter()
        .filter(|(path, _)| path != PACKAGE_MARKER)
        .collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (path, item) in sorted {
        let (kind, content): (&[u8], &[u8]) = match item {
            Item::Dir => (b"d", b""),
            Item::File(bytes) => (b"f", bytes),
            Item::Link(target) => (b"l", target.as_bytes()),
        };
        hasher.update(kind);
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update((content.len() as u64).to_le_bytes());
        hasher.update(content);
    }
    rust_cli_base::hash::hex(&hasher.finalize())
}

/// 标记文件的文字。
pub fn marker(plugin_id: &str, digest: &str, source: Option<&Value>) -> String {
    super::json_text(&json!({
        "version": MARKER_VERSION,
        "pluginId": plugin_id,
        "digest": format!("{DIGEST_PREFIX}{digest}"),
        "source": source.cloned().unwrap_or(Value::Null),
    }))
}

/// 安装目录名：`<净化的版本>-<摘要前 12 位>`。
pub fn dir_name(version: &str, digest: &str) -> String {
    let short = &digest[..DIR_DIGEST_LEN.min(digest.len())];
    format!("{}-{short}", rust_cli_base::plugin::sanitize_id(version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_ignore_order_and_the_marker_but_see_content_and_kind() {
        let a = digest(&[("b".into(), Item::File(b"1")), ("a".into(), Item::Dir)]);
        let b = digest(&[
            ("a".into(), Item::Dir),
            ("b".into(), Item::File(b"1")),
            (PACKAGE_MARKER.into(), Item::File(b"anything")),
        ]);
        assert_eq!(a, b);
        assert_ne!(
            a,
            digest(&[("a".into(), Item::Dir), ("b".into(), Item::File(b"2"))])
        );
        assert_ne!(
            a,
            digest(&[("a".into(), Item::Dir), ("b".into(), Item::Link("1"))])
        );
        assert_eq!(a.len(), 64);
        assert_eq!(dir_name("1.0/beta", &a), format!("1.0-beta-{}", &a[..12]));
    }

    #[test]
    fn markers_are_node_style_json() {
        let text = marker("kit@mkt", "ab", Some(&json!("./kit")));
        assert!(text.ends_with("}\n"));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value,
            json!({"version": 1, "pluginId": "kit@mkt", "digest": "sha256:ab", "source": "./kit"})
        );
    }
}
