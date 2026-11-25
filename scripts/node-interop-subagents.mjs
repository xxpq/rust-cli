// rust-cli 与 Node CLI 的会话互操作检查的第三十三段：Node 写的子代理关联（docs/specs/rust-cli/protocol-v5.md §54.3、§45.3，
// 重做线 SUBAGENT-ROWS）。
//
// Node 用自己的存储代码写一个父会话（一轮输入、一条 Agent 调用，结果文字里有 `agentId:` 一行，没有本工程写的 metadata）与它的
// 子会话（`sess_subagent_<agentId>`，taskType `subagent_child`，子代理的回答），再写一条不是子代理的子会话被 Agent 调用指着
//（另一种子会话：不进目录）。检查 rust-cli 的 `StoreQueries::subagents` 读出这一个关联：调用、子会话、agentId、类型（input 的
// `subagent_type`）、说明、结局、开始与结束的时刻、子代理最后的回答。v5 的冷读由它出 `subagent` 行（protocol-v5.md §54.3）。

import { importNode } from "./node-root.mjs";

const AGENT = "node_agent_sa1";
const CHILD = `sess_subagent_${AGENT}`;
const CALL = "toolu_node_agent_sa1";
const OTHER_CALL = "toolu_node_agent_other";
const OTHER_CHILD = "sess_node_workflow_child";

export async function checkSubagents(ctx) {
  const { buildExecutionStateEntry } = await importNode(
    ctx.nodeRoot,
    "core/src/runtime/execution-state.ts",
  );
  const { store, dir, rust, expectEqual, written } = ctx;
  const { nodeRuntime, persistence, createMessageId, createPartId, createTurnId } = ctx.node;
  const base = await store.getSession(written.sessions[0]);
  // Node 建会话（建子代理的子会话时 Node 带 taskType，core/src/runtime/methods/subagent.ts），再写执行状态：Node 的写入代码要读它
  const create = async (id, extra) => {
    await store.createSession({
      id,
      projectID: base.projectID,
      slug: id,
      directory: written.workspace,
      title: id,
      version: base.version,
      ...extra,
    });
    await store.saveSessionEntry(
      buildExecutionStateEntry(id, { mode: "build", planEnabled: false }),
    );
  };
  const parent = "sess_node_subagent_parent";
  await create(parent, {});
  await create(CHILD, { parentID: parent, taskType: "subagent_child" });
  await create(OTHER_CHILD, { parentID: parent });

  // 父会话：一轮输入、一条两次 Agent 调用的回复（Node 的 completed tool part）
  // 新建的会话还没有模型：写入代码用第一个会话的模型
  const { selection } = await nodeRuntime(written.sessions[0]);
  const runtimeOf = async (id) => {
    const { runtime } = await nodeRuntime(id);
    runtime.getSessionModelSelection = () => selection;
    return runtime;
  };
  const runtime = await runtimeOf(parent);
  const model = { providerId: selection.providerId, modelId: selection.modelId };
  const trace = { traceId: parent, turnId: createTurnId() };
  const askID = createMessageId();
  await persistence.persistUserPrompt.call(runtime, askID, "Node delegates", undefined, trace, {
    sourceCommandId: "node_cmd_subagents",
  });
  const stepID = createMessageId();
  const started = Date.now();
  await persistence.persistAssistantMessage.call(
    runtime,
    stepID,
    askID,
    started,
    { completed: Date.now(), finish: "tool-calls" },
    trace,
    model,
  );
  const agentPart = (callID, child, description) => ({
    id: createPartId(),
    sessionID: parent,
    messageID: stepID,
    type: "tool",
    callID,
    tool: "Agent",
    metadata: {},
    state: {
      status: "completed",
      input: { description, prompt: "Look around.", subagent_type: "Explore" },
      output: `Node subagent finished.\nagentId: ${child} (use SendMessage with to: '${child}' to continue this agent)`,
      title: "Agent",
      metadata: { schemaVersion: 1 },
      time: { start: started, end: started + 5 },
    },
  });
  await runtime.persistPart(agentPart(CALL, AGENT, "Node look"));
  // 另一种子会话：结果文字指着它，可它不是子代理的子会话（taskType 不是 subagent_child），不进目录
  const other = agentPart(OTHER_CALL, AGENT, "Not a subagent");
  other.state.output = "Node launched something else.";
  other.metadata = { childSessionId: OTHER_CHILD, agentId: "node_other" };
  await runtime.persistPart(other);

  // 子会话：子代理的输入与回答
  const child = { runtime: await runtimeOf(CHILD) };
  const childTrace = { traceId: CHILD, turnId: createTurnId() };
  const childAsk = createMessageId();
  await persistence.persistUserPrompt.call(
    child.runtime,
    childAsk,
    "Look around.",
    undefined,
    childTrace,
    {
      sourceCommandId: "node_cmd_subagents_child",
    },
  );
  const answerID = createMessageId();
  const answered = Date.now();
  await persistence.persistAssistantMessage.call(
    child.runtime,
    answerID,
    childAsk,
    answered,
    { completed: answered + 1, finish: "stop" },
    childTrace,
    model,
  );
  await child.runtime.persistPart({
    id: createPartId(),
    sessionID: CHILD,
    messageID: answerID,
    type: "text",
    text: "  Node child answer  ",
    time: { start: answered, end: answered + 1 },
  });

  expectEqual("rust-cli reads the association Node wrote", rust("subagents", dir, parent), [
    {
      tool_use: CALL,
      child: CHILD,
      agent: AGENT,
      agent_type: "Explore",
      description: "Node look",
      call: { kind: "completed" },
      started_at_ms: started,
      ended_at_ms: started + 5,
      summary: "Node child answer",
    },
  ]);
}
