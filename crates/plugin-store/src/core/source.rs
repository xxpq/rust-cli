//! 来源（plugin-store.md §9.1、§9.3）：市场的来源（写进 `known_marketplaces.json` 的形状，Node `MarketplaceSource`）与
//! 市场条目的来源（`source` 字段）。只解析，不做 IO：用户输入里的本地路径在不在由 `local` 判断。

use serde_json::{Map, Value, json};

use rust_cli_kernel_api::env_files::GIT_DIR;

use super::diagnostic::StoreError;

/// GitHub 的主机名（`https://github.com/<owner>/<repo>` 的输入与 Archive 的判断）。
pub const GITHUB_HOSTS: [&str; 2] = ["github.com", "www.github.com"];
/// GitHub 仓库的 clone 地址的前缀：`<前缀>/<owner>/<repo>.git`。
pub const GITHUB_CLONE_BASE: &str = "https://github.com";
/// 市场来源里 `source` 字段的名字。
const KIND: &str = "source";
/// Azure DevOps 的 git 地址里的段（Node 认它是 git 来源）。
const AZURE_GIT_SEGMENT: &str = "/_git/";
/// Windows 盘符的写法 `X:\` 至少这么多个字节。
const DRIVE_PREFIX_LEN: usize = 3;
/// `owner/repo` 至少两段。
const REPO_SEGMENTS: usize = 2;

