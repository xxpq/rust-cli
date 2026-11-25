// 内核不认识部署形态：每条规则一个合成的违规，加上不该报的写法。
import assert from "node:assert/strict";
import test from "node:test";

import { checkDeploymentNeutral } from "./deployment.mjs";

const policy = {
  LAYERS: {
    session: "kernel",
    "kernel-api": "kernel",
    policy: "kernel",
    cli: "entry",
    "pg-store": "adapter",
  },
  DEPLOYMENT_NEUTRAL_LAYERS: ["kernel"],
  DEPLOYMENT_TERMS: [
    { id: "cloud", re: /cloud/i },
    { id: "tenant", re: /tenant/i },
  ],
  DEPLOYMENT_TERM_EXCEPTIONS: [],
  PORT_KIND_METHODS: [
    { id: "is-kind", re: /^fn is_\w+\(&self\) -> bool$/ },
    { id: "optional-port", re: /-> Option<&dyn / },
  ],
};
const source = (crate, file, text) => ({ crate, file: `crates/${crate}/src/${file}`, text });
const ports = (methods) => ({
  SessionStore: { methods: methods.map((signature) => ({ signature })) },
});

test("a kernel crate may not name the deployment in code or in a string", () => {
  const failures = checkDeploymentNeutral(
    policy,
    [
      source("session", "actor.rs", "fn run() {\n    let cloud = true;\n}\n"),
      source("policy", "rules.rs", 'const TEXT: &str = "cloud session rules are unavailable";\n'),
      source("session", "ids.rs", "struct TenantKey;\n"),
    ],
    ports([]),
  );
  assert.equal(failures.length, 3, failures.join("\n"));
  assert.match(failures[0], /crates\/session\/src\/actor\.rs:2: "cloud"/u);
  assert.match(failures[1], /crates\/policy\/src\/rules\.rs:1: "cloud"/u);
  assert.match(failures[2], /crates\/session\/src\/ids\.rs:1: "TenantKey"/u);
});

test("comments, tests, other layers and longer words are not vocabulary", () => {
  const failures = checkDeploymentNeutral(
    policy,
    [
      source("session", "actor.rs", "// 云端（cloud）的会话库\nfn run() {}\n"),
      source("session", "actor_tests.rs", "fn cloud_case() {}\n"),
      source("cli", "cloud.rs", "pub fn cloud_kernel() {}\n"),
      source("pg-store", "lib.rs", "fn tenant(&self) -> &str { todo!() }\n"),
      // 命令名里的 gcloud 不是这个词。
      source("policy", "spec.rs", 'const SPEC: &str = "gcloud compute";\n'),
    ],
    ports([]),
  );
  assert.deepEqual(failures, []);
});

test("an exception names its file and must be used", () => {
  const exempt = {
    ...policy,
    DEPLOYMENT_TERM_EXCEPTIONS: [
      { file: "crates/policy/src/hosts.rs", reason: "预批准的域名" },
      { file: "crates/policy/src/gone.rs", reason: "已经没有了" },
    ],
  };
  const failures = checkDeploymentNeutral(
    exempt,
    [source("policy", "hosts.rs", 'const HOSTS: [&str; 1] = ["cloud.google.com"];\n')],
    ports([]),
  );
  assert.equal(failures.length, 1, failures.join("\n"));
  assert.match(
    failures[0],
    /DEPLOYMENT_TERM_EXCEPTIONS: crates\/policy\/src\/gone\.rs .*dead entry/u,
  );
});

test("a port may not answer what kind of implementation it is", () => {
  const failures = checkDeploymentNeutral(
    policy,
    [],
    ports([
      "fn is_shared(&self) -> bool",
      "fn input_journal(&self) -> Option<&dyn crate::input_journal::InputJournal>",
      // 带参数的判断与普通的查询不是“种类”方法。
      "fn reconnecting(&self,name:&MachineName) -> bool",
      "async fn load(&self,session:&SessionId) -> Result<LoadedSession,StoreError>",
    ]),
  );
  assert.equal(failures.length, 2, failures.join("\n"));
  assert.match(failures[0], /SessionStore: `fn is_shared\(&self\) -> bool`.*is-kind/u);
  assert.match(failures[1], /SessionStore: .*optional-port/u);
});
