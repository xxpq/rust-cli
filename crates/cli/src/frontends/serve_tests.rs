//! serve 的参数与服务凭据。

use super::*;

fn args(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

#[test]
fn serve_takes_its_options_with_defaults() {
    assert_eq!(parse(&args("-p hi")), None);
    let parsed = parse(&args("serve --listen 127.0.0.1:0 --token-env T"))
        .unwrap()
        .unwrap();
    assert_eq!(
        parsed,
        Parsed {
            listen: "127.0.0.1:0".to_owned(),
            token_env: "T".to_owned(),
            pg_url_env: None,
            pg_pool: DEFAULT_PG_POOL,
            grace: Duration::from_secs(DEFAULT_GRACE_SECS),
            data_dir: None,
        }
    );
    let parsed = parse(&args(
        "serve --listen=0.0.0.0:8080 --token-env T --pg-url-env PG --pg-pool 16 --grace 30 --data-dir /data",
    ))
    .unwrap()
    .unwrap();
    assert_eq!(parsed.pg_url_env.as_deref(), Some("PG"));
    assert_eq!(parsed.pg_pool, 16);
    assert_eq!(parsed.grace, Duration::from_secs(30));
    assert_eq!(parsed.data_dir.as_deref(), Some("/data"));
}

#[test]
fn serve_refuses_missing_and_unknown_options() {
    assert!(parse(&args("serve --token-env T")).unwrap().is_err());
    assert!(parse(&args("serve --listen 127.0.0.1:0")).unwrap().is_err());
    assert!(
        parse(&args(
            "serve --listen 127.0.0.1:0 --token-env T --pg-pool 0"
        ))
        .unwrap()
        .is_err()
    );
    assert!(
        parse(&args("serve --listen 127.0.0.1:0 --token-env T --nope"))
            .unwrap()
            .is_err()
    );
    assert!(parse(&args("serve --listen")).unwrap().is_err());
}

#[test]
fn only_the_service_token_is_admitted() {
    let token = RedactedString::new("s3cret".to_owned());
    let with = |value: &str| Upgrade {
        path: "/".to_owned(),
        headers: vec![("authorization".to_owned(), value.to_owned())],
    };
    assert_eq!(authorize(&with("Bearer s3cret"), &token), Ok(()));
    assert!(authorize(&with("Bearer s3cres"), &token).is_err());
    assert!(authorize(&with("Bearer s3cret2"), &token).is_err());
    let none = Upgrade {
        path: "/".to_owned(),
        headers: Vec::new(),
    };
    assert!(authorize(&none, &token).is_err());
}
