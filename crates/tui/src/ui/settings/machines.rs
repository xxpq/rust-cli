//! 设置的 Machines 段（docs/specs/rust-cli/tui-kit.md §4.2「设置」；照抄 Puffin `shell/settings/machines.rs`，两套界面不能
//! 互相依赖）：远端机器的清单、每台一页、添加页。界面自己的段，不经 runner 读：读 `/machines` 同一份快照（`app.machines`），
//! 操作经机器服务（[`super::super::machines::MachineRequest`]）。改 SSH 目标或工作目录 = 删掉再加（清单只有加与删）。
//!
//! 与 Puffin 的不同：旧界面不记连接走过的步，分步只有探测、这一步与握手（Puffin 另有界面看见的中间步）。
use rust_cli_tui_kit::app::widgets::machine_rows::Phase;
use rust_cli_tui_kit::app::widgets::settings::{Applies, Ink, Part};

use super::machine_words as words;
use super::page::{Act, Ctx, ItemRow, Page, Row, TextRow, View};
use crate::machines::{ConnectStep, MachineFailure, MachineRow, MachineStatus};
use crate::ui::i18n::Lang;
use crate::ui::i18n_machines as said;

/// 添加页的表单的键。
pub const SSH: &str = "nm.ssh";
pub const NAME: &str = "nm.name";
pub const DIR: &str = "nm.dir";

pub fn needs_target(l: Lang) -> &'static str {
    l.pick("Needs an SSH target.", "需要一个 SSH 目标。")
}

fn connects(l: Lang) -> &'static str {
    l.pick(
        "Checks the machine, uploads the executor if needed, and connects.",
        "检查机器，需要时上传执行器，然后连接。",
    )
}

/// 机器页的动作行的名字（随状态，[`words::action_word`] 的这种语言、首字母大写的写法）。
fn action_label(l: Lang, status: &MachineStatus) -> String {
    match l {
        Lang::En => {
            let word = words::action_word(status);
            format!("{}{}", word[..1].to_uppercase(), &word[1..])
        }
        Lang::Zh => match status {
            MachineStatus::Ready => "断开",
            MachineStatus::Reconnecting { .. } => "立即重试",
            MachineStatus::Connecting(_) => "停止",
            MachineStatus::Failed(_) => "再试一次",
            MachineStatus::Disconnected => "连接",
        }
        .to_owned(),
    }
}

/// 机器页的说明：这台的处境（[`words::situation`] 的这种语言）。
fn situation(l: Lang, name: &str, ssh: &str, status: &MachineStatus) -> String {
    match (l, status) {
        (Lang::En, _) => words::situation(name, ssh, status),
        (
            Lang::Zh,
            MachineStatus::Failed(MachineFailure::Unreachable(m) | MachineFailure::Lost(m)),
        ) => format!(
            "{name} 连不上：{}。看看它开着没有、在终端里 ssh {ssh} 能不能用。",
            said::explain(l, m).trim_end_matches('。')
        ),
        (Lang::Zh, MachineStatus::Failed(other)) => said::failure_text(l, name, other),
        (Lang::Zh, MachineStatus::Reconnecting { reason, .. }) if !reason.is_empty() => format!(
            "{name} 断了连接，正在重连：{}。",
            said::explain(l, reason).trim_end_matches('。')
        ),
        _ => String::new(),
    }
}

/// 对一台机器的操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineOp {
    Connect,
    Disconnect,
    Remove,
    Retry,
}

/// ⏎（列表）与动作（机器页）随状态做的事（Puffin `enter_op`）。
pub fn enter_op(status: &MachineStatus) -> MachineOp {
    match status {
        MachineStatus::Ready | MachineStatus::Connecting(_) => MachineOp::Disconnect,
        MachineStatus::Reconnecting { .. } => MachineOp::Retry,
        MachineStatus::Failed(_) | MachineStatus::Disconnected => MachineOp::Connect,
    }
}

/// 这次连接的分步：探测、这一步，没到握手时握手在最后（Puffin `steps`，没有界面看见的中间步）。
fn steps(now: ConnectStep) -> Vec<(ConnectStep, Phase)> {
    let mut out: Vec<(ConnectStep, Phase)> = Vec::new();
    if now != ConnectStep::Checking {
        out.push((ConnectStep::Checking, Phase::Done));
    }
    out.push((now, Phase::Now));
    if now != ConnectStep::Handshaking {
        out.push((ConnectStep::Handshaking, Phase::Later));
    }
    out
}

