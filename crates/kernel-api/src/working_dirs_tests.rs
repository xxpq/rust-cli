use super::*;
use crate::env::EnvFacts;
use crate::ids::EnvironmentId;
use crate::testing::MemoryEnvironment;

fn path(text: &str) -> EnvPath {
    EnvPath::new(text)
}

#[test]
fn covering_says_which_working_directory_holds_the_path() {
    let root = path("/work");
    let extra = vec![path("/other"), path("/shared/lib")];
    assert_eq!(covering(&root, &extra, "/elsewhere"), None);
    assert_eq!(
        covering(&root, &extra, "/work"),
        Some(Covered {
            by: root.clone(),
            exact: true,
            root: true
        })
    );
    assert_eq!(
        covering(&root, &extra, "/work/src"),
        Some(Covered {
            by: root.clone(),
            exact: false,
            root: true
        })
    );
    assert_eq!(
        covering(&root, &extra, "/other"),
        Some(Covered {
            by: path("/other"),
            exact: true,
            root: false
        })
    );
    assert_eq!(
        covering(&root, &extra, "/shared/lib/a"),
        Some(Covered {
            by: path("/shared/lib"),
            exact: false,
            root: false
        })
    );
    // 前缀相同、不是子目录的不算。
    assert_eq!(covering(&root, &extra, "/workspace"), None);
}

#[test]
fn covering_ignores_case_on_windows() {
    let root = path(r"C:\Work");
    let covered = covering(&root, &[], r"c:\work\src").unwrap();
    assert!(covered.root && !covered.exact);
}

#[test]
fn admit_keeps_absolute_paths_outside_the_root_once() {
    let root = path("/work");
    let admitted = admit(
        &root,
        [
            path("/other"),
            path("relative/dir"),
            path("/work"),
            path("/work/sub"),
            path("/other/inner"),
            path("/other"),
            path("/third"),
        ],
    );
    assert_eq!(admitted, vec![path("/other"), path("/third")]);
}

fn files() -> MemoryEnvironment {
    MemoryEnvironment::new(EnvFacts {
        cwd: path("/work"),
        platform: "darwin".to_owned(),
        os_version: "Darwin 24".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        home: Some(path("/Users/dev")),
        uid: None,
    })
    .with_file("/extra/notes.txt", "notes\n")
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: path("/work"),
    }
}

#[tokio::test]
async fn check_resolves_and_stats_like_the_reference() {
    let env = files();
    let workspace = workspace();
    let at = (&env as &dyn Files, &workspace);
    let style = PathStyle::Posix;
    let base = ("/work", Some("/Users/dev"));
    assert_eq!(
        check(at, style, base, "../extra").await,
        Ok("/extra".to_owned())
    );
    assert_eq!(check(at, style, base, "").await, Err(Unusable::Empty));
    assert_eq!(
        check(at, style, base, "/missing").await,
        Err(Unusable::Missing {
            absolute: "/missing".to_owned()
        })
    );
    assert_eq!(
        check(at, style, base, "/extra/notes.txt").await,
        Err(Unusable::NotDirectory {
            parent: "/extra".to_owned()
        })
    );
}

#[test]
fn texts_follow_the_reference_wording() {
    let plain = |text: &str| text.to_owned();
    assert_eq!(
        unusable_text("x", &Unusable::Empty, plain),
        "Please provide a directory path."
    );
    assert_eq!(
        unusable_text(
            "gone",
            &Unusable::Missing {
                absolute: "/work/gone".to_owned()
            },
            bold
        ),
        "Path \u{1b}[1m/work/gone\u{1b}[22m was not found."
    );
    assert_eq!(
        unusable_text(
            "a.txt",
            &Unusable::NotDirectory {
                parent: "/work".to_owned()
            },
            plain
        ),
        "a.txt is not a directory. Did you mean to add the parent directory /work?"
    );
    let covered = |exact, root| Covered {
        by: path(if root { "/work" } else { "/other" }),
        exact,
        root,
    };
    assert_eq!(
        covered_text(".", &covered(true, true), plain),
        ". is already the current working directory."
    );
    assert_eq!(
        covered_text("/other", &covered(true, false), plain),
        "/other is already added as a working directory."
    );
    assert_eq!(
        covered_text("src", &covered(false, true), plain),
        "src is already accessible within the current working directory /work."
    );
    assert_eq!(
        covered_text("/other/a", &covered(false, false), bold),
        "\u{1b}[1m/other/a\u{1b}[22m is already accessible within the additional working directory \u{1b}[1m/other\u{1b}[22m."
    );
    assert_eq!(
        added_record("/extra", &Remembered::No),
        "Added \u{1b}[1m/extra\u{1b}[22m as a working directory for this session \u{1b}[2m· /permissions to manage\u{1b}[22m"
    );
    // 确认框里选了「记住」与「不加」（permission-updates.md §6.1）。
    assert_eq!(
        added_record("/extra", &Remembered::Saved),
        "Added \u{1b}[1m/extra\u{1b}[22m as a working directory and saved to local settings \u{1b}[2m· /permissions to manage\u{1b}[22m"
    );
    assert_eq!(
        added_record(
            "/extra",
            &Remembered::Failed {
                reason: "disk full".to_owned()
            }
        ),
        "Added \u{1b}[1m/extra\u{1b}[22m as a working directory. Failed to save to local settings: disk full \u{1b}[2m· /permissions to manage\u{1b}[22m"
    );
    assert_eq!(
        declined_record("/extra"),
        "Did not add \u{1b}[1m/extra\u{1b}[22m as a working directory."
    );
}
