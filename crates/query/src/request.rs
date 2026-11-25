//! 一步的“准备请求”（docs/specs/rust-cli/query.md §1）：上下文宣布落库，按这一步的工具快照与模型组装请求。
//! 请求的字节由这里决定（逐请求的轨迹比对看它）；工作线 F131 从 `run.rs` 拆出，计算与顺序不变。

use std::sync::Arc;

use rust_cli_kernel_api::compaction::context_tokens;
use rust_cli_kernel_api::history;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::{
    ContextEdit, ModelRequest, ModelSelection, QuerySource, RequestOptions, SystemBlock,
    ThinkingConfig,
};
use rust_cli_kernel_api::orchestration::{OrchestrationError, QueryError};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::ModelCapabilities;
use rust_cli_kernel_api::tool::PromptContext;
use rust_cli_prompts::models;

use crate::RunScope;
use crate::deferred::{ToolOrder, step_tool_search, wire_tools};
use crate::limits::RequestLimits;
use crate::normalize::{NormalizeOptions, normalize};
use crate::respond::mark_interrupted;
use crate::step::StepTools;

pub(crate) fn orchestration(error: OrchestrationError) -> QueryError {
    QueryError::Unsupported {
        what: error.to_string(),
    }
}

/// 随模型变化的请求参数（该模型的默认值：
/// `max_tokens`、`thinking {adaptive, display: updates}`、`context_management` 的
/// `clear_thinking keep all`、`output_config.effort`）。
struct ModelShape {
    max_tokens: u32,
    thinking: Option<ThinkingConfig>,
    options: RequestOptions,
    normalize: NormalizeOptions,
}

/// `capabilities` 是这一步模型的生效配置（model.md §11）：对话中间的 system 只看它，答不出时按卡片的内核行为回退。卡片
/// 字段（thinking、effort、context_management、每轮 effort）只来自模型自己的卡片（卡片路，model-options.md §4.2）；
/// 请求方案决定发不发（model.md §3.2）。
/// `limits` 是入口的覆盖（修复线 F42，query.md §1.5）：`max_tokens` 由值、缺省与上限算出；thinking 关时不发 thinking 与
/// clear_thinking。
fn model_shape(
    selection: &ModelSelection,
    capabilities: Option<&ModelCapabilities>,
    limits: &RequestLimits,
) -> ModelShape {
    // 卡片路：只有模型自己的卡片写请求参数；没有卡片的模型（含 GLM）走配置路，推理字段只由 option map 写
    // （model-options.md §4.2）。修改（MO-1）：F121 时 GLM 借 opus 的卡片写这几项，选「关闭」不关。上限只按卡片或目录。
    let card = models::card(&selection.model);
    let adaptive = card.is_some_and(|card| card.adaptive_thinking) && limits.thinking_allowed();
    // 修 bug（F23）：没有卡片的模型以前也写选择的档位（`models::card_effort` 的注释）。
    let effort = models::card_effort(&selection.model, selection.reasoning.as_deref());
    let output_limit = models::card_output_limit(
        &selection.model,
        capabilities.and_then(|found| found.max_output_tokens),
    );
    ModelShape {
        max_tokens: limits.max_tokens(output_limit),
        thinking: adaptive.then(|| ThinkingConfig::Adaptive {
            display: Some("updates".to_owned()),
        }),
        options: RequestOptions {
            effort: effort.clone(),
            context_edits: if adaptive {
                vec![ContextEdit::ClearThinking {
                    keep: "all".to_owned(),
                }]
            } else {
                Vec::new()
            },
            // device_id 与上一次响应的 message id 由工作线 L6 / L1 补上。
            user_id: None,
            previous_message_id: None,
            ..RequestOptions::default()
        },
        normalize: NormalizeOptions {
            system_turns: models::mid_conversation_system(
                &selection.model,
                capabilities.map(|found| found.supports_mid_conversation_system),
            ),
            turn_effort: effort.filter(|_| card.is_some_and(|card| card.per_turn_effort)),
            skip_cache_write: false,
        },
    }
}

