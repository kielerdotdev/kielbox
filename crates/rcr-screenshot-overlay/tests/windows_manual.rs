//! Manual Windows test (compile-checked on Linux with
//! `cargo check --target x86_64-pc-windows-msvc -p rcr-screenshot-overlay --tests`).
//!
//! Run on a real Windows machine, ideally with two monitors at different DPI scales:
//!
//! ```text
//! cargo test -p rcr-screenshot-overlay --test windows_manual -- --ignored --nocapture
//! ```
//!
//! Checklist: see the crate README ("Windows").
#![cfg(windows)]

use rcr_screenshot_overlay::{OverlayInput, OverlayOptions, demo::demo_frame, select};
use rcr_image_types::Point;

#[test]
#[ignore = "needs an interactive Windows desktop; run with --ignored --nocapture"]
fn manual_select_over_a_synthetic_desktop() {
    let input = OverlayInput {
        desktop: demo_frame(1920, 1080, Point::new(0, 0)),
        monitors: vec![],
        windows: vec![],
        options: OverlayOptions { timeout_ms: Some(120_000), ..OverlayOptions::default() },
    };
    let outcome = select(input).expect("overlay");
    println!("{outcome:#?}");
}
