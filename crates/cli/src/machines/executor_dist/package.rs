//! npm 平台包的纯计算部分（docs/specs/rust-cli/remote-exec.md §26.3、§26.4）：包名、`dist.integrity` 的校验、从 tarball
//! 解出执行器并核对 `build.json` 与产物的头（ELF 或 Mach-O，F107 §28.4）。不做 IO。

use std::io::Read;

use base64::Engine;
use rust_cli_base::units;
use rust_cli_ssh_connect::Platform;
use serde_json::Value;
use sha2::{Digest, Sha512};

/// 平台包名的前缀（ZCode-rs 的发布脚本 `scripts/rust-cli/npm/package.mjs`：`@x-studio/zcode-<id>`）。
const PACKAGE_PREFIX: &str = "@x-studio/zcode-";
/// 包里的执行器与构建记录（发布脚本放的位置；npm 的 tarball 里每项都在 `package/` 下）。
const BINARY_ENTRY: &str = "package/bin/zcode-rs";
const BUILD_ENTRY: &str = "package/build.json";
/// 包里一项最多读这么多（执行器几十 MB；防止坏的 tar 头让内存失控）。
const MAX_ENTRY: u64 = 512 * units::MIB as u64;

/// 远端平台对应的平台包（§26.3、§28.2；id 由 ssh-connect 的 `Platform` 给）。
pub(super) fn package_name(platform: Platform) -> String {
    format!("{PACKAGE_PREFIX}{}", platform.package_id())
}

/// `dist.integrity`（SRI，空白分隔的几项）里有一项 `sha512-<base64>` 与 `bytes` 的 sha512 相同。没有 sha512 的一项算不对。
pub(super) fn integrity_matches(bytes: &[u8], integrity: &str) -> bool {
    let digest = Sha512::digest(bytes);
    integrity
        .split_whitespace()
        .filter_map(|item| item.strip_prefix("sha512-"))
        .map(|encoded| encoded.split('?').next().unwrap_or(encoded))
        .filter_map(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
        })
        .any(|expected| expected.as_slice() == digest.as_slice())
}

/// 能放进路径的版本号（F103，remote-exec.md §27.3：注册表的 latest 进本机缓存与远端的路径）：以数字开头、只含
/// `[0-9A-Za-z.+-]`、不含 `..`。
pub(super) fn plain_version(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'))
        && !text.contains("..")
}

/// 从 tarball（gzip + tar）解出执行器，核对 `build.json`（版本、target、sha256）与产物的头。错误是一句简短的原因。
pub(super) fn extract(
    tarball: &[u8],
    version: &str,
    platform: Platform,
) -> Result<Vec<u8>, String> {
    let found = entries(flate2::read::GzDecoder::new(tarball))
        .map_err(|error| format!("cannot read the package: {error}"))?;
    let binary = found.binary.ok_or("the package has no bin/zcode-rs")?;
    let build = found.build.ok_or("the package has no build.json")?;
    let build: Value = serde_json::from_slice(&build)
        .map_err(|error| format!("build.json is not JSON: {error}"))?;
    let field = |name: &str| build.get(name).and_then(Value::as_str).unwrap_or("");
    if field("version") != version {
        return Err(format!(
            "build.json's version is {:?}, expected {version}",
            field("version")
        ));
    }
    let target = platform.triple();
    if field("target") != target {
        return Err(format!(
            "build.json's target is {:?}, expected {target}",
            field("target")
        ));
    }
    if !field("sha256").eq_ignore_ascii_case(&rust_cli_base::hash::sha256_hex(&binary)) {
        return Err("build.json's sha256 does not match bin/zcode-rs".to_owned());
    }
    if !platform.runs_image(&binary) {
        return Err(format!("bin/zcode-rs is not a {target} executable"));
    }
    Ok(binary)
}

/// 包里要的两项。
#[derive(Default)]
struct Entries {
    binary: Option<Vec<u8>>,
    build: Option<Vec<u8>>,
}

/// 读 tar：取执行器与 `build.json` 两项（普通文件；路径由库按 pax 的 `path`、GNU 的长名、ustar 的 `prefix/name` 给出），
/// 读进内存。不用库的 `unpack`：写进缓存的做法（0700 / 0755、临时文件再改名）在上一层，与 tar 头里的权限位无关。
fn entries(reader: impl Read) -> std::io::Result<Entries> {
    let mut found = Entries::default();
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let slot = match name.as_str() {
            BINARY_ENTRY => &mut found.binary,
            BUILD_ENTRY => &mut found.build,
            _ => continue,
        };
        let size = entry.size();
        if size > MAX_ENTRY {
            return Err(invalid(format!("{name} is too large")));
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        // 库读到流的结尾只是少给，不报错：截断的包要在这里认出来。
        if data.len() as u64 != size {
            return Err(invalid(format!("{name} is cut short")));
        }
        *slot = Some(data);
    }
    Ok(found)
}

fn invalid(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
#[path = "package_tests.rs"]
mod tests;
