// rust-cli 与 Node CLI 的会话互操作检查的第二十段：压缩前已加载的工具（docs/specs/rust-cli/compact.md §3.5，
// 工作线 F82）。
//
// rust-cli 写的压缩会话（第四段的那个，node_interop `write` 的第 4 个会话）的边界带着压缩前已加载的工具：
// 1. 存在 `compaction` part 的 `metadata.rustCli.preCompactDiscoveredTools`，Node 读得出；`compactBoundary` 照样过
//    Node 的 strict schema（`parseCompactBoundaryPayload`），里面没有这一项；
// 2. App：每条消息过 App 的 strict schema；`mapMessageWithParts` 给 App 的 `compaction` part 的 metadata 不带它；
// 3. rust-cli 冷读取把它还原到边界上（`discovered_tools`）。
// 4. Node 的 v4 冷读（`synthesizeEventsFromMessages`）合成出一个压缩标记（重做线 EXP-DEFECTS-5，compact.md §6）：rust 照 Node 另写
//    时间线记录（timeline_event 的 assistant，带 `context_compaction` 的 timeline part 与带 `timelineStatus` 的 compaction part）。
//    以前只有摘要那条消息，v4 冷读不出标记，rust 下压缩过的会话回到 Node 之后时间线上看不出压缩过。
import { appSchemaRejections } from "./node-interop-history.mjs";
import { importNode } from "./node-root.mjs";

const CARRIED = ["WebFetch", "mcp__fixture__lookup"];
/** Node 的压缩生命周期事件（`SessionEventType` 的 compact_started / completed / failed）：v4 冷读据此画压缩标记。 */
const COMPACT_LIFECYCLE = new Set(["compact_started", "compact_completed", "compact_failed"]);

export async function checkCompactCarried(ctx) {
  const { nodeRoot, store, dir, rust, sessionID, expectEqual } = ctx;
  const { parseCompactBoundaryPayload } = await importNode(
    nodeRoot,
    "contracts/src/compact/index.ts",
  );
  const messages = await store.messages({ sessionID });
  // 带边界的那一个：时间线记录里另有一个不带边界的 compaction part（同 Node）。
  const compaction = messages
    .flatMap((message) => message.parts)
    .find((part) => part.type === "compaction" && part.compactBoundary);
  expectEqual(
    "Node reads the tools loaded before compaction",
    compaction?.metadata?.rustCli?.preCompactDiscoveredTools,
    CARRIED,
  );
  const payload = parseCompactBoundaryPayload(compaction?.compactBoundary);
  expectEqual(
    "the strict compactBoundary does not carry them",
    "preCompactDiscoveredTools" in payload,
    false,
  );
  expectEqual(
    "the App schema accepts the compacted session",
    await appSchemaRejections(nodeRoot, store, [sessionID]),
    [],
  );
  const { mapMessageWithParts } = await importNode(
    nodeRoot,
    "bootstrap/src/zcode-protocol/message-mapper.ts",
  );
  const mapped = messages
    .map(mapMessageWithParts)
    .flatMap((message) => message.parts)
    .find((part) => part.type === "compaction" && part.compactBoundary);
  expectEqual(
    "the App's compaction part leaves them out",
    "rustCli" in (mapped?.metadata ?? {}),
    false,
  );
  const { synthesizeEventsFromMessages } = await importNode(
    nodeRoot,
    "bootstrap/src/zcode-protocol-v4/transcript-hydration.ts",
  );
  const markers = synthesizeEventsFromMessages(messages, { sessionId: sessionID })
    .filter((event) => COMPACT_LIFECYCLE.has(event.type))
    .map(({ type, payload: event }) => ({
      type,
      status: event.status,
      trigger: event.trigger,
      boundaryId: event.boundaryId,
      summaryMessageId: event.summaryMessageId,
      tailStartMessageId: event.tailStartMessageId,
    }));
  // 压缩之前最后一条对话消息是保留段的尾行（这个会话以工具结果结尾，映射成它的 assistant 行）。
  expectEqual("Node's v4 cold read shows the compaction marker", markers, [
    {
      type: "compact_completed",
      status: "completed",
      trigger: payload.trigger,
      boundaryId: payload.boundaryId,
      summaryMessageId: payload.summaryMessageIds[0],
      tailStartMessageId: payload.preservedSegment?.tailMessageId,
    },
  ]);
  const loaded = rust("read", dir, sessionID);
  expectEqual(
    "rust-cli reads them back onto the boundary",
    loaded.messages[0]?.discovered_tools,
    CARRIED,
  );
}