/// 机器的操作（[`Act::Machine`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MachineAct {
    /// 连接、断开、立即重试、删除：`word` 是键说的。
    Op {
        op: MachineOp,
        name: String,
        word: &'static str,
    },
    /// 改 SSH 目标或工作目录：删掉再按新的值加，`reconnect` 时再连。
    Edit {
        name: String,
        ssh: String,
        directory: Option<String>,
        reconnect: bool,
    },
    /// 按添加页的表单添加并连上。
    Add,
}

impl MachineAct {
    /// 状态栏的键说的。
    pub fn word(&self) -> &'static str {
        match self {
            MachineAct::Op { word, .. } => word,
            MachineAct::Edit { .. } => "save",
            MachineAct::Add => "add",
        }
    }
}

/// 空格随状态（spec §11.1）：没连与失败的连接，其余断开（连接中说 `stop`）。
fn flip(m: &MachineRow) -> MachineAct {
    let (op, word) = match m.status {
        MachineStatus::Disconnected | MachineStatus::Failed(_) => (MachineOp::Connect, "connect"),
        MachineStatus::Connecting(_) => (MachineOp::Disconnect, "stop"),
        MachineStatus::Ready | MachineStatus::Reconnecting { .. } => {
            (MachineOp::Disconnect, "disconnect")
        }
    };
    MachineAct::Op {
        op,
        name: m.name.clone(),
        word,
    }
}

/// 一台的状态（清单的状态列与机器页的 Status）。
fn status_parts(l: Lang, m: &MachineRow, since: Option<u64>, now_ms: u64) -> Vec<Part> {
    let dot = || Part::new(" · ", Ink::Line);
    let list = said::list_words(l);
    match &m.status {
        MachineStatus::Ready => {
            let mut out = vec![Part::new(l.pick("connected", "已连接"), Ink::Fg)];
            if let Some(ms) = m.latency_ms {
                out.extend([dot(), Part::new(format!("{ms}ms"), Ink::Fg3)]);
            }
            out
        }
        MachineStatus::Reconnecting { retry_at_ms, .. } => {
            let mut out = vec![Part::new(said::reconnecting(l), Ink::Warn)];
            if let Some(next) =
                retry_at_ms.and_then(|at| said::next_try(l, Some(at.saturating_sub(now_ms))))
            {
                out.extend([dot(), Part::new(next, Ink::Fg3)]);
            }
            out
        }
        MachineStatus::Connecting(step) => {
            let since = since.unwrap_or(now_ms);
            vec![
                Part::new(said::step_word(l, *step), Ink::Fg2),
                dot(),
                Part::new(words::elapsed(now_ms.saturating_sub(since)), Ink::Fg3),
            ]
        }
        MachineStatus::Failed(_) => vec![Part::new(list.could_not_connect, Ink::Err)],
        MachineStatus::Disconnected => vec![Part::new(list.not_connected, Ink::Fg3)],
    }
}

/// 连接中的分步（同列表，spec §11.1）。
fn step_parts(l: Lang, m: &MachineRow) -> Vec<Part> {
    let MachineStatus::Connecting(now) = m.status else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, (step, phase)) in steps(now).into_iter().enumerate() {
        if i > 0 {
            out.push(Part::new("   ", Ink::Fg3));
        }
        let (mark, ink) = match phase {
            Phase::Done => (Some(("✓ ", Ink::Ok)), Ink::Fg3),
            Phase::Now => (Some(("○ ", Ink::Warn)), Ink::Fg2),
            Phase::Later => (None, Ink::Quiet),
        };
        if let Some((text, ink)) = mark {
            out.push(Part::new(text, ink));
        }
        out.push(Part::new(
            said::step_done(l, step, phase == Phase::Done),
            ink,
        ));
    }
    out
}

