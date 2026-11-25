// rust-cli 与 Node CLI 的会话互操作检查的第二十九段：工具调用的开工与结束（docs/specs/rust-cli/node-store.md §4.6 第 4 条，
// session-lifecycle.md §10.37，TOOL-TIME）。
//
// rust-cli 写一个会话（node_interop `tool-time-write`：一个成功、一个失败的调用，各自的结果与它的计量同一批）：
// 1. 两个 tool part 的 `state.time` 是计量的开工与结束（`ToolCall::time`），不是结果消息的创建时刻（Node 的 `end` 是 `result.completedAt`）；
// 2. Node 的读法：v4 的冷读合成（`synthesizeEventsFromMessages`）的 `ToolCallStarted.startedAt` 与 `ToolCallResult.duration` 拿到这一对；
//    每条消息过 App 的 strict schema；
// 3. Node 用自己的写入代码续写一轮带起止的调用，rust-cli 冷读取的 `tool_times` 读回它，rust 写的两个不变。

import { appSchemaRejections } from "./node-interop-history.mjs";
import { importNode } from "./node-root.mjs";

/** Node 续写的那一次调用开工之后多久结束（毫秒）。 */
const NODE_RUN_MS = 300;

export async function checkToolTime(ctx) {
  const { nodeRoot, store, dir, rust, expectEqual } = ctx;
  const { nodeRuntime, createMessageId, createPartId, createTurnId, persistence } = ctx.node;
  const { persistPendingToolPart } = await importNode(
    nodeRoot,
    "core/src/runtime/methods/tool-part-persistence.ts",
  );
  const { synthesizeEventsFromMessages } = await importNode(
    nodeRoot,
    "bootstrap/src/zcode-protocol-v4/transcript-hydration.ts",
  );

  const written = rust("tool-time-write", dir);
  const sessionID = written.session;
  const messages = await store.messages({ sessionID });
  const reply = messages.find((message) => message.info.id === written.reply);
  const stored = (call) =>
    reply?.parts.find((p) => p.type === "tool" && p.callID === call)?.state.time;

  // 1. 库里的起止是计量的那一对
  expectEqual(
    "the stored tool part times are the metered start and end",
    written.times.map((time) => stored(time.tool_use)),
    written.times.map((time) => ({ start: time.started_at, end: time.ended_at })),
  );

  // 2. Node 的读法与 App 的 strict schema
  const events = synthesizeEventsFromMessages(messages, { sessionId: sessionID });
  const started = events
    .filter((event) => event.type === "tool_call_started")
    .map((event) => [event.payload.toolCallId, event.payload.startedAt.getTime()]);
  const results = events
    .filter((event) => event.type === "tool_call_result")
    .map((event) => [event.payload.toolCallId, event.payload.duration]);
  expectEqual(
    "the v4 cold synthesis reads the start and the duration",
    { started, results },
    {
      started: written.times.map((time) => [time.tool_use, time.started_at]),
      results: written.times.map((time) => [time.tool_use, time.ended_at - time.started_at]),
    },
  );
  expectEqual(
    "the App schema accepts the session",
    await appSchemaRejections(nodeRoot, store, [sessionID]),
    [],
  );

  // 3. Node 续写一轮带起止的调用（Node 的 pending part 与 settleToolResults 的 completed 形状），rust-cli 读回
  const { runtime, selection } = await nodeRuntime(sessionID);
  const model = { providerId: selection.providerId, modelId: selection.modelId };
  const trace = { traceId: sessionID, turnId: createTurnId() };
  const askID = createMessageId();
  await persistence.persistUserPrompt.call(runtime, askID, "Node reads a file", undefined, trace, {
    sourceCommandId: "node_cmd_tool_time",
  });
  const stepID = createMessageId();
  const created = Date.now();
  await persistence.persistAssistantMessage.call(
    runtime,
    stepID,
    askID,
    created,
    { completed: Date.now(), finish: "tool-calls" },
    trace,
    model,
  );
  const partID = createPartId();
  const toolCall = { id: "call_node_time", name: "Read", input: { file_path: "/w/interop/b.txt" } };
  await persistPendingToolPart(runtime, {
    assistantMessageId: stepID,
    declarationIndex: 0,
    input: toolCall.input,
    partID,
    toolCall,
    traceContext: trace,
    model,
  });
  const nodeTime = { start: created, end: created + NODE_RUN_MS };
  await runtime.persistPart({
    id: partID,
    sessionID,
    messageID: stepID,
    type: "tool",
    callID: toolCall.id,
    declarationIndex: 0,
    tool: toolCall.name,
    metadata: {},
    state: {
      status: "completed",
      input: toolCall.input,
      output: "b",
      title: toolCall.name,
      metadata: { schemaVersion: 1 },
      time: nodeTime,
    },
  });
  const loaded = rust("read", dir, sessionID);
  expectEqual("rust-cli reads back the times of both sides", loaded.tool_times, [
    ...written.times,
    { tool_use: toolCall.id, started_at: nodeTime.start, ended_at: nodeTime.end },
  ]);
}
