// rust-cli 与 Node CLI 的会话互操作检查的第三十四段：v5 的 fork（FORK-V5，docs/specs/rust-cli/session-lifecycle.md §10.57，protocol-v5.md §55）。
//
// rust-cli 从第一轮的回答分到调用方给的 id（node_interop `fork-as-write`：第一轮里 ExitPlanMode 拿出一份计划），再用同一个 id 分一次（重发）：
// 1. 第二次是「已有」，库里只有一份新会话（不重复写行与消息）；
// 2. Node 读出会话行（parent、fork）与历史（正好是第一轮），用自己的写入代码续写，rust-cli 读回；
// 3. 计划随 fork 进了新会话（本工程的 entry，Node 不读），rust-cli 冷读得到它。
//
// 第三十五段：v5 的侧聊（SIDECHAT-V5，session-lifecycle.md §10.58，protocol-v5.md §57）。侧聊是另一种 v5 的分叉，与 fork 放在一个文件里。
// rust-cli 从两轮问答的父会话分出侧聊到调用方给的 id（node_interop `side-chat-write`），再用同一个 id 分一次（重发）：
// 1. 第二次是「已有」，库里只有一份侧聊（不重复写行与消息）；
// 2. Node 读出会话行（parent、selection_side_chat），继承的消息是只给模型看的，末尾是 Node 自己认得的侧聊边界；
// 3. Node 的历史与 rust-cli 冷读的同一份（侧聊边界两边都是提醒，不比）；Node 用自己的写入代码续写，rust-cli 读回、种类仍是侧聊。

/** rust-cli 冷读取里的分叉提示之外的消息。 */
const withoutNotice = (messages) =>
  messages.filter((message) => !(message.type === "attachment" && message.source === "fork"));

export async function checkForkAs(ctx) {
  const { store, artifactStore, dir, rust, nodeHistory, rustText, expectEqual } = ctx;
  const { nodeRuntime, persistence, createMessageId, createPartId, createTurnId } = ctx.node;
  const written = rust("fork-as-write", dir);
  expectEqual("the second fork into the same id writes nothing", written.receipts, [
    "created",
    "existing",
  ]);
  const child = await store.getSession(written.child);
  expectEqual(
    "Node reads the fork row the caller named",
    child && [child.id, child.parentID, child.taskType],
    [written.child, written.parent, "fork"],
  );
  const rows = await store.messages({ sessionID: written.child });
  expectEqual(
    "one copy of each message",
    new Set(rows.map((row) => row.info.id)).size,
    rows.length,
  );
  const parent = rust("read", dir, written.parent);
  const end = parent.messages.findIndex((message) => message.id === written.firstTurn[1]) + 1;
  const loaded = rust("read", dir, written.child);
  expectEqual(
    "the fork keeps the first turn",
    rustText(withoutNotice(loaded.messages)),
    rustText(parent.messages.slice(0, end)),
  );
  expectEqual(
    "Node reads the same history",
    await nodeHistory(store, artifactStore, child, written.child),
    rustText(withoutNotice(loaded.messages)),
  );
  expectEqual(
    "the plan goes with the fork",
    loaded.plans.map((plan) => [plan.tool_use, plan.plan]),
    [[written.planCall, written.plan]],
  );
  // Node 续写这个新会话，rust-cli 读回。
  const { runtime, selection } = await nodeRuntime(written.child);
  const trace = { traceId: written.child, turnId: createTurnId() };
  const userID = createMessageId();
  await persistence.persistUserPrompt.call(
    runtime,
    userID,
    "Node continues the v5 fork",
    undefined,
    trace,
    { sourceCommandId: "node_cmd_fork_as" },
  );
  const assistantID = createMessageId();
  const created = Date.now();
  await persistence.persistAssistantMessage.call(
    runtime,
    assistantID,
    userID,
    created,
    { completed: Date.now(), finish: "stop" },
    trace,
    { providerId: selection.providerId, modelId: selection.modelId },
  );
  await runtime.persistPart({
    id: createPartId(),
    sessionID: written.child,
    messageID: assistantID,
    type: "text",
    text: "Node answers in the v5 fork",
    time: { start: created, end: Date.now() },
  });
  const continued = rust("read", dir, written.child);
  expectEqual(
    "rust-cli reads Node's continuation of the v5 fork",
    continued.messages.slice(-2).map((message) => message.id),
    [userID, assistantID],
  );
  expectEqual(
    "the plan stays after Node continued",
    continued.plans.map((plan) => plan.tool_use),
    [written.planCall],
  );
}