/// 段自己的行：每台一项，最后 `+ Add a machine`。没有机器服务时说不可用。
pub fn machines(ctx: &Ctx<'_>) -> Vec<Row> {
    let l = ctx.lang;
    let Some(all) = ctx.machines else {
        return vec![Row::Note(said::unavailable(l).to_owned())];
    };
    let mut rows: Vec<Row> = all
        .iter()
        .map(|m| {
            let run = ctx.connecting.get(&m.name).copied();
            let desc = match &m.status {
                MachineStatus::Failed(f) => match l {
                    Lang::En => format!("{} ⏎ opens it.", said::cause(l, &m.name, f)),
                    Lang::Zh => format!("{}⏎ 打开它。", said::cause(l, &m.name, f)),
                },
                MachineStatus::Reconnecting { reason, .. } if !reason.is_empty() => {
                    said::last_error(l, reason)
                }
                _ => String::new(),
            };
            Row::item(ItemRow {
                key: format!("machine:{}", m.name),
                name: m.name.clone(),
                toggle: Some(m.status != MachineStatus::Disconnected),
                scope: m.ssh.clone(),
                status: status_parts(l, m, run, ctx.now_ms),
                desc,
                open: Some(Act::Push(Page::Machine(m.name.clone()))),
                flip: Some(Act::Machine(flip(m))),
                detail: step_parts(l, m),
                remove: None,
                enter: String::new(),
                words: String::new(),
            })
        })
        .collect();
    if all.is_empty() {
        rows.push(Row::Note(
            l.pick("No machines yet.", "还没有机器。").to_owned(),
        ));
    }
    rows.push(Row::plus(
        "machine:+",
        said::list_words(l).add,
        "",
        Act::Push(Page::NewMachine),
    ));
    rows
}

/// 工作目录的一行（机器页与添加页）：空时 `not set` 接 `remote home`。
fn directory_row(
    l: Lang,
    key: &str,
    value: &str,
    save: impl Fn(&str) -> Act + Send + Sync + 'static,
) -> Row {
    let mut t = TextRow::new(
        key,
        l.pick("Working directory", "工作目录"),
        value,
        "",
        Applies::Form,
        save,
    );
    t.placeholder = "/path/on/the/machine".to_owned();
    if value.is_empty() {
        t.note = said::remote_home(l).to_owned();
    }
    Row::Text(t)
}

/// 一台机器的页（spec §11.1）：处境、Status、可改的 SSH 目标与工作目录、系统与执行器、随状态的动作、删除（先问）。
pub fn machine(name: &str, ctx: &Ctx<'_>) -> View {
    let l = ctx.lang;
    let Some(m) = ctx
        .machines
        .and_then(|all| all.iter().find(|m| m.name == name))
    else {
        return View::loading(l, name);
    };
    let run = ctx.connecting.get(&m.name).copied();
    let reconnect = m.status != MachineStatus::Disconnected;
    let dir = m.working_directory.clone().unwrap_or_default();
    let mut ssh = {
        let (name, dir) = (m.name.clone(), m.working_directory.clone());
        TextRow::new(
            "m.ssh",
            ssh_target(l),
            m.ssh.clone(),
            "",
            Applies::Form,
            move |v| {
                let v = v.trim();
                if v.is_empty() {
                    Act::Refuse(needs_target(l).to_owned())
                } else {
                    Act::Machine(MachineAct::Edit {
                        name: name.clone(),
                        ssh: v.to_owned(),
                        directory: dir.clone(),
                        reconnect,
                    })
                }
            },
        )
    };
    ssh.hint = target_hint(l).to_owned();
    let directory = {
        let (name, target) = (m.name.clone(), m.ssh.clone());
        directory_row(l, "m.dir", &dir, move |v| {
            let v = v.trim();
            Act::Machine(MachineAct::Edit {
                name: name.clone(),
                ssh: target.clone(),
                directory: (!v.is_empty()).then(|| v.to_owned()),
                reconnect,
            })
        })
    };
    let system = match &m.executor {
        Some(e) => vec![Part::new(
            format!("{} {}", words::os_name(&e.os), e.arch),
            Ink::Fg,
        )],
        None => vec![Part::italic(
            l.pick("known once it connects", "连上后才知道"),
            Ink::Fg3,
        )],
    };
    let mut rows = vec![
        Row::info(
            l.pick("Status", "状态"),
            status_parts(l, m, run, ctx.now_ms),
        ),
        Row::Text(ssh),
        directory,
        Row::info(l.pick("System", "系统"), system),
    ];
    if let Some(e) = &m.executor {
        let mut parts = vec![Part::new(e.version.clone(), Ink::Fg)];
        if !e.refreshes_env {
            parts.push(Part::new(
                format!("  {}", said::env_not_refreshed(l)),
                Ink::Fg3,
            ));
        }
        rows.push(Row::info(l.pick("Executor", "执行器"), parts));
    }
    let word = words::action_word(&m.status);
    let label = action_label(l, &m.status);
    let op = enter_op(&m.status);
    let desc = if op == MachineOp::Connect {
        connects(l)
    } else {
        ""
    };
    rows.push(Row::action(
        "m.do",
        label,
        desc,
        Act::Machine(MachineAct::Op {
            op,
            name: m.name.clone(),
            word,
        }),
    ));
    let remove = match l {
        Lang::En => format!("Remove {}", m.name),
        Lang::Zh => format!("删除 {}", m.name),
    };
    rows.push(Row::danger(
        "m.remove",
        &remove,
        Act::Confirm {
            label: l.pick("Remove", "删除").to_owned(),
            note: l
                .pick(
                    "This can't be undone. Nothing on the machine is deleted.",
                    "删了就恢复不了。机器上的东西不会删。",
                )
                .to_owned(),
            next: Box::new(Act::Machine(MachineAct::Op {
                op: MachineOp::Remove,
                name: m.name.clone(),
                word: "remove",
            })),
        },
    ));
    View::new(
        vec![m.name.clone()],
        situation(l, &m.name, &m.ssh, &m.status),
        rows,
    )
}

