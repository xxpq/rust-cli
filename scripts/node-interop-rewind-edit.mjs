// rust-cli 与 Node CLI 的会话互操作检查的第三十二段：编辑重发（docs/specs/rust-cli/session-lifecycle.md §10.53，protocol-v5.md §53，REWIND）。
//
// rust-cli 照会话 actor 的 `ReplaceInput` 写一个会话（node_interop `rewind-edit-write`：两轮，回退到第二轮的输入，再写新输入与回答；截断与新输入
// 两批带同一个命令 id）：
// 1. Node 的冷恢复（history hydrator 按 `session.revert`）读到的活动分支是「目标之前的行 + 新输入 + 回答」，与 rust-cli 的冷读取相同；
// 2. 新输入的 user 行带编辑的命令（`anchor.sourceCommandId`），回退游标的保留前缀是第一轮的两行、代数是 1。

export async function checkRewindEdit(ctx) {
  const { store, artifactStore, dir, rust, nodeHistory, rustText, expectEqual } = ctx;
  const written = rust("rewind-edit-write", dir);
  const sessionID = written.session;
  const session = await store.getSession(sessionID);
  const history = await nodeHistory(store, artifactStore, session, sessionID);
  const texts = (items) => items.map(({ role, text }) => ({ role, text }));
  const branch = [
    { role: "user", text: "first" },
    { role: "assistant", text: "one" },
    { role: "user", text: "second again" },
    { role: "assistant", text: "edited" },
  ];
  expectEqual("Node's active branch after an edit", texts(history), branch);
  const loaded = rust("read", dir, sessionID);
  expectEqual("rust-cli reads the same branch", texts(rustText(loaded.messages)), branch);
  expectEqual("the revert keeps the first round", session.revert.keptMessageIDs.length, 2);
  expectEqual("the revert generation", session.revert.branchGeneration, 1);
  const messages = await store.messages({ sessionID });
  const edited = messages.find((message) => message.info.id === written.edited);
  expectEqual(
    "the new input carries the edit's command",
    edited?.info.anchor?.sourceCommandId,
    written.command,
  );
}
