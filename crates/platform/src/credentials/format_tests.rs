//! 值的格式与 Node 逐项相同（platform.md §13.1、§13.4）。

use super::*;
use crate::credentials::seal::{BASE64URL, seal_with_iv};
use base64::Engine;

/// Node 的 v1 缺省派生（platform linux、home /home/dev、用户 dev）加密 `sk-fixture-国`、IV 01..0c 的结果。
const NODE_V1: &str = "enc:v1:AQIDBAUGBwgJCgsM.ugbwcGz6yNOO7MTsaggOWA.TSZzRstDM_2VG1gJJ9g";
const FALLBACK: &str = "zcode-credential-fallback:linux:/home/dev:dev";

fn golden() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/credential-v2-golden.json"
    ))
    .unwrap()
}

fn os_material() -> KeyMaterial {
    let key: Key = BASE64URL
        .decode(golden()["osKey"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    KeyMaterial::new(key, Source::Os, legacy_keys(None, FALLBACK))
}

fn env_material(secret: &str) -> KeyMaterial {
    KeyMaterial::new(
        digest(secret),
        Source::Env,
        legacy_keys(Some(secret), FALLBACK),
    )
}

#[test]
fn the_sealed_part_matches_node_byte_for_byte() {
    let iv = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    let payload = seal_with_iv("sk-fixture-国", &digest(FALLBACK), iv).unwrap();
    assert_eq!(format!("{V1_PREFIX}{payload}"), NODE_V1);
    assert!(!payload.contains('='));
}

#[test]
fn node_v2_values_are_decrypted_from_both_sources() {
    let golden = golden();
    let os = os_material();
    let env = env_material(golden["envSecret"].as_str().unwrap());
    for value in golden["values"].as_array().unwrap() {
        let plain = value["plain"].as_str().unwrap();
        assert_eq!(os.decrypt(value["os"].as_str().unwrap()).unwrap(), plain);
        assert_eq!(env.decrypt(value["env"].as_str().unwrap()).unwrap(), plain);
        // 另一个来源的值不是“登录失效”，是存储错误
        assert_eq!(
            os.decrypt(value["env"].as_str().unwrap()),
            Err(CredentialError::Unavailable)
        );
    }
}

#[test]
fn the_managed_secret_is_not_trimmed_for_v2() {
    let golden = golden();
    let trimmed = env_material(golden["envSecret"].as_str().unwrap().trim());
    // 去空白之后是另一把密钥：指纹对不上
    assert_eq!(
        trimmed.decrypt(golden["values"][0]["env"].as_str().unwrap()),
        Err(CredentialError::Unavailable)
    );
}

#[test]
fn v1_tries_the_raw_secret_the_trimmed_secret_and_the_fallback() {
    let golden = golden();
    let legacy = &golden["legacy"];
    let env = env_material(golden["envSecret"].as_str().unwrap());
    assert_eq!(
        env.decrypt(legacy["raw"].as_str().unwrap()).unwrap(),
        "raw-国"
    );
    assert_eq!(
        env.decrypt(legacy["trimmed"].as_str().unwrap()).unwrap(),
        "trimmed"
    );
    assert_eq!(env.decrypt(NODE_V1).unwrap(), "sk-fixture-国");
    // 系统凭据库的来源只有缺省派生
    let os = os_material();
    assert_eq!(os.decrypt(NODE_V1).unwrap(), "sk-fixture-国");
    assert_eq!(
        os.decrypt(legacy["raw"].as_str().unwrap()),
        Err(CredentialError::Decrypt("密钥不匹配或密文已损坏"))
    );
    assert_eq!(legacy_keys(Some("  "), FALLBACK).len(), 2);
    assert_eq!(legacy_keys(None, FALLBACK).len(), 1);
}

#[test]
fn writes_are_v2_with_the_source_and_the_fingerprint() {
    let golden = golden();
    let os = os_material();
    let sealed = os.encrypt("token-国").unwrap();
    // 指纹与 Node 写的相同（同一把密钥）
    let node = golden["values"][0]["os"].as_str().unwrap();
    let prefix = &node[..node.len() - node.rsplit(':').next().unwrap().len()];
    assert!(sealed.starts_with(prefix), "{sealed} vs {prefix}");
    assert_eq!(os.decrypt(&sealed).unwrap(), "token-国");
    assert_eq!(os.decrypt(&os.encrypt("").unwrap()).unwrap(), "");
    assert!(
        env_material("s")
            .encrypt("x")
            .unwrap()
            .starts_with("enc:v2:env:")
    );
}

#[test]
fn malformed_values_are_decrypt_errors_and_plain_text_passes() {
    let os = os_material();
    let malformed = Err(CredentialError::Decrypt("密文格式非法"));
    assert_eq!(os.decrypt("plain"), Ok("plain".to_owned()));
    assert_eq!(os.decrypt(""), Ok(String::new()));
    assert_eq!(os.decrypt("enc:v1:a.b"), malformed);
    assert_eq!(os.decrypt("enc:v1:a=.b.c"), malformed);
    assert_eq!(
        os.decrypt("enc:v1:AAAA.AAAA.AAAA"),
        Err(CredentialError::Decrypt("IV 长度非法"))
    );
    assert_eq!(
        os.decrypt("enc:v1:AQIDBAUGBwgJCgsM.AAAA.AAAA"),
        Err(CredentialError::Decrypt("AuthTag 长度非法"))
    );
    assert_eq!(
        os.decrypt("enc:v9:x"),
        Err(CredentialError::Decrypt("未知的密文版本"))
    );
    for bad in [
        "enc:v2:os:abc",
        "enc:v2:os:ae216c2ef5247a3782c135ef",
        "enc:v2:cloud:ae216c2ef5247a3782c135ef:a.b.c",
        "enc:v2:os:AE216C2EF5247A3782C135EF:a.b.c",
        "enc:v2:os:ae216c2ef5247a3782c135ef:",
        "enc:v2:os:ae216c2ef5247a3782c135ef:a.b.c:d",
    ] {
        assert_eq!(os.decrypt(bad), malformed, "{bad}");
    }
    assert_eq!(
        os.decrypt("enc:v2:os:000000000000000000000000:a.b.c"),
        Err(CredentialError::Unavailable)
    );
    assert_eq!(
        CredentialError::Decrypt("密文格式非法").to_string(),
        "凭据解密失败：密文格式非法"
    );
}

/// v1 文件的写密钥（platform.md §13.2 第 4 条）：能解开旧值的候选密钥；都解不开时用给定的那个。
#[test]
fn v1_files_write_with_the_key_that_opens_their_values() {
    let golden = golden();
    let secret = golden["envSecret"].as_str().unwrap();
    let candidates = legacy_keys(Some(secret), FALLBACK);
    let trimmed = golden["legacy"]["trimmed"].as_str().unwrap();
    let keys = LegacyKeys::new(
        candidates.clone(),
        digest(secret),
        [trimmed, "plain"].into_iter(),
    );
    let sealed = FileKey::V1(keys).encrypt("new").unwrap();
    assert!(sealed.starts_with(V1_PREFIX));
    // 用去空白的 secret 写（旧值是它写的），不是原值
    let only_trimmed = LegacyKeys::new(vec![digest(secret.trim())], [0; 32], std::iter::empty());
    assert_eq!(FileKey::V1(only_trimmed).decrypt(&sealed).unwrap(), "new");
    let plain_only = LegacyKeys::new(candidates, digest(secret), ["plain"].into_iter());
    let sealed = FileKey::V1(plain_only).encrypt("new").unwrap();
    let raw_only = LegacyKeys::new(vec![digest(secret)], [0; 32], std::iter::empty());
    assert_eq!(FileKey::V1(raw_only).decrypt(&sealed).unwrap(), "new");
}

/// 与 Node 的 `createLegacyServerCredentialCipher` 对拍：同一份 v1 文件，Node 写的新值 rust 选的写密钥解得开。
#[test]
fn the_v1_write_key_matches_node() {
    let golden = golden();
    let case = &golden["legacyWrite"];
    let secret = golden["envSecret"].as_str().unwrap();
    let values = case["file"]
        .as_object()
        .unwrap()
        .values()
        .map(|value| value.as_str().unwrap());
    let ours = LegacyKeys::new(legacy_keys(Some(secret), FALLBACK), digest(secret), values);
    let written = LegacyKeys::new(vec![ours.write], [0; 32], std::iter::empty());
    assert_eq!(
        FileKey::V1(written)
            .decrypt(case["sealed"].as_str().unwrap())
            .unwrap(),
        case["plain"].as_str().unwrap()
    );
}