fn ssh_target(l: Lang) -> &'static str {
    l.pick("SSH target", "SSH 目标")
}

fn target_hint(l: Lang) -> &'static str {
    l.pick(
        "a Host from ~/.ssh/config, or user@host",
        "~/.ssh/config 里的一个 Host，或 user@host",
    )
}

/// 添加页（spec §11.1）：SSH 目标、名字（没填时从目标取）、工作目录，`Add and connect`；`~/.ssh/config` 里还没添加的主机。
pub fn new_machine(ctx: &Ctx<'_>) -> View {
    let l = ctx.lang;
    let form = |k: &str| {
        ctx.form
            .get(k)
            .map(|v| v.trim().to_owned())
            .unwrap_or_default()
    };
    let set = |k: &'static str| {
        move |v: &str| Act::SetForm {
            key: k.to_owned(),
            value: v.trim().to_owned(),
        }
    };
    let (target, given) = (form(SSH), form(NAME));
    let mut ssh = TextRow::new(
        SSH,
        ssh_target(l),
        target.clone(),
        "",
        Applies::Form,
        set(SSH),
    );
    ssh.hint = target_hint(l).to_owned();
    ssh.placeholder = "user@host".to_owned();
    let derived = words::name_from(&target);
    let mut name = TextRow::new(
        NAME,
        l.pick("Name", "名字"),
        if given.is_empty() {
            derived.clone()
        } else {
            given.clone()
        },
        "",
        Applies::Form,
        set(NAME),
    );
    if given.is_empty() {
        let from = l.pick("from the target", "取自目标").to_owned();
        if derived.is_empty() {
            name.empty = from;
        } else {
            name.note = from;
        }
    }
    let mut rows = vec![
        Row::Text(ssh),
        Row::Text(name),
        directory_row(l, DIR, &form(DIR), set(DIR)),
    ];
    let mut add = Row::action(
        "nm.add",
        l.pick("Add and connect", "添加并连接"),
        "",
        Act::Machine(MachineAct::Add),
    );
    if let Row::Action { off, .. } = &mut add {
        *off = target.is_empty();
    }
    rows.push(add);
    if target.is_empty() {
        rows.push(Row::Note(needs_target(l).to_owned()));
    }
    let added = |alias: &str| {
        ctx.machines
            .is_some_and(|all| all.iter().any(|m| m.ssh == alias || m.name == alias))
    };
    let hosts: Vec<_> = ctx.ssh_hosts.iter().filter(|h| !added(&h.alias)).collect();
    if !hosts.is_empty() {
        rows.push(Row::Heading(
            l.pick("From ~/.ssh/config", "来自 ~/.ssh/config")
                .to_owned(),
        ));
        for h in hosts {
            let mut row = Row::plus(
                &format!("nm.host:{}", h.alias),
                &h.alias,
                "",
                Act::SetForm {
                    key: SSH.to_owned(),
                    value: h.alias.clone(),
                },
            );
            if let Row::Action { aside, .. } = &mut row {
                aside.clone_from(&h.target);
            }
            rows.push(row);
        }
    }
    View::new(
        vec![l.pick("New machine", "新机器").to_owned()],
        l.pick(
            "Paste the SSH target: a Host from ~/.ssh/config or user@host. ZCode copies its executor there when it first connects (Linux x86_64 / aarch64 or macOS arm64).",
            "粘贴 SSH 目标：~/.ssh/config 里的一个 Host，或 user@host。第一次连接时 ZCode 把它的执行器复制过去（Linux x86_64 / aarch64 或 macOS arm64）。",
        ),
        rows,
    )
}
