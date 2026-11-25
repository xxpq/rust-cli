//! 会话的策略：随部署形态变的几处行为，由组装根算一次、注入内核（architecture.md §3）。
//!
//! 内核只读这些字段，不去问会话库或执行环境“你是哪一种”。每个字段说的是**做什么**，不是**在哪里跑**；缺省值是一个进程
//! 独占一个本机会话库时的行为。哪种部署取哪个值写在组装根（cli）里。

use serde::{Deserialize, Serialize};

/// 恢复一个已有的会话（`StartSource::Resume`）时怎样启动。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeStart {
    /// 打开就启动：跑 SessionStart，队列照常推进。
    #[default]
    Immediate,
    /// 等用户的下一条输入：在那之前不跑 SessionStart，账本里待处理的输入留在队列里不自动执行（pg-store.md §4）。
    OnNextInput,
}

/// bash 模式（`!` 命令）的输入消息什么时候提交。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashInputCommit {
    /// 命令跑完后与输出一起提交（被中断时的顺序是说明、输入、中断提示）。
    #[default]
    WithOutput,
    /// 命令开始前先提交：命令可能已经有副作用，重开会话后不能再把这条输入当成没执行过（被中断时输入在说明之前）。
    BeforeRun,
}

/// 会话自己的权限规则（会话行里的规则集）什么时候读。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRulesRead {
    /// 打开会话时读一次：只有本进程会改它。
    #[default]
    AtOpen,
    /// 每次判决前现读：别的进程随时可能改它（deployment.md §9）。
    EachDecision,
}

/// 回答一个脱离了会话的询问（会话已经关掉，答案之后才来）时，提交之前要不要先占住会话表里这个会话的位置。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetachedAnswer {
    /// 直接提交。
    #[default]
    Direct,
    /// 先占位：这期间同一个会话不能被打开；它正开着时回答失败。
    HoldSession,
}

/// 删除一个开着的会话不在这里：两种部署都先关掉它的 actor 再删（runtime 的 `delete`，session-facts.md §5）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPolicy {
    pub resume: ResumeStart,
    pub bash_input: BashInputCommit,
    pub session_rules: SessionRulesRead,
    pub detached_answer: DetachedAnswer,
}
