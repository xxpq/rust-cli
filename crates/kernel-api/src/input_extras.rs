//! 一条输入除正文之外带的事实（docs/specs/rust-cli/session-lifecycle.md §10.14、§10.25）：接受时固定，随这条输入排队、开回合，
//! 之后不变。前端无关：v5 前端从 `input.send` 换来；TUI 与 `-p` 不带（全空），行为不变。
//!
//! 显示的那一部分随这条输入的用户消息落库（[`InputRecord`]，`Fact::Entry` 的 [`crate::store::entry_kinds::INPUT`]），活会话
//! 与冷读读到同一份。给模型的材料（引用、页面状态）由前端层经端口成文（[`crate::frontend_prompt::FrontendPrompt`]），会话
//! actor 把它们作为 attachment 与这条输入一起提交（docs/specs/rust-cli/context.md §15.8）。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{ArtifactId, MessageId, SessionId};
use crate::model::ModelSelection;
use crate::permission::{ExecutionState, PermissionMode};
use crate::store::{Fact, entry_kinds};
use crate::user_input::InputAttachment;
use rust_cli_base::proto::mime;

/// 用户附上的一个本机文件（§10.14）：在模型边界按 `@` 提及的规则读（[`crate::user_input::UserInputs::attached`]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedFile {
    /// 会话主 env 上的绝对路径。
    pub path: String,
    /// 下面三项只供显示：前端给什么就记什么。
    pub name: String,
    pub media_type: String,
    pub size: u64,
}

impl AttachedFile {
    /// 媒体文件（图片、PDF、视频）开回合时经 [`crate::user_input::UserInputs::media`] 读，别的按 `@` 提及读（§10.41）。
    pub fn is_media(&self) -> bool {
        MediaKind::of(&self.media_type).is_some()
    }
}

/// 附件的媒体种类（§10.41）：按前端给的类型的主类型分（大小写与参数不论），同 Node `mapAttachmentRef` 的三种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Pdf,
    Video,
}

impl MediaKind {
    pub fn of(media_type: &str) -> Option<Self> {
        let base = media_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if base.starts_with(mime::IMAGE_PREFIX) {
            Some(Self::Image)
        } else if base.starts_with(mime::VIDEO_PREFIX) {
            Some(Self::Video)
        } else {
            (base == mime::PDF).then_some(Self::Pdf)
        }
    }
}

/// 用户上传进会话 Artifact 的文件（§10.40）：开回合时作为这条输入的图片块，排在正文之后（同粘贴的图片）。Artifact 的内容是 data URL
/// （[`crate::data_url`]）；`name`、`media_type`、`size` 只供显示（前端给什么就记什么），`media_type` 也决定给不给模型（图片才给）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadedFile {
    /// Artifact 所在的会话：通常是这个会话，跨工作区恢复的草稿带着原会话的。
    pub session: SessionId,
    pub artifact: ArtifactId,
    pub name: String,
    pub media_type: String,
    pub size: u64,
}

impl UploadedFile {
    /// 行与队列项里它的引用（同上传的结果）。
    pub fn uri(&self) -> String {
        crate::artifact_uri::uri(&self.session, &self.artifact)
    }
}

/// 用户引用的一段内容（§10.14，K-P1）：对话里选中的文字、文件选段、IM 里回复的消息。给模型看，也显示。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    pub text: String,
    /// 文件选段的文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// IM 里回复的那条消息的发言人、发言人 id、消息 id 与发送时刻（Unix 毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at_ms: Option<u64>,
}

/// 谁代用户发的这条输入（§10.14，K-P2）：只供显示，不给模型，内核不解释它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Relay {
    /// 插件的界面：插件与它所在的工具调用（插件面板是面板的作用域标记）。
    Plugin { plugin: String, tool_call: String },
    /// 生成的页面：页面文件与标题。
    Page {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
}

/// 生成的页面在发送时的状态（§10.14，K-P3）：只给模型，不显示。`state` 是页面自己的状态（不透明）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageState {
    pub path: String,
    pub state: Value,
}

/// 一条输入带的事实（§10.14）。全空时与以前的纯文字输入相同。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputExtras {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<AttachedFile>,
    /// 上传的图片（§10.40）：显示时排在本机文件之后。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uploads: Vec<UploadedFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quotes: Vec<Quote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<Relay>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<PageState>,
    /// 这条输入开的那一轮不许用的工具（§10.15，K-C2）：工具名；这一轮里启动的子代理继承。不落库、不显示。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,
    /// 这条输入开的那一轮的模型、权限模式与 plan 开关（§10.25）：开回合时写成会话的配置。
    #[serde(default, skip_serializing_if = "TurnConfig::is_unset")]
    pub config: TurnConfig,
    /// 这条输入开的那一轮的执行覆盖（§10.39）：这一轮的请求用它的选择与凭据引用，不写成会话的模型。装箱：同 `config.selection`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<Box<crate::turn_execution::TurnExecution>>,
    /// 这条输入的注记（§10.48）：前端给的 JSON 对象，内核只存、只合并（[`InputRecord::patched`]），不解释。只供显示，随输入记录落库。
    /// 装箱：同 `execution`（命令与排队项里多数输入没有它）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<Box<Value>>,
}

