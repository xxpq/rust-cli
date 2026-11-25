//! `puffin-tui/cockpit.json` 的读写（tui-cockpit.md §3.1、§3.6）。
use super::CockpitPrefs;

/// 没有的键、读不出的文件用缺省（按需要分组、← 打开任务列表）；以前只写 `byFolder` 的文件照样读得出。
#[test]
fn missing_keys_and_bad_files_use_the_defaults() {
    let d = CockpitPrefs::default();
    assert_eq!(
        d,
        CockpitPrefs {
            by_folder: false,
            left_arrow: true
        }
    );
    assert_eq!(CockpitPrefs::parse("not json"), d);
    assert_eq!(
        CockpitPrefs::parse("{\"byFolder\":true}\n"),
        CockpitPrefs {
            by_folder: true,
            left_arrow: true
        }
    );
    assert_eq!(
        CockpitPrefs::parse("{\"leftArrow\":false}"),
        CockpitPrefs {
            by_folder: false,
            left_arrow: false
        }
    );
}

/// 两样一起写，读回来不变。
#[test]
fn both_values_are_written_together() {
    let prefs = CockpitPrefs {
        by_folder: true,
        left_arrow: false,
    };
    assert_eq!(prefs.text(), "{\"byFolder\":true,\"leftArrow\":false}\n");
    assert_eq!(CockpitPrefs::parse(&prefs.text()), prefs);
}