/// 这一步请求的选择（model-options.md 待确认 6）：卡片模型的选择没有档位（TUI 的 `auto`、旧数据）时，provider 有卡片的缺省
/// 档位就写成它（只改这个请求，`RunModel` 与会话的选择不变），补丁按它求值，与卡片写的 effort 相同。别的情形原样，不多查档位。
///
/// 修 bug（D4）：以前请求的选择没有档位，平台层按最后一档求补丁（卡片模型是 `max`），卡片写 `effort: medium`，补丁优先，
/// 实际发的是 `max`。“没有档位”在内核与平台层各解释了一次。
async fn card_selection(scope: &RunScope, selection: &ModelSelection) -> ModelSelection {
    if selection.reasoning.is_some() || models::card(&selection.model).is_none() {
        return selection.clone();
    }
    let offered = scope.parts.model.reasoning_levels(selection).await;
    ModelSelection {
        reasoning: models::card_level(&selection.model, offered.as_deref()),
        ..selection.clone()
    }
}

/// 这一步的请求与它的输出上限（没有 tool_use 时按它判定输出上限的续写）。
pub(crate) struct Prepared {
    pub(crate) request: ModelRequest,
    pub(crate) max_tokens: u32,
}

/// 上下文宣布（规则文件、会话上下文、日期、署名）落库，再组装请求。已中止时写中断标记、返回 None。`prior_turns`：会话在这次
/// 运行之前已经完成过回合（运行开始时算一次，provider-errors.md §5.3.2）。
///
/// `history` 是主循环的历史（只追加）：这里的追加不复制（没有别的引用），组装时把一份引用交给前缀计划
/// （request-prefix.md §3、§8）。
pub(crate) async fn prepare(
    scope: &RunScope,
    history: &mut Arc<Vec<Message>>,
    step: &StepTools,
    (selection, source, prior_turns): (&ModelSelection, &QuerySource, bool),
) -> Result<Option<Prepared>, QueryError> {
    let request = step.context_request(scope, selection, source).await;
    let mode = request.mode;
    let context = scope
        .parts
        .context
        .build(request, history)
        .await
        .map_err(orchestration)?;
    for attachment in context.attachments {
        let message = Message::Attachment(attachment);
        scope.sink.commit_message(message.clone()).await?;
        Arc::make_mut(history).push(message);
    }
    // 修 bug（F37，query.md §1.2）：以前在这一步开头看到中止就返回，什么都不写；会话在输入还没提交时被中断
    // （展开或 UserPromptSubmit 期间）时照常提交输入、带着已中止的信号开主循环，这一轮就没有中断标记。正确的时序是
    // 发请求之前不看中止，请求立即失败、写中断标记，上下文宣布已经在前面产生。这里不发请求，宣布之后写同样的标记。
    if scope.abort.is_aborted() {
        mark_interrupted(scope, Arc::make_mut(history), false).await?;
        return Ok(None);
    }
    let context = (context.system, mode);
    Ok(Some(
        assemble(
            scope,
            history,
            step,
            (selection, source, prior_turns),
            context,
        )
        .await,
    ))
}

