//! TUI 与 `-p` 不加载仓库的项目配置用 `plugins.dirs` 引入的插件（docs/zcode-protocol-v5/18-mcp-trust.md §11.5，
//! docs/specs/rust-cli/config.md §2.1）：真实进程 `zcode-rs`，临时 home、临时仓库、本地模型夹具、假凭据。App 形态的同一条规则
//! 由 `app_project_mcp.rs` 覆盖。
//!
//! 仓库的项目配置只用 `plugins.dirs` 引入仓库里的一个插件（不受信任的来源）；用户级配置引入用户自己的插件（对照）。两个插件
//! 各有一个标记 MCP 服务器与 SessionStart、UserPromptSubmit 两个 hook，都记进同一个标记日志。跑一轮之后：用户插件的服务器
//! 与两个 hook 都有，仓库插件的一样都没有，运行日志里有 `config.project_keys.ignored`（`keys` 里有 `plugins.dirs`；PROJECT-CONFIG 起
//! 项目文件读进来时只留扩展声明，docs/specs/rust-cli/config.md §1.1，以前是 `plugins.project_dirs.skipped`）。
//! 不出现的判定没有计时器：两个插件的服务器在同一批里开始连接，同一个事件的 hook 在同一批里跑（hooks 引擎并行起一批、
//! 等它们都结束之后才发模型请求）。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "集成测试：准备临时目录、找 node、在伪终端里起真实进程、检查写出的文件"
)]

mod sse;
mod system_env;

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use rust_cli_base::models::GLM_5_3;
use serde_json::{Value, json};
use system_env::KeepSystem as _;

/// 仓库里的插件目录（项目配置里写的相对路径，相对仓库根）。
const REPO_PLUGIN_DIR: &str = "./tools/kit";
/// 两个插件的名字（也是标记日志里服务器与 hook 的名字）。
const REPO_PLUGIN: &str = "repo-kit";
const USER_PLUGIN: &str = "user-kit";
/// 每个插件登记 hook 的两个事件。
const HOOK_EVENTS: [&str; 2] = ["SessionStart", "UserPromptSubmit"];
/// 项目文件里剔掉了键时运行日志里的事件名（config.md §1.1）。
const IGNORED_EVENT: &str = "config.project_keys.ignored";
/// 剔掉的项目层插件目录在这条日志的 `keys` 里的写法。
const IGNORED_DIRS: &str = "plugins.dirs";
/// 等一个条件的上限（存活的上限：正常不到几秒）。
const WAIT: Duration = Duration::from_secs(60);
/// 伪终端的一次读等多久：到了就再看一次进程外的条件（标记日志、模型夹具）。
const POLL: Duration = Duration::from_millis(50);

/// 测试进程的 PATH 里的 node（`pnpm test:rust-cli` 由 node 启动，PATH 里一定有）。
fn node() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("node"))
        .find(|candidate| candidate.is_file())
        .expect("node is not on PATH")
}

fn write(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, value.to_string()).unwrap();
}

/// 临时目录：home、仓库（进程 cwd）、用户自己的插件目录与标记日志。
struct Place {
    _dir: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
    /// 标记服务器每起一个进程记一行 `start <名字> <pid> <cwd>`（`tests/fixtures/mcp-counter.mjs`）；hook 每跑一次记一行
    /// `hook <插件> <事件>`。
    log: PathBuf,
}

