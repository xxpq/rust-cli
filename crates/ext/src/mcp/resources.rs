//! `ListMcpResourcesTool` 与 `ReadMcpResourceTool`（docs/specs/rust-cli/ext.md §3.7）。有 resources 能力的服务器
//! 连上后才注册。描述与 schema 在 `prompts/tools/*.json`。

use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_kernel_api::classifier::ClassifierInput;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, failed, text,
};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use super::connection;
use super::manager::{Inner, resource_servers};
use super::rpc::{self, Client, RpcError};
use rust_cli_kernel_api::tool_names;

pub(crate) const LIST: &str = tool_names::LIST_MCP_RESOURCES;
pub(crate) const READ: &str = tool_names::READ_MCP_RESOURCE;
pub(crate) const READ_DIR: &str = tool_names::READ_MCP_RESOURCE_DIR;
/// MCP SDK 的 `ResourceSchema` 字段顺序：结果是 schema 解析后的对象，键按 schema 的顺序，
/// 未知的键在后。
const RESOURCE_KEYS: &[&str] = &[
    "name",
    "title",
    "uri",
    "description",
    "mimeType",
    "annotations",
    "size",
    "_meta",
];
/// 结果的字符上限（通用值只定义在 kernel-api）。
const MAX_RESULT_CHARS: usize = rust_cli_kernel_api::tool::DEFAULT_MAX_RESULT_CHARS;

/// 三个资源工具，按名字排序（和其他内置工具一起按名字排）。`managers` 是这次运行看得到的连接：内核级的那一份，
/// 有时加上顶层会话自己的那一份（工作线 F145，ext.md §3.9）。
pub(crate) fn tools(managers: Vec<Weak<Inner>>) -> Vec<Arc<dyn Tool>> {
    let managers = Managers(managers);
    vec![
        Arc::new(ListResources {
            managers: managers.clone(),
        }),
        Arc::new(ReadResourceDir),
        Arc::new(ReadResource { managers }),
    ]
}

/// 资源工具看得到的管理器。
#[derive(Clone)]
struct Managers(Vec<Weak<Inner>>);

/// 一个有 resources 能力的已连接服务器：名字、客户端与它所在的管理器。
type ResourceServer = (String, Arc<Client>, Arc<Inner>);

impl Managers {
    /// 还在的管理器；一个都不在时报错。
    fn alive(&self) -> Result<Vec<Arc<Inner>>, ToolError> {
        let alive: Vec<Arc<Inner>> = self.0.iter().filter_map(Weak::upgrade).collect();
        if alive.is_empty() {
            return Err(failed("MCP is shut down".to_owned()));
        }
        Ok(alive)
    }

    /// 已连接、有 resources 能力的服务器：（名字，客户端，它所在的管理器）。
    fn servers(&self) -> Result<Vec<ResourceServer>, ToolError> {
        Ok(self
            .alive()?
            .into_iter()
            .flat_map(|inner| {
                resource_servers(&inner)
                    .into_iter()
                    .map(move |(name, client)| (name, client, inner.clone()))
            })
            .collect())
    }
}

/// 资源对象按 SDK schema 的顺序重排键，再加上 `server`。
fn with_server(resource: Value, server: &str) -> Value {
    let Value::Object(mut fields) = resource else {
        return resource;
    };
    let mut out = Map::new();
    for key in RESOURCE_KEYS {
        if let Some(value) = fields.remove(*key) {
            out.insert((*key).to_owned(), value);
        }
    }
    out.extend(fields);
    out.insert("server".to_owned(), Value::String(server.to_owned()));
    Value::Object(out)
}

struct ListResources {
    managers: Managers,
}

#[async_trait]
impl Tool for ListResources {
    fn classifier_input(&self, input: &Value) -> ClassifierInput {
        ClassifierInput::first_of(input, &["server"])
    }

    fn name(&self) -> &str {
        LIST
    }

    /// 延迟加载（`shouldDefer: true`，tools-more.md §3.1）。
    fn should_defer(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some("list resources from connected MCP servers")
    }

    fn input_schema(&self) -> Value {
        rust_cli_prompts::definition(LIST).schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        rust_cli_prompts::definition(LIST).description.clone()
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(MAX_RESULT_CHARS)
    }

