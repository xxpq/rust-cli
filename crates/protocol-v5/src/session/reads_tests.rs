//! 要会话运行时的读（protocol-v5.md §43.2、§45.3）：后台输出的换算、子代理目录的筛选、排序与分页、表与库的合并；冷的答案；经 handle 读。

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::{BoxStream, StreamExt};
use rust_cli_kernel_api::agents::{StoredCall, StoredSubagent};
use rust_cli_kernel_api::background::{OutputTail, OutputUnreadable, WorkOutput};
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::ids::{AgentId, SessionId, ToolUseId};
use rust_cli_kernel_api::panels::{SubagentEntry, SubagentStatus};
use rust_cli_kernel_api::session_api::{
    Ack, CommandEnvelope, KernelError, SequencedEvent, SessionView,
};
use rust_cli_kernel_api::testing::{HandleOverride, MemoryStore};
use serde_json::json;

use super::*;

fn tail(status: BackgroundStatus) -> Option<WorkOutput> {
    Some(WorkOutput {
        status,
        read: Ok(OutputTail {
            path: EnvPath::new("/tmp/tasks/b1.output"),
            text: "done\n".to_owned(),
            truncated: true,
        }),
    })
}

#[test]
fn a_command_output_maps_its_status_and_tail() {
    assert_eq!(
        bash_output("b1", tail(BackgroundStatus::Running)),
        json!({"kind": "output", "workId": "b1", "status": "running", "output": "done\n", "truncated": true,
               "outputPath": "/tmp/tasks/b1.output"})
    );
    // 停掉的、被输出上限结束的都是 cancelled
    assert_eq!(
        bash_output("b1", tail(BackgroundStatus::Killed))["status"],
        json!("cancelled")
    );
    assert_eq!(
        bash_output("b1", tail(BackgroundStatus::Failed))["status"],
        json!("failed")
    );
    assert_eq!(
        bash_output("b1", None),
        json!({"kind": "unavailable", "workId": "b1"})
    );
    let unreadable = WorkOutput {
        status: BackgroundStatus::Completed,
        read: Err(OutputUnreadable::NotFound),
    };
    assert_eq!(
        bash_output("b1", Some(unreadable)),
        json!({"kind": "read_failed", "workId": "b1", "code": "not_found"})
    );
}

/// 快照的时刻：表里的绝对时刻按它与相对用时算出（两者出自同一次快照）。
const NOW: u64 = 10_000;

fn agent(
    id: &str,
    status: SubagentStatus,
    elapsed_ms: u64,
    ended_ago_ms: Option<u64>,
) -> SubagentEntry {
    let ended_at_ms = ended_ago_ms.map(|ago| NOW - ago);
    SubagentEntry {
        agent: AgentId::new(format!("agent_{id}")),
        child: SessionId::new(format!("ses_{id}")),
        agent_type: "Explore".to_owned(),
        description: format!("look at {id}"),
        status,
        background: false,
        waiting: false,
        elapsed_ms,
        ended_ago_ms,
        version: 0,
        owner: None,
        activity: None,
        parked: false,
        machine: rust_cli_kernel_api::ids::MachineName::primary(),
        tool_use: Some(ToolUseId::new(format!("toolu_{id}"))),
        started_at_ms: ended_at_ms.unwrap_or(NOW) - elapsed_ms,
        ended_at_ms,
    }
}

fn stored(id: &str, call: StoredCall, ended_at_ms: u64) -> StoredSubagent {
    StoredSubagent {
        tool_use: ToolUseId::new(format!("toolu_stored_{id}")),
        child: SessionId::new(format!("ses_{id}")),
        agent: Some(AgentId::new(format!("agent_{id}"))),
        agent_type: None,
        description: None,
        call,
        started_at_ms: Some(ended_at_ms - 1),
        ended_at_ms: Some(ended_at_ms),
        summary: Some(format!("answer of {id}")),
    }
}

fn children(value: &serde_json::Value, path: &str) -> Vec<String> {
    value
        .pointer(path)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["childSessionId"].as_str().unwrap().to_owned())
        .collect()
}

