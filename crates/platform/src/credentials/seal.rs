//! AES-256-GCM 的密文段 `<iv>.<tag>.<密文>`（无填充 base64url）：`enc:v1` 与 `enc:v2` 共用（Node `seal` / `open`，
//! docs/specs/rust-cli/platform.md §13.1）。

use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};

use super::CredentialError;

/// AES-256 密钥的字节数（系统凭据库里的随机密钥也是这么长）。
pub(crate) const KEY_BYTES: usize = 32;
/// AES-256 的密钥。
pub(crate) type Key = [u8; KEY_BYTES];
const TAG_BYTES: usize = 16;
/// 密文 `iv.tag.data` 的段数。
const PARTS: usize = 3;

/// Node `Buffer.from(raw, "base64url")` 接受有无填充两种写法；输出始终无填充。段里的字符另由 [`open`] 限定。
pub(crate) const BASE64URL: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// 加密强度的随机字节（Node `randomBytes`）：IV 与系统凭据库里的随机密钥。
#[expect(
    clippy::disallowed_methods,
    reason = "platform 是加密强度随机数的入口：凭据的 IV 与随机密钥（README §4 的随机类别，F58）"
)]
pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N], CredentialError> {
    let mut bytes = [0u8; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| CredentialError::Encrypt("random bytes"))?;
    Ok(bytes)
}

fn sealing_key(key: &Key) -> Result<LessSafeKey, CredentialError> {
    UnboundKey::new(&AES_256_GCM, key)
        .map(LessSafeKey::new)
        .map_err(|_| CredentialError::Encrypt("AES-256 key length"))
}

/// 用随机 IV 加密（Node `randomBytes(12)`）。
pub(crate) fn seal(value: &str, key: &Key) -> Result<String, CredentialError> {
    seal_with_iv(value, key, random_bytes()?)
}

/// 指定 IV 的加密：测试用它证明密文段与 Node 逐字节相同。
pub(crate) fn seal_with_iv(
    value: &str,
    key: &Key,
    iv: [u8; NONCE_LEN],
) -> Result<String, CredentialError> {
    let mut data = value.as_bytes().to_vec();
    let tag = sealing_key(key)?
        .seal_in_place_separate_tag(Nonce::assume_unique_for_key(iv), Aad::empty(), &mut data)
        .map_err(|_| CredentialError::Encrypt("AES-GCM seal"))?;
    Ok(format!(
        "{}.{}.{}",
        BASE64URL.encode(iv),
        BASE64URL.encode(tag.as_ref()),
        BASE64URL.encode(&data)
    ))
}

/// 段里只有 base64url 的字符（可以为空；Node 的 `/^[A-Za-z0-9_-]*$/`，没有 `=` 填充）。
fn is_base64url(part: &str) -> bool {
    part.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// 解开一个密文段。空的密文段是空明文（Node 加密空串的结果）。
pub(crate) fn open(payload: &str, key: &Key) -> Result<String, CredentialError> {
    let parts: Vec<&str> = payload.split('.').collect();
    if parts.len() != PARTS || !parts.iter().all(|part| is_base64url(part)) {
        return Err(CredentialError::Decrypt("密文格式非法"));
    }
    let decode = |raw: &str| BASE64URL.decode(raw).unwrap_or_default();
    let (iv, tag, cipher) = (decode(parts[0]), decode(parts[1]), decode(parts[2]));
    let iv: [u8; NONCE_LEN] = iv
        .try_into()
        .map_err(|_| CredentialError::Decrypt("IV 长度非法"))?;
    if tag.len() != TAG_BYTES {
        return Err(CredentialError::Decrypt("AuthTag 长度非法"));
    }
    let mut data = cipher;
    data.extend_from_slice(&tag);
    let plain = sealing_key(key)
        .map_err(|_| CredentialError::Decrypt("密钥不匹配或密文已损坏"))?
        .open_in_place(Nonce::assume_unique_for_key(iv), Aad::empty(), &mut data)
        .map_err(|_| CredentialError::Decrypt("密钥不匹配或密文已损坏"))?;
    Ok(String::from_utf8_lossy(plain).into_owned())
}