/// 市场的来源。`raw` 是记录里的原样（刷新时照它写回，不重新生成）。
#[derive(Debug, Clone, PartialEq)]
pub struct MarketplaceSource {
    pub kind: MarketplaceKind,
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarketplaceKind {
    Url {
        url: String,
        headers: Vec<(String, String)>,
    },
    /// GitHub 的 `owner/repo`；`path` 是仓库里清单的位置。
    Github {
        repo: String,
        git_ref: Option<String>,
        path: Option<String>,
        sparse: Vec<String>,
    },
    Git {
        url: String,
        git_ref: Option<String>,
        path: Option<String>,
        sparse: Vec<String>,
    },
    File {
        path: String,
    },
    Directory {
        path: String,
    },
    /// 认得、不支持的种类（npm、settings、hostPattern、pathPattern）或不认得的形状。
    Unsupported(String),
}

fn text(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn strings(object: &Map<String, Value>, key: &str) -> Vec<String> {
    object
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

impl MarketplaceSource {
    /// 记录里的 `source` 对象。
    pub fn from_json(raw: &Value) -> Self {
        let kind = match raw.as_object() {
            Some(object) => {
                let kind = text(object, KIND).unwrap_or_default();
                match (
                    kind.as_str(),
                    text(object, "url"),
                    text(object, "path"),
                    text(object, "repo"),
                ) {
                    ("url", Some(url), _, _) => MarketplaceKind::Url {
                        url,
                        headers: object
                            .get("headers")
                            .and_then(Value::as_object)
                            .map(|headers| {
                                headers
                                    .iter()
                                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    },
                    ("github", _, path, Some(repo)) => MarketplaceKind::Github {
                        repo,
                        git_ref: text(object, "ref"),
                        path,
                        sparse: strings(object, "sparsePaths"),
                    },
                    ("git", Some(url), path, _) => MarketplaceKind::Git {
                        url,
                        git_ref: text(object, "ref"),
                        path,
                        sparse: strings(object, "sparsePaths"),
                    },
                    ("file", _, Some(path), _) => MarketplaceKind::File { path },
                    ("directory", _, Some(path), _) => MarketplaceKind::Directory { path },
                    (other, ..) => MarketplaceKind::Unsupported(other.to_owned()),
                }
            }
            None => MarketplaceKind::Unsupported(String::new()),
        };
        Self {
            kind,
            raw: raw.clone(),
        }
    }

    fn new(kind: MarketplaceKind) -> Self {
        let raw = match &kind {
            MarketplaceKind::Url { url, .. } => json!({ KIND: "url", "url": url }),
            MarketplaceKind::Github { repo, git_ref, .. } => match git_ref {
                Some(r) => json!({ KIND: "github", "repo": repo, "ref": r }),
                None => json!({ KIND: "github", "repo": repo }),
            },
            MarketplaceKind::Git { url, git_ref, .. } => match git_ref {
                Some(r) => json!({ KIND: "git", "url": url, "ref": r }),
                None => json!({ KIND: "git", "url": url }),
            },
            MarketplaceKind::File { path } => json!({ KIND: "file", "path": path }),
            MarketplaceKind::Directory { path } => json!({ KIND: "directory", "path": path }),
            MarketplaceKind::Unsupported(kind) => json!({ KIND: kind }),
        };
        Self { kind, raw }
    }

    /// `--sparse`（Node `applySparsePaths`）：只给 git、github。
    pub fn with_sparse(mut self, paths: &[String]) -> Result<Self, StoreError> {
        let paths: Vec<String> = paths
            .iter()
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty())
            .collect();
        if paths.is_empty() {
            return Ok(self);
        }
        match &mut self.kind {
            MarketplaceKind::Github { sparse, .. } | MarketplaceKind::Git { sparse, .. } => {
                sparse.clone_from(&paths);
            }
            _ => {
                return Err(StoreError::Invalid(
                    "--sparse only applies to git or GitHub marketplace sources".to_owned(),
                ));
            }
        }
        if let Some(object) = self.raw.as_object_mut() {
            object.insert("sparsePaths".to_owned(), json!(paths));
        }
        Ok(self)
    }

    /// 缺省市场的来源写法（Node `defaultMarketplaceSourceFromString`）：`owner/repo` 是 github，其余是 url。
    pub fn default_from(text: &str) -> Self {
        let trimmed = text.trim();
        if is_github_shorthand(trimmed) {
            let (repo, git_ref) = split_github_shorthand(trimmed);
            return Self::new(MarketplaceKind::Github {
                repo,
                git_ref,
                path: None,
                sparse: Vec::new(),
            });
        }
        Self::new(MarketplaceKind::Url {
            url: trimmed.to_owned(),
            headers: Vec::new(),
        })
    }

    /// 本地路径的输入：目录或 `.json` 文件（`local` 判断后调用）。
    pub fn local(path: String, is_dir: bool) -> Self {
        Self::new(if is_dir {
            MarketplaceKind::Directory { path }
        } else {
            MarketplaceKind::File { path }
        })
    }
}

/// 用户输入（`marketplace add <source>`）的分类，照 Node `parseMarketplaceSourceInput`。
#[derive(Debug, Clone, PartialEq)]
pub enum SourceInput {
    Remote(MarketplaceSource),
    /// 本地路径（`./`、`../`、`/`、`~`、`X:\` 开头）：原样，由 `local` 展开并判断。
    Path(String),
}

pub fn classify_input(input: &str) -> Result<SourceInput, StoreError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(StoreError::Invalid(
            "Marketplace source is empty".to_owned(),
        ));
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let (url, git_ref) = split_ref(trimmed);
        if url.ends_with(GIT_DIR) || url.contains(AZURE_GIT_SEGMENT) {
            return Ok(SourceInput::Remote(git(url, git_ref)));
        }
        if is_github_web_url(&url) {
            let url = if url.ends_with(GIT_DIR) {
                url
            } else {
                format!("{url}{GIT_DIR}")
            };
            return Ok(SourceInput::Remote(git(url, git_ref)));
        }
        return Ok(SourceInput::Remote(MarketplaceSource::new(
            MarketplaceKind::Url {
                url,
                headers: Vec::new(),
            },
        )));
    }
    if is_git_ssh(trimmed) {
        let (url, git_ref) = split_ref(trimmed);
        return Ok(SourceInput::Remote(git(url, git_ref)));
    }
    if is_path_input(trimmed) {
        return Ok(SourceInput::Path(trimmed.to_owned()));
    }
    if trimmed.contains('/') && !trimmed.contains(':') {
        let (repo, git_ref) = split_github_shorthand(trimmed);
        return Ok(SourceInput::Remote(MarketplaceSource::new(
            MarketplaceKind::Github {
                repo,
                git_ref,
                path: None,
                sparse: Vec::new(),
            },
        )));
    }
    Err(StoreError::Invalid(format!(
        "Unsupported marketplace source: {input}"
    )))
}

fn git(url: String, git_ref: Option<String>) -> MarketplaceSource {
    MarketplaceSource::new(MarketplaceKind::Git {
        url,
        git_ref,
        path: None,
        sparse: Vec::new(),
    })
}

/// GitHub 网页上的仓库地址（Node：主机是 GitHub、路径至少有 `owner/repo` 两段）。
fn is_github_web_url(text: &str) -> bool {
    url::Url::parse(text).is_ok_and(|url| {
        url.host_str()
            .is_some_and(|host| GITHUB_HOSTS.contains(&host))
            && url.path().split('/').filter(|s| !s.is_empty()).count() >= REPO_SEGMENTS
    })
}

/// Node `resolvePathInput` 认作路径的开头。
fn is_path_input(input: &str) -> bool {
    let bytes = input.as_bytes();
    let drive = bytes.len() >= DRIVE_PREFIX_LEN
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    input.starts_with("./")
        || input.starts_with("../")
        || input.starts_with('/')
        || input.starts_with('~')
        || drive
}

/// Node `isGitSshUrl`：`user@host:path`。
fn is_git_ssh(input: &str) -> bool {
    let Some((user, rest)) = input.split_once('@') else {
        return false;
    };
    let Some((host, path)) = rest.split_once(':') else {
        return false;
    };
    !user.is_empty()
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !host.is_empty()
        && !path.is_empty()
}

/// Node `splitRef`：最后一个 `#` 之后是 ref。
fn split_ref(input: &str) -> (String, Option<String>) {
    match input.rsplit_once('#') {
        Some((url, git_ref)) => (url.to_owned(), Some(git_ref.to_owned())),
        None => (input.to_owned(), None),
    }
}

/// Node `splitGitHubShorthand`：最后一个 `#` 或 `@` 之后是 ref（在开头时不算）。
fn split_github_shorthand(input: &str) -> (String, Option<String>) {
    let index = [input.rfind('#'), input.rfind('@')]
        .into_iter()
        .flatten()
        .max();
    match index {
        Some(index) if index > 0 => (
            input[..index].to_owned(),
            Some(input[index + 1..].to_owned()),
        ),
        _ => (input.to_owned(), None),
    }
}

/// `owner/repo`（可带 `#ref` / `@ref`），没有 `:`。
fn is_github_shorthand(input: &str) -> bool {
    let (repo, _) = split_github_shorthand(input);
    let mut parts = repo.split('/');
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if !a.is_empty() && !b.is_empty())
        && !input.contains(':')
}

/// `owner/repo` 的 clone 地址。
pub fn github_clone_url(repo: &str) -> String {
    // 仓库地址的 `.git` 后缀与仓库目录名是同一个字面量。
    format!("{GITHUB_CLONE_BASE}/{repo}{GIT_DIR}")
}

/// 公开 GitHub 仓库的 HTTPS 地址（Node `parsePublicGitHubRepositoryUrl`）：`(owner, repo)`。
pub fn github_repository(text: &str) -> Option<(String, String)> {
    let url = url::Url::parse(text).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    if url.scheme() != "https"
        || !GITHUB_HOSTS.contains(&host.as_str())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    let [owner, repo] = segments.as_slice() else {
        return None;
    };
    let repo = repo.strip_suffix(GIT_DIR).unwrap_or(repo);
    let segment = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    };
    (segment(owner) && segment(repo)).then(|| ((*owner).to_owned(), repo.to_owned()))
}