/// 四个主线程的子代理（两个在跑、两个结束了）与一个嵌套的。
fn directory() -> Vec<SubagentEntry> {
    let mut nested = agent("nested", SubagentStatus::Completed, 5, Some(1));
    nested.owner = Some(AgentId::new("agent_a"));
    let mut waiting = agent("w", SubagentStatus::Running, 50, None);
    waiting.waiting = true;
    let mut untitled = agent("old", SubagentStatus::Killed, 30, Some(900));
    untitled.description.clear();
    vec![
        agent("a", SubagentStatus::Running, 200, None),
        waiting,
        agent("done", SubagentStatus::Failed, 40, Some(10)),
        untitled,
        nested,
    ]
}

#[test]
fn the_directory_lists_main_thread_subagents_newest_first() {
    let value = subagents(directory(), Vec::new(), None, 20).unwrap();
    // 在跑的：开始得晚的（用时短）在前；嵌套的不列
    assert_eq!(children(&value, "/running"), ["ses_w", "ses_a"]);
    assert_eq!(value["running"][0]["status"], json!("waiting"));
    assert_eq!(value["running"][1]["status"], json!("running"));
    // 结束的：结束得晚的在前
    assert_eq!(children(&value, "/ended/items"), ["ses_done", "ses_old"]);
    assert_eq!(value["ended"]["items"][0]["status"], json!("failed"));
    assert_eq!(
        value["ended"]["items"][1],
        json!({"childSessionId": "ses_old", "agentId": "agent_old", "toolCallId": "toolu_old", "subagentType": "Explore",
               "title": SUBAGENT_TITLE, "startedAt": 9_070, "endedAt": 9_100, "status": "cancelled"})
    );
    assert_eq!(value["ended"]["total"], json!(2));
    assert!(value["ended"].get("nextCursor").is_none());
    assert_eq!(
        value["childSessionIds"],
        json!(["ses_w", "ses_a", "ses_done", "ses_old"])
    );
}

#[test]
fn ended_subagents_page_by_the_last_child_session() {
    let first = subagents(directory(), Vec::new(), None, 1).unwrap();
    assert_eq!(children(&first, "/ended/items"), ["ses_done"]);
    assert_eq!(first["ended"]["nextCursor"], json!("ses_done"));
    let second = subagents(directory(), Vec::new(), Some("ses_done"), 1).unwrap();
    assert_eq!(children(&second, "/ended/items"), ["ses_old"]);
    assert!(second["ended"].get("nextCursor").is_none());
    assert_eq!(second["ended"]["total"], json!(2));
    // 不是这份目录里结束了的项（在跑的、不认识的）→ 参数错误
    for cursor in ["ses_a", "ses_missing"] {
        let error = subagents(directory(), Vec::new(), Some(cursor), 1).unwrap_err();
        assert_eq!(error.code, codes::PRECONDITION_INVALID_PAYLOAD);
        assert!(error.message.starts_with(ENDED_CURSOR), "{}", error.message);
    }
}

