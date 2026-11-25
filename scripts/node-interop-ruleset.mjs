// rust-cli 与 Node CLI 的会话互操作检查的第六段之二：本机项目规则集里 hook 的权限更新与 `/add-dir` 的「记住」
// （docs/specs/rust-cli/permission-updates.md §2.3、§6）。
//
// rust-cli 按会话提交 deny 整份换掉、记住两个目录再去掉一个（node_interop `rules-facts`）；Node 读出同样的规则集，
// `additionalDirectories` 是它不认识的键、排在最后。Node 再按 hook 的更新加一条 ask（`applyPermissionUpdates`，它的
// `{...当前}` 保留这个键）并写回，rust-cli 读回规则与目录。
import { importNode } from "./node-root.mjs";

export async function checkRememberedRules({
  nodeRoot,
  store,
  rust,
  dir,
  written,
  projectID,
  expectEqual,
}) {
  const { applyPermissionUpdates } = await importNode(
    nodeRoot,
    "core/src/tool/executor/permission-rules.ts",
  );
  rust(
    "rules-facts",
    dir,
    written.sessions[0],
    JSON.stringify([
      {
        type: "permission_rules_replaced",
        behavior: "deny",
        rules: [{ tool: "Bash", content: "rm:*" }],
      },
      { type: "directories_remembered", directories: ["/Users/dev/extra", "/Users/dev/more"] },
      { type: "directories_forgotten", directories: ["/Users/dev/more"] },
    ]),
  );
  const remembered = await store.getProjectPermission(projectID);
  expectEqual("Node reads the rules and directories rust-cli kept", remembered, {
    version: 1,
    allow: [{ toolName: "Bash", ruleContent: "git status:*" }],
    deny: [{ toolName: "Bash", ruleContent: "rm:*" }],
    ask: [],
    additionalDirectories: ["/Users/dev/extra"],
  });
  expectEqual("the remembered directories come last", Object.keys(remembered), [
    "version",
    "allow",
    "deny",
    "ask",
    "additionalDirectories",
  ]);
  await store.saveProjectPermission({
    projectID,
    permission: applyPermissionUpdates(remembered, [
      {
        type: "addRules",
        behavior: "ask",
        rules: [{ toolName: "Bash", ruleContent: "git push:*" }],
      },
    ]),
  });
  expectEqual(
    "rust-cli reads the rules and directories after Node applies a hook update",
    rust("rules", dir, written.workspace),
    {
      allow: [{ tool: "Bash", content: "git status:*" }],
      deny: [{ tool: "Bash", content: "rm:*" }],
      ask: [{ tool: "Bash", content: "git push:*" }],
      additional_directories: ["/Users/dev/extra"],
    },
  );
}
