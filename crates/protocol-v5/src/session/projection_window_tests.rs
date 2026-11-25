//! 冷读只建窗口（protocol-v5.md §71.3）：由冷视图只建窗口的投影 adopt + thin 之后，与「建全部整行再 thin」逐项相同（整行、骨架、版本、state、
//! 快照）；建出的内容里整行只有窗口里的与留整行的。

use rust_cli_kernel_api::session_api::SessionView;
use serde_json::Value;

use super::kit::{self, long_view};
use super::residence::OUTLINE;
use super::*;
use crate::consts::SESSION_SNAPSHOT_ROWS;
use crate::generated::core_v1::FrameBody;

/// 内容进 Publisher、换骨架（同 actor 物化时的两步）。
fn settled(projection: &Projection, content: Content) -> Publisher {
    let mut publisher = kit::publisher();
    publisher.adopt(content);
    publisher.thin(ROWS, OUTLINE, u64::MAX, |fields| {
        projection.resident(fields)
    });
    publisher
}

/// 一份 Publisher 里看得到的全部：全部行（整行与骨架，按全序）、哪些是骨架、快照。
fn seen(publisher: &Publisher) -> (Vec<Value>, Vec<String>, Value) {
    let page = publisher
        .page(ROWS, CREATED_AT_SEQ, None, usize::MAX)
        .unwrap();
    let FrameBody::Snapshot(snapshot) = publisher.frame_for(None, "s", false).body else {
        panic!("not a snapshot")
    };
    (
        page.items,
        page.outlined,
        serde_json::to_value(snapshot).unwrap(),
    )
}

/// 长会话（有回合）、同一段对话没有回合记录（Node 写的）、比窗口短的会话。
fn views() -> Vec<SessionView> {
    let turnless = SessionView {
        turns: Vec::new(),
        ..long_view(20, 100)
    };
    vec![long_view(20, 100), turnless, long_view(2, 10)]
}

#[test]
fn a_window_build_matches_building_everything_then_thinning() {
    for (n, view) in views().into_iter().enumerate() {
        let (whole_projection, whole) = Projection::build(&view, kit::origin(false));
        let (window_projection, window) = Projection::build_window(&view, kit::origin(false));
        let full_rows = window.collections[ROWS].len();
        let pinned = window.collections[ROWS]
            .values()
            .filter(|fields| window_projection.resident(fields))
            .count();
        assert!(
            full_rows <= SESSION_SNAPSHOT_ROWS + pinned,
            "{n}: {full_rows} {pinned}"
        );
        assert_eq!(
            seen(&settled(&whole_projection, whole)),
            seen(&settled(&window_projection, window)),
            "{n}"
        );
    }
}