/// 表与库按子会话合并（§45.3）：表里有的状态与时刻取表、toolCallId 与 summary 取库（SendMessage 续跑之后表里是那次调用，§61.1）；只在库里的
/// 按调用的结局。
#[test]
fn the_table_and_the_store_merge_by_child_session() {
    let mut resumed = agent("done", SubagentStatus::Failed, 40, Some(10));
    resumed.tool_use = Some(ToolUseId::new("toolu_send_message"));
    let store = vec![
        stored("done", StoredCall::Completed, 1),
        stored("cold", StoredCall::Completed, 500),
        stored(
            "stopped",
            StoredCall::Failed {
                error: "The operation was Aborted".to_owned(),
            },
            400,
        ),
        stored(
            "broke",
            StoredCall::Failed {
                error: "boom".to_owned(),
            },
            300,
        ),
        stored("lost", StoredCall::Unfinished, 200),
    ];
    let value = subagents(vec![resumed], store, None, 20).unwrap();
    assert_eq!(
        children(&value, "/ended/items"),
        [
            "ses_done",
            "ses_cold",
            "ses_stopped",
            "ses_broke",
            "ses_lost"
        ]
    );
    let items = &value["ended"]["items"];
    assert_eq!(
        items[0],
        json!({"childSessionId": "ses_done", "agentId": "agent_done", "toolCallId": "toolu_stored_done", "subagentType": "Explore",
               "title": "look at done", "summary": "answer of done", "startedAt": 9_950, "endedAt": 9_990, "status": "failed"})
    );
    // 只在库里的：Node 的缺省类型与标题
    assert_eq!(
        items[1],
        json!({"childSessionId": "ses_cold", "agentId": "agent_cold", "toolCallId": "toolu_stored_cold", "subagentType": "subagent",
               "title": SUBAGENT_TITLE, "summary": "answer of cold", "startedAt": 499, "endedAt": 500, "status": "success"})
    );
    let statuses: Vec<&str> = (2..5)
        .map(|index| items[index]["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["cancelled", "failed", "lost"]);
}

#[tokio::test]
async fn without_a_runtime_the_answers_come_from_the_store() {
    let store = Arc::new(MemoryStore::default());
    let session = SessionId::new("ses_1");
    let output = Read::BashOutput {
        work: "b1".to_owned(),
    };
    assert_eq!(
        output
            .answer(None, store.clone(), session.clone())
            .await
            .unwrap(),
        json!({"kind": "unavailable", "workId": "b1"})
    );
    // 库里没有这个会话（草稿、删掉了）：没有派生过子代理
    let directory = Read::Subagents {
        after: None,
        limit: 20,
    };
    assert_eq!(
        directory.answer(None, store, session).await.unwrap(),
        json!({"childSessionIds": [], "running": [], "ended": {"total": 0, "items": []}})
    );
}

/// 只答这两条读的会话句柄。
struct Handle {
    id: SessionId,
}

#[async_trait]
impl HandleOverride for Handle {
    fn id(&self) -> &SessionId {
        &self.id
    }

    async fn send(&self, _command: CommandEnvelope) -> Ack {
        unreachable!("reads send no commands")
    }

    fn subscribe(&self, _after: Option<u64>) -> BoxStream<'static, SequencedEvent> {
        futures_util::stream::pending().boxed()
    }

    async fn view(&self) -> Result<SessionView, KernelError> {
        Err(KernelError::Unsupported {
            what: "view".to_owned(),
        })
    }

    async fn subagents(&self) -> Result<Vec<SubagentEntry>, KernelError> {
        Ok(directory())
    }

    async fn work_output(
        &self,
        work: &str,
        tail_bytes: usize,
    ) -> Result<Option<WorkOutput>, KernelError> {
        assert_eq!(tail_bytes, BASH_OUTPUT_TAIL_BYTES);
        Ok((work == "b1")
            .then(|| tail(BackgroundStatus::Completed))
            .flatten())
    }
}

#[tokio::test]
async fn a_live_session_is_read_through_its_handle() {
    let handle: Arc<dyn SessionHandle> = Arc::new(Handle {
        id: SessionId::new("ses_1"),
    });
    let store: Arc<dyn StoreQueries> = Arc::new(MemoryStore::default());
    let session = SessionId::new("ses_1");
    let output = Read::BashOutput {
        work: "b1".to_owned(),
    };
    assert_eq!(
        output
            .answer(Some(handle.clone()), store.clone(), session.clone())
            .await
            .unwrap()["status"],
        json!("completed")
    );
    let unknown = Read::BashOutput {
        work: "b9".to_owned(),
    };
    assert_eq!(
        unknown
            .answer(Some(handle.clone()), store.clone(), session.clone())
            .await
            .unwrap()["kind"],
        json!("unavailable")
    );
    let directory = Read::Subagents {
        after: None,
        limit: 1,
    };
    let value = directory
        .answer(Some(handle), store, session)
        .await
        .unwrap();
    assert_eq!(value["ended"]["nextCursor"], json!("ses_done"));
}