/** 侧聊边界的来源（kernel-api `store::side_chat::SOURCE`，Node `selection_side_chat`）。 */
const SIDE_CHAT = "selection_side_chat";

/** rust-cli 冷读取里侧聊边界之外的消息。 */
const withoutBoundary = (messages) =>
  messages.filter((message) => !(message.type === "attachment" && message.source === SIDE_CHAT));

export async function checkSideChat(ctx) {
  const { store, artifactStore, dir, rust, nodeHistory, rustText, expectEqual } = ctx;
  const { nodeRuntime, persistence, createMessageId, createPartId, createTurnId } = ctx.node;
  const written = rust("side-chat-write", dir);
  expectEqual("the second side chat into the same id writes nothing", written.receipts, [
    "created",
    "existing",
  ]);
  const child = await store.getSession(written.child);
  expectEqual(
    "Node reads the side chat row the caller named",
    child && [child.id, child.parentID, child.taskType],
    [written.child, written.parent, SIDE_CHAT],
  );
  const rows = await store.messages({ sessionID: written.child });
  expectEqual(
    "one copy of each message",
    new Set(rows.map((row) => row.info.id)).size,
    rows.length,
  );
  expectEqual(
    "the inherited messages are for the model only and the boundary is last",
    rows.map((row) => [row.info.visibility, row.info.semantics?.uiVisibility, row.info.source]),
    [
      ...rows.slice(0, -1).map(() => ["model-only", "hidden", undefined]),
      ["model-only", "hidden", SIDE_CHAT],
    ],
  );
  const parent = rust("read", dir, written.parent);
  const loaded = rust("read", dir, written.child);
  expectEqual("rust-cli reads a side chat", loaded.meta.kind, "side_chat");
  expectEqual(
    "the side chat keeps the parent's whole history",
    rustText(withoutBoundary(loaded.messages)),
    rustText(parent.messages),
  );
  expectEqual(
    "Node reads the same history",
    await nodeHistory(store, artifactStore, child, written.child),
    rustText(withoutBoundary(loaded.messages)),
  );
  // Node 续写这个侧聊，rust-cli 读回。
  const { runtime, selection } = await nodeRuntime(written.child);
  const trace = { traceId: written.child, turnId: createTurnId() };
  const userID = createMessageId();
  await persistence.persistUserPrompt.call(
    runtime,
    userID,
    "Node asks in the side chat",
    undefined,
    trace,
    { sourceCommandId: "node_cmd_side_chat" },
  );
  const assistantID = createMessageId();
  const created = Date.now();
  await persistence.persistAssistantMessage.call(
    runtime,
    assistantID,
    userID,
    created,
    { completed: Date.now(), finish: "stop" },
    trace,
    { providerId: selection.providerId, modelId: selection.modelId },
  );
  await runtime.persistPart({
    id: createPartId(),
    sessionID: written.child,
    messageID: assistantID,
    type: "text",
    text: "Node answers in the side chat",
    time: { start: created, end: Date.now() },
  });
  const continued = rust("read", dir, written.child);
  expectEqual(
    "rust-cli reads Node's continuation of the side chat",
    continued.messages.slice(-2).map((message) => message.id),
    [userID, assistantID],
  );
  expectEqual("the continued side chat is still a side chat", continued.meta.kind, "side_chat");
}
