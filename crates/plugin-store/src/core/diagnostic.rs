//! 诊断与错误（plugin-store.md §11）。`code` 用 Node 的闭集（`PluginDiagnosticCode`）；错误是有类型的，每种对应一个
//! `code`（Node 匹配错误文字，这里按类型，结果相同）。

use serde_json::{Map, Value};

/// Node 的诊断码（只列本工程会发的）。
pub mod code {
    pub const NOT_FOUND: &str = "plugin_not_found";
    pub const MARKETPLACE_INVALID: &str = "plugin_marketplace_invalid";
    pub const DEPENDENCY_MISSING: &str = "plugin_dependency_missing";
    pub const DEPENDENCY_CYCLE: &str = "plugin_dependency_cycle";
    pub const DEPENDENCY_CROSS_MARKETPLACE: &str = "plugin_dependency_cross_marketplace";
    pub const SOURCE_UNSUPPORTED: &str = "plugin_marketplace_source_unsupported";
    pub const GIT_UNAVAILABLE: &str = "plugin_git_unavailable";
    pub const ARCHIVE_FETCH_FAILED: &str = "plugin_archive_fetch_failed";
    pub const MANIFEST_NOT_FOUND: &str = "plugin_manifest_not_found";
    pub const MANIFEST_INVALID: &str = "plugin_manifest_invalid";
    pub const UNSUPPORTED_COMPONENT: &str = "plugin_unsupported_component";
    pub const VARIABLE_MISSING: &str = "plugin_variable_missing";
    pub const VALIDATION_DEFERRED: &str = "plugin_validation_deferred";
    pub const SECRETS_UNREADABLE: &str = "plugin_secrets_unreadable";
    pub const ROOT_NOT_FOUND: &str = "plugin_root_not_found";
    pub const MCP_INVALID: &str = "plugin_mcp_invalid";
    pub const COMPONENT_PATH_INVALID: &str = "plugin_component_path_invalid";
    /// 声明的市场用了官方的 id（plugin-store.md §9.5）。
    pub const DECLARATION_RESERVED: &str = "plugin_marketplace_declaration_reserved";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// 一条诊断（Node `PluginDiagnostic`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    pub severity: Severity,
    pub path: Option<String>,
    pub plugin_id: Option<String>,
}

impl Diagnostic {
    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            severity: Severity::Error,
            path: None,
            plugin_id: None,
        }
    }

    pub fn warning(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(code, message)
        }
    }

    pub fn at(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn of(mut self, plugin_id: impl Into<String>) -> Self {
        self.plugin_id = Some(plugin_id.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// JSON 的形状（Node `formatDiagnosticJson`）：`{code, message, severity, path?, pluginId?}`。
    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("code".into(), self.code.into());
        object.insert("message".into(), self.message.clone().into());
        object.insert("severity".into(), self.severity.as_str().into());
        if let Some(path) = &self.path {
            object.insert("path".into(), path.clone().into());
        }
        if let Some(id) = &self.plugin_id {
            object.insert("pluginId".into(), id.clone().into());
        }
        Value::Object(object)
    }
}

/// 存储操作的错误。文字照 Node（命令层原样显示）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("Plugin not found: {0}")]
    PluginNotFound(String),
    #[error("Marketplace not found: {0}")]
    MarketplaceNotFound(String),
    #[error("{0}")]
    DependencyMissing(String),
    #[error("{0}")]
    DependencyCycle(String),
    #[error("{0}")]
    CrossMarketplace(String),
    /// 认得、但这个运行时不支持的来源（npm、pip、settings 等）。
    #[error("{0}")]
    Unsupported(String),
    #[error("Git is not available: {0}")]
    GitUnavailable(String),
    #[error("{0}")]
    Fetch(String),
    #[error("{0}")]
    ManifestMissing(String),
    #[error("{0}")]
    ManifestInvalid(String),
    /// 清单、记录文件或别的输入不合法；IO 失败。
    #[error("{0}")]
    Invalid(String),
    /// 等存储锁超时。
    #[error("the plugin storage is locked by another process: {0}")]
    Locked(String),
    #[error("Plugin operation cancelled")]
    Cancelled,
}

impl StoreError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::PluginNotFound(_) => code::NOT_FOUND,
            Self::DependencyMissing(_) => code::DEPENDENCY_MISSING,
            Self::DependencyCycle(_) => code::DEPENDENCY_CYCLE,
            Self::CrossMarketplace(_) => code::DEPENDENCY_CROSS_MARKETPLACE,
            Self::Unsupported(_) => code::SOURCE_UNSUPPORTED,
            Self::GitUnavailable(_) => code::GIT_UNAVAILABLE,
            Self::Fetch(_) => code::ARCHIVE_FETCH_FAILED,
            Self::ManifestMissing(_) => code::MANIFEST_NOT_FOUND,
            Self::ManifestInvalid(_) => code::MANIFEST_INVALID,
            Self::MarketplaceNotFound(_) | Self::Invalid(_) | Self::Locked(_) | Self::Cancelled => {
                code::MARKETPLACE_INVALID
            }
        }
    }

    /// 一条 error 级诊断（`plugin_id` 是这次操作的目标）。
    pub fn to_diagnostic(&self, plugin_id: Option<&str>) -> Diagnostic {
        let diagnostic = Diagnostic::error(self.code(), self.to_string());
        match plugin_id {
            Some(id) => diagnostic.of(id),
            None => diagnostic,
        }
    }

    /// IO 失败：带上路径。
    pub fn io(path: &std::path::Path, error: &std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Self::Cancelled;
        }
        Self::Invalid(format!("{}: {error}", path.display()))
    }
}