impl Place {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let place = Self {
            home: base.join("home"),
            repo: base.join("repo"),
            log: base.join("markers.log"),
            _dir: dir,
        };
        std::fs::create_dir_all(&place.home).unwrap();
        std::fs::create_dir_all(place.repo.join(".git")).unwrap();
        let user_plugin = base.join("user-plugin");
        write(
            &place.repo.join(".zcode/config.json"),
            &json!({"plugins": {"dirs": [REPO_PLUGIN_DIR]}}),
        );
        write(
            &place.home.join(".zcode/cli/config.json"),
            &json!({"plugins": {"dirs": [user_plugin]}}),
        );
        place.write_plugin(&user_plugin, USER_PLUGIN);
        place.write_plugin(&place.repo.join(REPO_PLUGIN_DIR), REPO_PLUGIN);
        place
    }

    /// 一个插件：manifest、一个标记服务器、每个事件一个在标记日志里记一行的 hook。
    fn write_plugin(&self, dir: &Path, name: &str) {
        write(
            &dir.join(".zcode-plugin/plugin.json"),
            &json!({"name": name}),
        );
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-counter.mjs");
        write(
            &dir.join(".mcp.json"),
            &json!({"mcpServers": {"probe": {
                "type": "stdio", "command": node(), "args": [script, self.log, name]
            }}}),
        );
        let mut hooks = serde_json::Map::new();
        for event in HOOK_EVENTS {
            let command = format!("echo 'hook {name} {event}' >> '{}'", self.log.display());
            hooks.insert(
                event.to_owned(),
                json!([{"hooks": [{"type": "command", "command": command}]}]),
            );
        }
        write(&dir.join("hooks/hooks.json"), &json!({"hooks": hooks}));
    }

    fn lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// 插件 `name` 的标记服务器起过几次。
    fn starts_of(&self, name: &str) -> usize {
        self.lines()
            .iter()
            .filter(|line| line.split(' ').take(2).eq(["start", name]))
            .count()
    }

    /// 插件 `name` 的 hook 跑过的事件，按跑的先后。
    fn hooks_of(&self, name: &str) -> Vec<String> {
        self.lines()
            .iter()
            .filter_map(|line| line.strip_prefix(&format!("hook {name} ")))
            .map(str::to_owned)
            .collect()
    }

    /// 起 `zcode-rs` 的命令：cwd 是仓库，模型是本地夹具。
    fn command(&self, model_url: &str) -> Command {
        let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
        command
            .current_dir(&self.repo)
            .env_clear()
            .keep_system()
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("ZCODE_DATA_BASE_DIR", &self.home)
            .env("ZCODE_MODEL_BASE_URL", model_url)
            .env("ZCODE_API_KEY", "test-key")
            .env("ZCODE_MODEL", GLM_5_3);
        command
    }

    /// 一轮之后的结论：用户插件的服务器与两个 hook 都有（对照），仓库插件的一样都没有；运行日志记下了项目文件里剔掉的 `plugins.dirs`。
    fn assert_only_the_user_plugin_loaded(&self) {
        let lines = self.lines();
        assert_eq!(self.starts_of(USER_PLUGIN), 1, "{lines:?}");
        assert_eq!(self.hooks_of(USER_PLUGIN), HOOK_EVENTS, "{lines:?}");
        assert_eq!(self.starts_of(REPO_PLUGIN), 0, "{lines:?}");
        assert!(self.hooks_of(REPO_PLUGIN).is_empty(), "{lines:?}");
        let ignored = self.run_log_events(IGNORED_EVENT);
        assert_eq!(ignored.len(), 1, "{ignored:?}");
        assert_eq!(ignored[0]["level"], "info", "{ignored:?}");
        assert!(
            ignored[0]["context"]["keys"]
                .as_str()
                .is_some_and(|keys| keys.contains(IGNORED_DIRS)),
            "{ignored:?}"
        );
    }

    /// 运行日志（`<home>/.zcode/rust-cli/log/*.jsonl`，logging.md §7）里事件名为 `event` 的行。
    fn run_log_events(&self, event: &str) -> Vec<Value> {
        let dir = self.home.join(".zcode/rust-cli/log");
        std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
            .flat_map(|text| {
                text.lines()
                    .map(|line| serde_json::from_str::<Value>(line).unwrap())
                    .collect::<Vec<_>>()
            })
            .filter(|line| line["event"] == event)
            .collect()
    }
}