    async fn call(
        &self,
        input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let wanted = input.get("server").and_then(Value::as_str);
        let servers = self.managers.servers()?;
        if let Some(wanted) = wanted
            && !servers.iter().any(|(name, ..)| name == wanted)
        {
            let names: Vec<&str> = servers.iter().map(|(name, ..)| name.as_str()).collect();
            return Err(failed(format!(
                "Server \"{wanted}\" not found. Available servers: {}",
                names.join(", ")
            )));
        }
        let mut all = Vec::new();
        for (name, client, manager) in servers {
            if wanted.is_some_and(|wanted| wanted != name) {
                continue;
            }
            let options = manager.options(&name);
            let timeout = Duration::from_millis(manager.settings().connect_timeout_for(options));
            match connection::list(&client, "resources/list", "resources", timeout).await {
                Ok(resources) => all.extend(
                    resources
                        .into_iter()
                        .map(|resource| with_server(resource, &name)),
                ),
                Err(error) => tracing::warn!(server = name, %error, "MCP resources/list failed"),
            }
        }
        if all.is_empty() {
            return Ok(text(
                "No resources found. MCP servers may still provide tools even if they have no resources."
                    .to_owned(),
            ));
        }
        Ok(text(Value::Array(all).to_string()))
    }
}

struct ReadResource {
    managers: Managers,
}

/// 目录资源的列出：缺省构建没有打开（`isMcpSkillsEnabled` 为假），总是回答未启用。
struct ReadResourceDir;

#[async_trait]
impl Tool for ReadResourceDir {
    fn classifier_input(&self, input: &Value) -> ClassifierInput {
        server_and_uri(input)
    }

    fn name(&self) -> &str {
        READ_DIR
    }

    /// 延迟加载（`shouldDefer: true`，tools-more.md §3.1）。
    fn should_defer(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some("list the children of an MCP directory resource")
    }

    fn input_schema(&self) -> Value {
        rust_cli_prompts::definition(READ_DIR).schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        rust_cli_prompts::definition(READ_DIR).description.clone()
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        Ok(text(
            "Directory listing is not enabled in this build.".to_owned(),
        ))
    }
}

/// 一项内容：文本原样；二进制不落盘（ext.md §8 D9）。
fn content(item: &Value) -> Value {
    let mut out = Map::new();
    for key in ["uri", "mimeType"] {
        if let Some(value) = item.get(key) {
            out.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(text) = item.get("text") {
        out.insert("text".to_owned(), text.clone());
    } else if item.get("blob").is_some_and(Value::is_string) {
        out.insert(
            "text".to_owned(),
            Value::String("Binary content could not be saved to disk: not supported".to_owned()),
        );
    }
    Value::Object(out)
}

#[async_trait]
impl Tool for ReadResource {
    fn classifier_input(&self, input: &Value) -> ClassifierInput {
        server_and_uri(input)
    }

    fn name(&self) -> &str {
        READ
    }

    /// 延迟加载（`shouldDefer: true`，tools-more.md §3.1）。
    fn should_defer(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some("read a specific MCP resource by URI")
    }

    fn input_schema(&self) -> Value {
        rust_cli_prompts::definition(READ).schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        rust_cli_prompts::definition(READ).description.clone()
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(MAX_RESULT_CHARS)
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let server = input
            .get("server")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let uri = input.get("uri").and_then(Value::as_str).unwrap_or_default();
        let servers = self.managers.servers()?;
        let Some((_, client, manager)) = servers.iter().find(|(name, ..)| name == server) else {
            let connected = self
                .managers
                .alive()?
                .iter()
                .any(|manager| manager.client(server).is_some());
            return if connected {
                Err(failed(format!(
                    "Server \"{server}\" does not support resources"
                )))
            } else {
                Err(failed(format!("Server \"{server}\" is not connected")))
            };
        };
        let options = manager.options(server);
        let timeout = Duration::from_millis(manager.settings().tool_timeout_for(options));
        // 同 MCP 工具的调用：读的时候服务器的询问挂在这次调用上（ext.md §3.13.2）。
        let read = client
            .request_for(
                super::asks::Caller::of(&context),
                "resources/read",
                json!({"uri": uri}),
                (timeout, &context.cancel),
            )
            .await;
        let result = match read {
            Ok(result) => result,
            Err(RpcError::Cancelled) => return Err(ToolError::Cancelled),
            Err(RpcError::Server {
                code: rpc::METHOD_NOT_FOUND,
                ..
            }) => {
                return Ok(text(format!(
                    "Server \"{server}\" advertises resource support but does not implement resource reads."
                )));
            }
            Err(error) => return Err(failed(error.to_string())),
        };
        let contents: Vec<Value> = result
            .get("contents")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(content).collect())
            .unwrap_or_default();
        Ok(text(json!({"contents": contents}).to_string()))
    }
}

/// 读资源的两个工具给自动模式的分类器看的内容（auto-classifier.md §15.3）：`<server> <uri>`。
fn server_and_uri(input: &Value) -> ClassifierInput {
    match (input.get("server"), input.get("uri")) {
        (Some(server), Some(uri)) => ClassifierInput::text(format!(
            "{} {}",
            rust_cli_kernel_api::classifier::js_text(server),
            rust_cli_kernel_api::classifier::js_text(uri)
        )),
        _ => ClassifierInput::raw(input),
    }
}
