//! 信任记录的写入（protocol-v5.md §16、§59.2）：本链路唯一的工作任务，`mcp.trust.set` 与 `hooks.trustGrant` 共用。
//!
//! 写入要等 Node 的目录锁与文件 IO，actor 是整条链路唯一的 owner，不能等它：校验通过的命令交来一个做完就给出事件的 future，
//! 任务按到达顺序一条一条跑完，把事件发回 actor（ACK 在那里排，效果之后）。只有一个任务：两条命令并发时结果可能乱序回来，
//! 后到的那份写入之后的状态（MCP 的待信任声明、重读之后的 hooks 快照）反而是旧的；按顺序做，最后一份一定对应最后一次写入，
//! 不用计时器、不用版本号。

use futures_util::future::BoxFuture;
use rust_cli_base::Spawner;
use tokio::sync::mpsc;

use crate::server::Event;

/// actor 里的信任写入：工作任务的队列（第一条信任命令到时起）与在途的条数（`shutdown` 的回复等它们，03 §3.4）。
#[derive(Default)]
pub(crate) struct TrustWrites {
    jobs: Option<mpsc::UnboundedSender<BoxFuture<'static, Event>>>,
    pub(crate) in_flight: usize,
}

impl TrustWrites {
    /// 排一条写入：做完时它给出的事件回到 actor。事件到了之后调 [`Self::done`]。
    pub(crate) fn submit(
        &mut self,
        job: BoxFuture<'static, Event>,
        events: &mpsc::UnboundedSender<Event>,
        spawner: &Spawner,
    ) {
        let jobs = self
            .jobs
            .get_or_insert_with(|| start_worker(events.clone(), spawner));
        if jobs.send(job).is_ok() {
            self.in_flight += 1;
        }
    }

    /// 一条写入的事件已经回到 actor。
    pub(crate) fn done(&mut self) {
        self.in_flight -= 1;
    }
}

/// 按到达顺序一条一条跑，结果发回 actor。队列关了（actor 结束）时任务结束。
fn start_worker(
    events: mpsc::UnboundedSender<Event>,
    spawner: &Spawner,
) -> mpsc::UnboundedSender<BoxFuture<'static, Event>> {
    let (jobs, mut queue) = mpsc::unbounded_channel::<BoxFuture<'static, Event>>();
    drop(spawner.spawn("protocol-v5-trust-writes", async move {
        while let Some(job) = queue.recv().await {
            let _ = events.send(job.await);
        }
    }));
    jobs
}