/// 按这一步的工具快照、模型与活动历史组装请求：工具清单与 ToolSearch 的形式、`machine` 参数、规范化的消息、随模型
/// 变化的参数与输出预算。
async fn assemble(
    scope: &RunScope,
    history: &Arc<Vec<Message>>,
    step: &StepTools,
    (selection, source, prior_turns): (&ModelSelection, &QuerySource, bool),
    (system, mode): (Vec<SystemBlock>, PermissionMode),
) -> Prepared {
    let prompt = PromptContext {
        tool_names: step.names(),
        mode,
    };
    let capabilities = step.capabilities.as_ref();
    // 只发活动历史：最后一个压缩边界之后，保留段接回摘要之后（compact.md §5）。
    let active = history::active(history);
    // 目录模型的输出预算（修复线 F108，query.md §1.11）：按这一步的活动历史估算输入（与压缩的判定同一个函数），
    // 交给补丁的上限按窗口余量压低。
    let estimate = context_tokens(&active.iter().map(AsRef::as_ref).collect::<Vec<_>>());
    let provider_max_output_tokens = crate::output_budget::provider_output_limit(
        capabilities,
        scope.parts.limits.max_output_tokens,
        source,
        estimate,
    );
    // 延迟工具与 ToolSearch（tools-more.md §3.3）：排序、按需加载、占位工具与 `defer_loading`。这一步开不开看工具里有没有
    // ToolSearch 与 `ENABLE_TOOL_SEARCH`（自动时按门槛，工作线 F89，§3.2.1）；开着时的形式按端点的能力（§3.2.2：产品
    // 路径一律本地，F89；F84 时 z.ai / bigmodel 原生）。
    let tool_search = step_tool_search(
        &step.tools,
        &prompt,
        (
            scope.parts.limits.tool_search,
            &selection.model,
            capabilities,
        ),
    );
    // 子代理点名的工具清单按清单的顺序发（agents.md §4.2，F16），其余按名字排序。
    let order = if scope.parts.tools.keeps_order() {
        ToolOrder::AsGiven
    } else {
        ToolOrder::ByName
    };
    let history_iter = active.iter().map(AsRef::as_ref);
    let mut wired = wire_tools(&step.tools, history_iter, &prompt, (tool_search, order));
    // 有远端机器连上时带 `machine` 参数的工具多这一项（R3，remote-exec.md §20.5.1）；没有时不变。
    crate::machines::add_machine_param(
        &mut wired.definitions,
        &step.tools,
        &scope.parts.machines.machines(),
        &scope.default_machine,
    );
    step.declare(scope, source);
    let sources = crate::prefix::sources(&active, wired.mode, &prompt.tool_names);
    let selection = &card_selection(scope, selection).await;
    let mut shape = model_shape(selection, capabilities, &scope.parts.limits);
    // 摘要请求不写缓存（context.md §3 第 7 条）。
    shape.normalize.skip_cache_write = *source == QuerySource::Compact;
    let max_tokens = shape.max_tokens;
    let sources: Vec<&Message> = sources.iter().map(AsRef::as_ref).collect();
    // 主线程的请求在组装根打开时接着上一步的已定前缀（request-prefix.md §2、§3）：`messages` 是前缀之后的那一段。
    let (messages, prefix) = match &scope.parts.prefixes {
        Some(table) if *source == QuerySource::MainThread => {
            let input = crate::prefix::StepInput {
                session: &scope.session,
                table,
                history,
                tool_search: wired.mode,
                tool_names: &prompt.tool_names,
                options: &shape.normalize,
                artifacts: &scope.parts.artifacts,
            };
            let (messages, plan) = crate::prefix::messages(input, &sources).await;
            (messages, Some(plan))
        }
        _ => {
            let mut messages = normalize(sources.iter().copied(), &shape.normalize);
            // 上传的图片在消息里只是 Artifact 引用：发出之前读成 base64（session-lifecycle.md §10.40）。
            crate::materialize::materialize(&mut messages, scope.parts.artifacts.as_ref()).await;
            (messages, None)
        }
    };
    let request = ModelRequest {
        selection: selection.clone(),
        system,
        messages,
        tools: wired.definitions,
        tool_choice: None,
        max_tokens: shape.max_tokens,
        stop_sequences: Vec::new(),
        thinking: shape.thinking,
        temperature: None,
        betas: Vec::new(),
        source: source.clone(),
        options: RequestOptions {
            tool_flags: wired.flags,
            // 工作线 F52（model.md §3.3）：发请求的会话（子代理是子会话），模型适配器据此写 `metadata.user_id`。
            session_id: Some(scope.session.clone()),
            // 请求属于的顶层会话（platform.md §5.3）：子代理的运行才与 `session` 不同；
            // 经宿主取账号凭据的来源按它找会话。主线程不写（None 是 `session_id` 自己）。
            root: scope.nested_root(),
            // 工作线 F64（model.md §2.4）：这次运行（回合）与子代理，模型适配器据此写每请求头。
            run: Some(scope.run.clone()),
            agent: scope.agent.clone(),
            provider_max_output_tokens,
            prior_turns,
            prefix,
            ..shape.options
        },
    };
    Prepared {
        request,
        max_tokens,
    }
}