/// 市场条目的来源（plugin-store.md §9.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSource {
    /// 相对市场目录的路径（字符串的 `source`）。
    Relative(String),
    /// 没写 `source`：市场目录下与插件同名的目录。
    ByName,
    /// `{source: "directory", path}`。
    Directory(String),
    /// git 仓库（github、git、git-subdir、不是 zip 的 url）。
    Repo {
        url: String,
        git_ref: Option<String>,
        path: Option<String>,
        sha: Option<String>,
    },
    /// `{source: "url", type: "zip", url, sha256, …}`（CDN 的 zip）。
    Zip {
        url: String,
        sha256: String,
        path: Option<String>,
        strip_root: Option<bool>,
        headers: Vec<(String, String)>,
    },
}

/// 条目的 `source` → 来源；不支持或写错时是错误（Node `resolvePluginSourceRoot` 的文字）。
pub fn plugin_source(source: Option<&Value>, label: &str) -> Result<PluginSource, StoreError> {
    let invalid = |message: String| StoreError::Invalid(message);
    let object = match source {
        None => return Ok(PluginSource::ByName),
        Some(Value::String(kind)) if kind == "filesystem" || kind == "sea" => {
            // Node 播种的内置条目：内容在 Node 的官方缓存里，rust 不从那里装（A5）。
            return Err(StoreError::Unsupported(format!(
                "Plugin source is recognized but not supported in this runtime: {kind}"
            )));
        }
        Some(Value::String(path)) => return Ok(PluginSource::Relative(path.clone())),
        Some(Value::Object(object)) => object,
        Some(_) => {
            return Err(invalid(format!(
                "Plugin source is not supported for {label}"
            )));
        }
    };
    let required = |field: &str, what: &str| {
        text(object, field)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| invalid(format!("Plugin {what} source requires a non-empty {field}")))
    };
    let pin = || text(object, "sha").or_else(|| text(object, "commit"));
    let kind = text(object, KIND).unwrap_or_default();
    match kind.as_str() {
        "directory" => Ok(PluginSource::Directory(required("path", "directory path")?)),
        "github" => Ok(PluginSource::Repo {
            url: github_clone_url(&required("repo", "GitHub repo")?),
            git_ref: text(object, "ref"),
            path: text(object, "path"),
            sha: pin(),
        }),
        "git" => Ok(PluginSource::Repo {
            url: required("url", "Git URL")?,
            git_ref: text(object, "ref"),
            path: text(object, "path"),
            sha: pin(),
        }),
        "git-subdir" => {
            let url = required("url", "git-subdir URL")?;
            // Node `normalizeGitUrl`：`owner/repo` 写法是 GitHub。
            let url = if url.matches('/').count() == 1 && !url.contains(':') {
                github_clone_url(&url)
            } else {
                url
            };
            Ok(PluginSource::Repo {
                url,
                git_ref: text(object, "ref"),
                path: Some(required("path", "git-subdir path")?),
                sha: pin(),
            })
        }
        "url" => {
            let url = required("url", "URL")?;
            match text(object, "type").as_deref() {
                Some("zip") => {
                    let sha256 = text(object, "sha256").ok_or_else(|| {
                        invalid("Plugin zip source sha256 is required".to_owned())
                    })?;
                    let headers = match object.get("headers") {
                        None => Vec::new(),
                        Some(Value::Object(headers)) => headers
                            .iter()
                            .map(|(k, v)| match v.as_str() {
                                Some(v) => Ok((k.clone(), v.to_owned())),
                                None => Err(invalid(format!(
                                    "Plugin zip source header must be a string: {k}"
                                ))),
                            })
                            .collect::<Result<_, _>>()?,
                        Some(_) => {
                            return Err(invalid(
                                "Plugin zip source headers must be an object".to_owned(),
                            ));
                        }
                    };
                    Ok(PluginSource::Zip {
                        url,
                        sha256,
                        path: text(object, "path"),
                        strip_root: object.get("stripRoot").and_then(Value::as_bool),
                        headers,
                    })
                }
                None | Some("git") | Some("") => Ok(PluginSource::Repo {
                    url,
                    git_ref: text(object, "ref"),
                    path: text(object, "path"),
                    sha: pin(),
                }),
                Some(other) => Err(StoreError::Unsupported(format!(
                    "Plugin source is recognized but not supported in this runtime: url:{other}"
                ))),
            }
        }
        "npm" | "pip" => Err(StoreError::Unsupported(format!(
            "Plugin source is recognized but not supported in this runtime: {kind}"
        ))),
        other => Err(invalid(format!(
            "Plugin source is invalid or unsupported for {label}: {}",
            if other.is_empty() {
                "missing kind"
            } else {
                other
            }
        ))),
    }
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
