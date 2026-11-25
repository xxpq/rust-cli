// rust-cli 与 Node CLI 的会话互操作检查的第二十八段：Bash 的输出头部（docs/specs/rust-cli/node-store.md §4.6 第 7 条，
// session-lifecycle.md §10.34，PROJ-GAPS）。
//
// rust-cli 写一个会话（node_interop `output-head-write`：一次成功的大输出、一次失败的大输出，各带 `Fact::OutputHead`）：
// 1. 两个 tool part 的 `state.metadata`：display 与 Node 的 `createBashResultDisplay` 按同一份事实（stdout = 头部、截断、输出文件）算出的
//    相同；成功的那个整份 metadata 与 Node 的 `completedToolPartMetadata` 给出的相同（Node 把失败的 Bash 存成 completed，rust 存成 error，
//    error 的 metadata 另有给模型的 `modelContent`）；
// 2. Node 的读法：`parseCompletedToolPartMetadata` 读出 display；v4 的冷读合成（`synthesizeEventsFromMessages`）的工具事件带它；
//    每条消息过 App 的 strict schema；
// 3. Node 用自己的写入代码续写一轮带 Bash display 的调用，rust-cli 冷读取的 `output_heads` 读回它，rust 写的两个不变。

import { appSchemaRejections } from "./node-interop-history.mjs";
import { importNode } from "./node-root.mjs";

/** Node 的 Bash 输出（`BashOutputSchema`）：头部是 stdout、截断了、输出文件是保留的那一个。 */
const bashOutput = (head) => ({
  stdout: head.head,
  stderr: "",
  stdoutTruncated: true,
  rawOutputPath: head.output_file,
  interrupted: false,
});

export async function checkOutputHead(ctx) {
  const { nodeRoot, store, dir, rust, expectEqual } = ctx;
  const { nodeRuntime, createMessageId, createPartId, createTurnId, persistence } = ctx.node;
  const { createBashResultDisplay } = await importNode(
    nodeRoot,
    "core/src/tool/executor/bash-result-display.ts",
  );
  const { completedToolPartMetadata } = await importNode(
    nodeRoot,
    "core/src/runtime/methods/tool-part-metadata.ts",
  );
  const { parseCompletedToolPartMetadata, COMPLETED_TOOL_PART_METADATA_SCHEMA_VERSION } =
    await importNode(nodeRoot, "contracts/src/tools/tool-result-metadata.ts");
  const { persistPendingToolPart } = await importNode(
    nodeRoot,
    "core/src/runtime/methods/tool-part-persistence.ts",
  );
  const { synthesizeEventsFromMessages } = await importNode(
    nodeRoot,
    "bootstrap/src/zcode-protocol-v4/transcript-hydration.ts",
  );

  const written = rust("output-head-write", dir);
  const sessionID = written.session;
  const [ok, failed] = written.heads;
  const messages = await store.messages({ sessionID });
  const reply = messages.find((message) => message.info.id === written.reply);
  const part = (call) => reply?.parts.find((p) => p.type === "tool" && p.callID === call);

  // 1. 与 Node 按同一份事实算出的形状相同。
  const okPart = part(ok.tool_use);
  const failedPart = part(failed.tool_use);
  expectEqual(
    "the stored success metadata is Node's completedToolPartMetadata",
    completedToolPartMetadata({
      success: true,
      display: createBashResultDisplay(bashOutput(ok.head)),
    }),
    okPart?.state.metadata,
  );
  expectEqual(
    "the stored failure metadata carries Node's display and schemaVersion",
    {
      status: "error",
      schemaVersion: COMPLETED_TOOL_PART_METADATA_SCHEMA_VERSION,
      display: createBashResultDisplay(bashOutput(failed.head)),
    },
    {
      status: failedPart?.state.status,
      schemaVersion: failedPart?.state.metadata?.schemaVersion,
      display: failedPart?.state.metadata?.display,
    },
  );

  // 2. Node 的读法与 App 的 strict schema。
  expectEqual(
    "Node parses the display of both parts",
    [okPart, failedPart].map((p) => parseCompletedToolPartMetadata(p?.state.metadata)?.display),
    [ok, failed].map((head) => createBashResultDisplay(bashOutput(head.head))),
  );
  const scheduled = synthesizeEventsFromMessages(messages, { sessionId: sessionID })
    .filter((event) => event.type === "tool_call_scheduled")
    .map((event) => [event.payload.toolCallId, event.payload.display?.output]);
  expectEqual("the v4 cold synthesis carries the heads", scheduled, [
    [ok.tool_use, ok.head.head],
    [failed.tool_use, failed.head.head],
  ]);
  expectEqual(
    "the App schema accepts the session",
    await appSchemaRejections(nodeRoot, store, [sessionID]),
    [],
  );

  // 3. Node 续写一轮带 Bash display 的调用（Node 的 pending part 与 settleToolResults 的 completed 形状），rust-cli 读回。
  const { runtime, selection } = await nodeRuntime(sessionID);
  const model = { providerId: selection.providerId, modelId: selection.modelId };
  const trace = { traceId: sessionID, turnId: createTurnId() };
  const askID = createMessageId();
  await persistence.persistUserPrompt.call(runtime, askID, "Node prints a lot", undefined, trace, {
    sourceCommandId: "node_cmd_output_head",
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
  const toolCall = { id: "call_node_head", name: "Bash", input: { command: "seq 100000" } };
  await persistPendingToolPart(runtime, {
    assistantMessageId: stepID,
    declarationIndex: 0,
    input: toolCall.input,
    partID,
    toolCall,
    traceContext: trace,
    model,
  });
  const nodeHead = { head: "node-line-0", output_file: "/tmp/zcode-bash-node/stdout.txt" };
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
      output: "<persisted-output>",
      title: toolCall.name,
      metadata: completedToolPartMetadata({
        success: true,
        display: createBashResultDisplay(bashOutput(nodeHead)),
      }),
      time: { start: created, end: Date.now() },
    },
  });
  const loaded = rust("read", dir, sessionID);
  expectEqual(
    "rust-cli reads back the heads of both sides",
    [ok, failed, { tool_use: toolCall.id, head: nodeHead }],
    loaded.output_heads,
  );
}
