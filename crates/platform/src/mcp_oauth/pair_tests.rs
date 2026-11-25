//! 与 Node 的对拍（ext.md §3.12.4）：键的前缀、刷新锁的路径与 canonical pair 的派生，期望值由 Node 适配器的真代码算出
//! （`scripts/rust-cli/mcp-oauth-golden.mts` → `tests/fixtures/mcp-oauth-golden.json`）。另测发布的形状。

use std::path::Path;

use rust_cli_kernel_api::mcp_auth::{McpAuthTarget, McpOAuth};
use serde_json::{Value, json};

use super::*;
use crate::mcp_oauth::keys::Keys;

fn golden() -> Value {
    serde_json::from_str(include_str!("../../tests/fixtures/mcp-oauth-golden.json")).unwrap()
}

fn text(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}

#[test]
fn key_prefixes_and_refresh_locks_are_the_ones_node_computes() {
    let golden = golden();
    let credentials = Path::new(golden["credentials"].as_str().unwrap());
    for case in golden["prefixes"].as_array().unwrap() {
        let oauth = &case["oauth"];
        let target = McpAuthTarget {
            server: text(case, "server").unwrap(),
            url: text(case, "url").unwrap(),
            oauth: McpOAuth {
                client_id: text(oauth, "clientId"),
                scope: text(oauth, "scope"),
                redirect_path: text(oauth, "redirectPath"),
                ..McpOAuth::default()
            },
        };
        let keys = Keys::of(&target);
        assert_eq!(keys.prefix(), case["prefix"], "{case}");
        assert_eq!(
            keys.refresh_lock(credentials).to_string_lossy(),
            case["lock"].as_str().unwrap(),
            "{case}"
        );
    }
}

#[test]
fn pairs_are_derived_with_the_rules_node_uses() {
    for case in golden()["pairs"].as_array().unwrap() {
        let input = &case["input"];
        let stored = Stored {
            canonical: text(input, "canonicalRaw"),
            legacy_client: text(input, "legacyClientRaw"),
            legacy_tokens: text(input, "legacyTokensRaw"),
        };
        let derived = derive(&stored).map(|pair| {
            json!({
                "client": pair.client,
                "tokens": pair.tokens,
                "raw": pair.raw,
                "generation": pair.generation,
            })
        });
        assert_eq!(
            derived.unwrap_or(Value::Null),
            case["expected"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn a_published_pair_has_node_s_canonical_shape_and_both_mirrors() {
    let client = json!({"client_id": "dyn-1", "client_secret": "s"});
    let tokens: Tokens = serde_json::from_value(json!({
        "access_token": "at-1", "token_type": "bearer", "expires_in": 3600, "refresh_token": "rt-1", "scope": "a"
    }))
    .unwrap();
    let published = publish(Publish {
        client: client.as_object().unwrap(),
        tokens: &tokens,
        issuer: Some("https://auth.example.com"),
        published_by: "tx",
        obtained_at_ms: 1_700_000_000_000,
        generation: "0123456789abcdef0123456789abcdef".to_owned(),
    })
    .unwrap();
    let canonical: Value = serde_json::from_str(&published.canonical).unwrap();
    assert_eq!(
        canonical,
        json!({
            "client_information": client,
            "expires_at": 1_700_003_600_000_u64,
            "generation": "0123456789abcdef0123456789abcdef",
            "issuer": "https://auth.example.com",
            "obtained_at": 1_700_000_000_000_u64,
            "published_by": "tx",
            "tokens": {"access_token": "at-1", "token_type": "bearer", "expires_in": 3600, "scope": "a", "refresh_token": "rt-1"},
            "version": 2,
        })
    );
    // 键按字母序（同 Node 写出的顺序）
    assert!(published.canonical.starts_with(r#"{"client_information":"#));
    assert_eq!(
        serde_json::from_str::<Value>(&published.legacy_client).unwrap(),
        client
    );
    assert_eq!(
        serde_json::from_str::<Value>(&published.legacy_tokens).unwrap(),
        canonical["tokens"]
    );
    // 读回来：同一份 client 与令牌，有原文（失效时比它）
    let pair = derive(&Stored {
        canonical: Some(published.canonical.clone()),
        legacy_client: Some(published.legacy_client),
        legacy_tokens: Some(published.legacy_tokens),
    })
    .unwrap();
    assert_eq!(pair.tokens, Some(tokens));
    assert_eq!(pair.expires_at, Some(1_700_003_600_000.0));
    assert_eq!(pair.raw.as_deref(), Some(published.canonical.as_str()));
}