impl InputExtras {
    /// 全空（序列化时省略、与以前的纯文字输入相同）。
    pub fn is_empty(&self) -> bool {
        !self.own_turn() && self.config.is_unset()
    }

    /// 这条输入自己开一轮、运行中不插话（§10.14、§10.15）：附件、引用、来源、页面状态或禁用名单都挂在开回合的输入上。
    /// 执行覆盖也是（§10.39）：它只属于它开的那一轮，并进别的回合或插进在跑的回合就用错了模型。
    /// 执行配置不算（§10.25）：App 的每条输入都带它，算进去 App 的输入就永远不成批、不能插话。注记也是（§10.48）：成批的后续输入没有输入记录。
    pub fn own_turn(&self) -> bool {
        self.relay.is_some()
            || self.note.is_some()
            || !self.disallowed_tools.is_empty()
            || self.execution.is_some()
            || self.gives_the_model_something()
    }

    /// 有给模型的材料（附件、上传的图片、引用、页面状态）：正文为空时这条输入照样有内容（来源只供显示，不算）。
    pub fn gives_the_model_something(&self) -> bool {
        !self.files.is_empty()
            || !self.uploads.is_empty()
            || !self.quotes.is_empty()
            || !self.pages.is_empty()
    }

    /// 这条输入落库的显示事实：没有要显示的时为 None（页面状态只给模型；TUI 与 `-p` 的输入没有记录）。
    ///
    /// `ran` 是这一轮用的执行配置（开回合时套用之后会话的模型、模式与 plan，§10.59）：这条输入带了配置时记下它，界面编辑这条输入时按它载入
    /// 工具栏。记的是套用之后的值，不是输入带的那几项：没带的项用的是会话那时的值，记下来三项总是齐的（照 v4 在准入时补齐）。没带配置的输入
    /// （TUI 与 `-p`）不记，记录与以前相同。
    pub fn record(&self, message: MessageId, ran: &TurnConfig) -> Option<InputRecord> {
        let config = if self.config.is_unset() {
            TurnConfig::default()
        } else {
            ran.clone()
        };
        (!self.files.is_empty()
            || !self.uploads.is_empty()
            || !self.quotes.is_empty()
            || self.relay.is_some()
            || self.note.is_some()
            || !config.is_unset())
        .then(|| InputRecord {
            message,
            files: self.files.clone(),
            quotes: self.quotes.clone(),
            relay: self.relay.clone(),
            uploads: self.uploads.clone(),
            note: self.note.as_deref().cloned(),
            config,
        })
    }
}

/// 一条输入开的那一轮的执行配置（§10.25）：没给的项用会话那时的值。前端无关：v5 前端从 `input.send` 的
/// `modelSelection` / `mode` / `planEnabled` 换来；TUI 与 `-p` 不给。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnConfig {
    /// 装箱：配置随输入进命令、排队项与回合的输入，不让它们变大（选择本身有 80 多字节）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<Box<ModelSelection>>,
    /// 权限模式（不是旧写法 `Plan`：plan 开关在 `plan`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PermissionMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<bool>,
}

impl TurnConfig {
    pub fn is_unset(&self) -> bool {
        self.selection.is_none() && self.mode.is_none() && self.plan.is_none()
    }

    /// 会话套用它之后的执行状态：只换给了的项。
    pub fn execution_over(&self, current: ExecutionState) -> ExecutionState {
        ExecutionState {
            mode: self.mode.unwrap_or(current.mode),
            plan: self.plan.unwrap_or(current.plan),
        }
    }

    /// 要换成的模型：给了、并且与会话现在的不是同一个选择（[`ModelSelection::same_choice`]）时。
    pub fn new_selection(&self, current: &ModelSelection) -> Option<&ModelSelection> {
        self.selection
            .as_deref()
            .filter(|selection| !selection.same_choice(current))
    }
}

/// 一条输入落库的显示事实（§10.14）：与这条输入的用户消息同一批提交，键是用户消息的 id。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRecord {
    pub message: MessageId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<AttachedFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quotes: Vec<Quote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<Relay>,
    /// 上传的图片（§10.40）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uploads: Vec<UploadedFile>,
    /// 注记（§10.48）：开回合时来自 [`InputExtras::note`]，之后经 [`Self::patched`] 补字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<Value>,
    /// 这一轮用的执行配置（§10.59，[`InputExtras::record`]）：没带配置的输入为空。
    #[serde(default, skip_serializing_if = "TurnConfig::is_unset")]
    pub config: TurnConfig,
}

/// 输入记录的 `data` 里各项的键（键是用户消息的 id，不在 data 里）。
const FILES_KEY: &str = "files";
const QUOTES_KEY: &str = "quotes";
const RELAY_KEY: &str = "relay";
const UPLOADS_KEY: &str = "uploads";
const NOTE_KEY: &str = "note";
const CONFIG_KEY: &str = "config";

