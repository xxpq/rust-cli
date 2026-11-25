//! 发现的地址与规则（TS SDK 的写法，ext.md §3.12.6 第 1 步）。

use serde_json::json;

use super::*;

#[test]
fn authorization_server_metadata_is_looked_up_where_the_ts_sdk_looks() {
    let urls = |base: &str| -> Vec<String> {
        metadata_urls(&Url::parse(base).unwrap())
            .iter()
            .map(Url::to_string)
            .collect()
    };
    assert_eq!(
        urls("https://auth.example.com"),
        [
            "https://auth.example.com/.well-known/oauth-authorization-server",
            "https://auth.example.com/.well-known/openid-configuration",
        ]
    );
    assert_eq!(
        urls("https://auth.example.com/tenant/one/"),
        [
            "https://auth.example.com/.well-known/oauth-authorization-server/tenant/one",
            "https://auth.example.com/.well-known/openid-configuration/tenant/one",
            "https://auth.example.com/tenant/one/.well-known/openid-configuration",
        ]
    );
    assert_eq!(
        well_known(PROTECTED_RESOURCE, "/mcp/"),
        "/.well-known/oauth-protected-resource/mcp"
    );
    assert_eq!(
        well_known(PROTECTED_RESOURCE, "/"),
        "/.well-known/oauth-protected-resource"
    );
}

#[test]
fn the_resource_must_be_on_the_server_s_origin_and_a_prefix_of_its_path() {
    let with = |resource: &str| Discovery {
        authorization_server_url: "https://auth.example.com".to_owned(),
        resource_metadata_url: None,
        resource_metadata: Some(json!({"resource": resource})),
        authorization_server_metadata: None,
    };
    assert_eq!(
        with("https://mcp.example.com/mcp").resource("https://mcp.example.com/mcp#x"),
        Ok(Some("https://mcp.example.com/mcp".to_owned()))
    );
    assert_eq!(
        with("https://mcp.example.com/").resource("https://mcp.example.com/mcp"),
        Ok(Some("https://mcp.example.com/".to_owned()))
    );
    assert!(
        with("https://mcp.example.com/other")
            .resource("https://mcp.example.com/mcp")
            .is_err()
    );
    assert!(
        with("https://evil.example.com/mcp")
            .resource("https://mcp.example.com/mcp")
            .is_err()
    );
    let without = Discovery {
        resource_metadata: None,
        ..with("https://mcp.example.com/mcp")
    };
    assert_eq!(without.resource("https://mcp.example.com/mcp"), Ok(None));
}

#[test]
fn a_cached_discovery_is_used_for_a_day_and_only_for_the_same_issuer() {
    let state = json!({
        "authorizationServerUrl": "https://auth.example.com/",
        "authorizationServerMetadata": {"issuer": "https://auth.example.com/"}
    })
    .to_string();
    let at = "1700000000000";
    let now = 1_700_000_000_000;
    assert!(
        cached(
            Some(&state),
            Some(at),
            now + super::super::DISCOVERY_TTL_MS - 1,
            None
        )
        .is_some()
    );
    assert!(
        cached(
            Some(&state),
            Some(at),
            now + super::super::DISCOVERY_TTL_MS,
            None
        )
        .is_none()
    );
    // 末尾的 `/` 不算不同
    assert!(
        cached(
            Some(&state),
            Some(at),
            now,
            Some("https://auth.example.com")
        )
        .is_some()
    );
    assert!(
        cached(
            Some(&state),
            Some(at),
            now,
            Some("https://other.example.com")
        )
        .is_none()
    );
    // 旧版本没写时刻：现取
    assert!(cached(Some(&state), None, now, None).is_none());
    assert!(cached(Some("{not json"), Some(at), now, None).is_none());
}