#[test]
fn print_mode_does_not_load_plugins_from_project_plugin_dirs() {
    let place = Place::new();
    let fixture = sse::serve(Vec::new());
    let output = place
        .command(&fixture.url)
        .args(["-p", "hi"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "done");
    // `-p` 发第一个请求之前等 MCP 服务器连完（ext.md §3.3.1），进程退出时两个插件的服务器若起过都已记下。
    place.assert_only_the_user_plugin_loaded();
}

#[test]
fn the_tui_does_not_load_plugins_from_project_plugin_dirs() {
    let place = Place::new();
    let fixture = sse::serve(Vec::new());
    let mut command = place.command(&fixture.url);
    command
        .env("TERM", "xterm-256color")
        .env("LANG", "en_US.UTF-8");
    let mut tui = Terminal::start(command);
    // 陌生仓库：先问整个文件夹的信任（workspace-trust.md §2.4）；`2` 选 `Yes, I trust this folder`。改动之前，信任之后仓库
    // 插件的服务器与 hook 照常起（18-mcp-trust.md §11.2）。
    tui.wait(0, "the trust dialog", |text| {
        shows(text, "Accessing workspace")
    });
    let from = tui.mark();
    tui.send(b"2");
    tui.wait(from, "the input box", |text| shows(text, "? keys"));
    let from = tui.mark();
    tui.send(b"hi\r");
    // 第一次发送建会话（SessionStart）、过 UserPromptSubmit、再发模型请求；服务器在信任之后就开始连。
    tui.wait(from, "the user plugin's server and the reply", |text| {
        place.starts_of(USER_PLUGIN) == 1
            && fixture.main_requests().len() == 1
            && shows(text, "done")
    });
    let from = tui.mark();
    tui.send(b"/exit");
    tui.wait(from, "the exit command", |text| shows(text, "Quit ZCode"));
    tui.send(b"\r");
    let (status, screen) = tui.finish();
    assert!(status.success(), "{screen:?}");
    place.assert_only_the_user_plugin_loaded();
}

/// 画面上有没有 `needle`：去掉控制序列与空白再比（空格不画，光标直接跳过去）。
fn shows(text: &str, needle: &str) -> bool {
    let squash = |text: &str| {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(ch) = chars.next() {
            match ch {
                // CSI：参数之后的终止字节在 `@`..=`~`；OSC：到 BEL 或 ST。
                '\x1b' => match chars.next() {
                    Some('[') => while chars.next().is_some_and(|ch| !('@'..='~').contains(&ch)) {},
                    Some(']') => while chars.next().is_some_and(|ch| ch != '\x07' && ch != '\\') {},
                    _ => {}
                },
                ch if ch.is_whitespace() => {}
                ch => out.push(ch),
            }
        }
        out
    };
    squash(text).contains(&squash(needle))
}

/// 伪终端：`zcode-rs` 起在从端上（stdin、stdout、stderr 都是它），进入真实终端。父进程读主端的输出，照真实终端回答
/// 主要设备属性与光标位置的查询（同 tui 的 `tests/suite/pty.rs`）。
struct Terminal {
    master: OwnedFd,
    child: Child,
    seen: Vec<u8>,
    /// 已经回答过的主要设备属性查询（`ESC [ c`）与光标位置查询（`ESC [ 6 n`）个数。
    answered: (usize, usize),
}

impl Terminal {
    fn start(mut command: Command) -> Self {
        use rustix::fs::{Mode, OFlags};
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        use rustix::termios::Winsize;
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let name = ptsname(&master, Vec::new()).unwrap();
        let slave = rustix::fs::open(
            name.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY,
            Mode::empty(),
        )
        .unwrap();
        let size = Winsize {
            ws_row: 30,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        rustix::termios::tcsetwinsize(&slave, size).unwrap();
        let io = || Stdio::from(slave.try_clone().unwrap());
        let child = command
            .stdin(io())
            .stdout(io())
            .stderr(io())
            .spawn()
            .unwrap();
        Self {
            master,
            child,
            seen: Vec::new(),
            answered: (0, 0),
        }
    }

    /// 到现在为止读到的字节数（[`Self::wait`] 的起点）。
    fn mark(&self) -> usize {
        self.seen.len()
    }

    /// 一边读输出（回答查询）一边等 `done`。`done` 只看第 `from` 个字节之后的输出（终端只重画变了的格子，之前的帧里
    /// 出现过的字不算），也可以同时看进程外的事实（标记日志、模型夹具）。
    fn wait(&mut self, from: usize, what: &str, done: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            let text =
                String::from_utf8_lossy(&self.seen[from.min(self.seen.len())..]).into_owned();
            if done(&text) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {text:?}"
            );
            self.read(POLL);
        }
    }

    /// 等子进程退出，再读完剩下的输出（从端还开着，读不到文件结束，读到停顿为止）。
    fn finish(&mut self) -> (ExitStatus, String) {
        let deadline = Instant::now() + WAIT;
        let status = loop {
            self.read(POLL);
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "the child did not exit");
        };
        while self.read(POLL) {}
        (status, String::from_utf8_lossy(&self.seen).into_owned())
    }

    /// 读一次；读到了返回真。
    fn read(&mut self, wait: Duration) -> bool {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let wait = Timespec {
            tv_sec: wait.as_secs() as _,
            tv_nsec: wait.subsec_nanos() as _,
        };
        let mut fds = [PollFd::new(&self.master, PollFlags::IN)];
        if !matches!(poll(&mut fds, Some(&wait)), Ok(n) if n > 0) {
            return false;
        }
        let mut chunk = [0u8; 4096];
        let Ok(read @ 1..) = rustix::io::read(&self.master, &mut chunk) else {
            return false;
        };
        self.seen.extend_from_slice(&chunk[..read]);
        let count = |pattern: &[u8]| {
            self.seen
                .windows(pattern.len())
                .filter(|window| *window == pattern)
                .count()
        };
        let (attributes, positions) = (count(b"\x1b[c"), count(b"\x1b[6n"));
        for _ in self.answered.0..attributes {
            self.send(b"\x1b[?62c");
        }
        for _ in self.answered.1..positions {
            self.send(b"\x1b[1;1R");
        }
        self.answered = (attributes, positions);
        true
    }

    fn send(&self, bytes: &[u8]) {
        rustix::io::write(&self.master, bytes).unwrap();
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