/// `data` 里的一项：没有这个键是缺省值；有但形状不对时为 None。
fn field<T: DeserializeOwned + Default>(data: &Value, key: &str) -> Option<T> {
    match data.get(key) {
        None | Some(Value::Null) => Some(T::default()),
        Some(value) => serde_json::from_value(value.clone()).ok(),
    }
}

impl InputRecord {
    /// 一条没有任何显示事实的记录（别的字段用结构体更新语法给）。
    pub fn new(message: MessageId) -> Self {
        Self {
            message,
            files: Vec::new(),
            quotes: Vec::new(),
            relay: None,
            uploads: Vec::new(),
            note: None,
            config: TurnConfig::default(),
        }
    }

    /// 落库的写法（`Fact::Entry`，种类 [`entry_kinds::INPUT`]，键是用户消息的 id）。
    pub fn fact(&self) -> Fact {
        let mut data = serde_json::json!({
            FILES_KEY: self.files,
            QUOTES_KEY: self.quotes,
            RELAY_KEY: self.relay,
        });
        // 只在有上传附件时写：没有上传附件的输入记录与以前逐字节相同
        if !self.uploads.is_empty() {
            data[UPLOADS_KEY] = serde_json::json!(self.uploads);
        }
        // 同上：只在有注记时写
        if let Some(note) = &self.note {
            data[NOTE_KEY] = note.clone();
        }
        // 同上：只在带了配置时写（TUI 与 `-p` 的输入不带）
        if !self.config.is_unset() {
            data[CONFIG_KEY] = serde_json::json!(self.config);
        }
        Fact::Entry {
            kind: entry_kinds::INPUT.to_owned(),
            id: self.message.as_str().to_owned(),
            data,
        }
    }

    /// 读回 [`Self::fact`] 写的 entry（`SessionStore::entries` 的一项）；不是对象或某项形状不对时为 None。
    pub fn from_entry(id: &str, data: &Value) -> Option<Self> {
        if !data.is_object() {
            return None;
        }
        Some(Self {
            message: MessageId::new(id),
            files: field(data, FILES_KEY)?,
            quotes: field(data, QUOTES_KEY)?,
            relay: field(data, RELAY_KEY)?,
            uploads: field(data, UPLOADS_KEY)?,
            note: field(data, NOTE_KEY)?,
            config: field(data, CONFIG_KEY)?,
        })
    }

    /// 补了字段的记录（`Command::PatchNote`，§10.48）：`patch` 的键浅合并进注记，后写胜；没有注记时为 None（不补）。
    pub fn patched(&self, patch: &serde_json::Map<String, Value>) -> Option<Self> {
        let mut note = self.note.clone()?;
        let fields = note.as_object_mut()?;
        fields.extend(patch.clone());
        Some(Self {
            note: Some(note),
            ..self.clone()
        })
    }
}

/// 上传的文字与附件说明的 attachment 来源（§10.41）：与 Node 的提醒同名同义（`system-reminder/source.ts` 的 `prompt_attachment`），user 角色。
pub const PROMPT_ATTACHMENT_SOURCE: &str = "prompt_attachment";
/// 引用的 attachment 来源（§10.14）：user 角色（[`crate::message::AttachmentMessage::role`]）。
pub const QUOTE_SOURCE: &str = "conversation_quote";
/// 页面状态的 attachment 来源（§10.14）：与 Node 记录里的来源同名（存储格式），user 角色。
pub const PAGE_STATE_SOURCE: &str = "gen_ui_state";

// 给模型的写法（docs/specs/rust-cli/context.md §15.8）：引用与页面状态由前端层给（`frontend_prompt`），上传的文字与附件占位在
// prompts 的 `input_material`；kernel-api 只留类型与来源名。下面三个构造器只把成文的材料配上来源（角色由来源定）。

/// 引用的材料（`text` 是前端层写的）：不加 `<system-reminder>`。
pub fn quote_attachment(text: String) -> InputAttachment {
    InputAttachment {
        source: QUOTE_SOURCE.to_owned(),
        text,
        data: Value::Null,
    }
}

/// 页面状态的材料（`text` 已包 `<system-reminder>`）。
pub fn page_state_attachment(text: String) -> InputAttachment {
    InputAttachment {
        source: PAGE_STATE_SOURCE.to_owned(),
        text,
        data: Value::Null,
    }
}

/// 附件给模型的文字（§10.41：上传的文字、读不出 / 解不开 / 模型不收的附件的占位、本机文件超过上限时的路径引用）：一条
/// `prompt_attachment` attachment，原文照给。不放进用户消息：用户消息里的文字块就是用户的正文（行、标题、落库都这么读）。
pub fn attachment_note(text: String) -> InputAttachment {
    InputAttachment {
        source: PROMPT_ATTACHMENT_SOURCE.to_owned(),
        text,
        data: Value::Null,
    }
}

#[cfg(test)]
#[path = "input_extras_tests.rs"]
mod tests;
