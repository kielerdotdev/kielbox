# Local MP4 preparation from Clip show — 2026-11-05

> Historical record. Source paths and commands describe the audited revision.
> Retired frontend sources and proof scripts were removed after this report;
> see [Rune status](RUNE_MIGRATION.md) for current behavior. Exact original reports remain in git at `a52bff7`.

This work adds an explicit local-file preparation path to the watch session. It does not create a remote watch party, upload footage, or copy a file automatically.

## Behavior and ownership

Clip show's existing four-control header is retained. A 36-pixel row beneath it displays the current source filename and size from the project's already-loaded `SourceStamp`; rendering does not stat files. Missing cached size information is labeled unavailable.

**Prepare MP4** snapshots the current framed project into a separate clone, bounds its dimensions to Full HD without upscaling, and requests H.264 video with the project's audio mixed to the normal AAC export path at an 8 Mb/s video target. The original project, framing controls, tracks, gains, edits, undo history, and normal export preset are not rewritten. Playback-monitor volume/mute are separate from the exported project audio. Existing normal editor exports explicitly retain AV1.

The existing single export worker owns preparation, progress, cancellation, and publication. Navigating to another clip does not cancel that worker or relabel its source. Worker-side output metadata is sent before the existing terminal export event and matched by destination path. The completed receipt includes the filename, cached output size, and source title.

The row provides a separate **Copy file** action only after a successful file receipt. It uses the existing admitted Windows file-clipboard action on an explicit click. **Show folder** uses the existing shell-action worker. Neither preparation nor completion sends a message, uploads footage, or changes the clipboard. Failure details and cancellation remain discoverable from the watch row. Editing clears the displayed saved receipt through the existing edit-change path.

The compatibility preset is a codec/container choice, not a guarantee about every messaging service's size, duration, account, or playback limits. Actual output size is shown for the user's decision. Missing H.264 encoding support is reported through the existing export-failure and retained-output recovery mechanisms.

## Files and API coordination

- `crates/desktop/src/editor.rs`: normal/share export dispatch, worker-side size event, metadata receipt ownership, watch-row integration.
- `crates/desktop/src/editor_watch_share.rs`: local share UI, bounded-clone preset, cached metadata labels, and synthetic status fixtures.
- `crates/desktop/src/editor_watch_fullscreen.rs` and the existing keyboard smoke: explicit fullscreen controls, acknowledged hide restoration, controller regressions, and guarded hidden key dispatch with the platform toggle suppressed.
- `crates/desktop/src/editor_native_fullscreen_proof.rs`: separate opt-in native geometry evaluation using an exclusively owned, shown, nonactivating background host.
- The primary agent owns the coordinated core API: `ExportVideoCodec::{Av1,H264}` and `ExportOptions.video_codec`, including H.264 encoding and all other option constructors.

The existing `Event::Exported` signature remains compatible with injected export-receipt proofs. `ExportSize` adds cached metadata without introducing UI-thread filesystem access.

## Validation

Pure regressions verify that the compatible preset fits landscape/portrait/small sources without modifying the input project, and that output-size metadata is destination-scoped, ignores unrelated late events, and clears with an invalidated receipt.

`seed_watch_share_status` provides four generated UI states: idle, preparing, ready receipt, and failure. It starts no encoder, probes no filesystem path, and performs no clipboard or network action. All four states passed hidden snapshot bounds checks at both 900×600 and 1440×900 (eight images), and the primary agent visually inspected all eight successfully. `verify_clip_show_snapshot` checks the share-row metadata/action bounds in addition to the existing header, preview, and seek bar.

The desktop suite passed **253 tests, 8 ignored**, including the share/controller cases and the new hide-during-entry acknowledgment regression. Strict Desktop all-targets Clippy passed after the production restoration fix; the preceding four-package strict check also passed. Actual H.264/AAC preparation passed with generated media: 60 decoded frames over one second, 48 kHz stereo AAC, and SSIM **0.987799** against an independent lossless trimmed graph (minimum 0.98). Source/project preservation and non-clobber checks passed. See [export fidelity evidence and limitations](EXPORT_FIDELITY_2026-11-05.md). Those focused receipts describe development layouts/media processing; separate final package receipts appear below. Neither establishes an upload test or service size-limit guarantee. No actual clipboard action is part of these proofs.

## Fullscreen inspection

retired UI's Windows fullscreen implementation was inspected. Its inner transition changes style/bounds with `SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOZORDER`. The public API deliberately defers fullscreen for a hidden window until it is shown. Therefore an exclusively hidden-host proof cannot verify actual native fullscreen geometry or restoration. The initial transition-based hidden proof timed out; treating its pending request as a completed native transition would be incorrect. That proof remains hidden, and no vendored retired UI change was made to force it. A separate explicitly owned shown-host evaluation now establishes native geometry, as detailed below.

Fullscreen is an explicit viewer control and F11 action. Escape first leaves fullscreen and a later windowed Escape returns to editing. Pending native transitions are coalesced so rapid repeated Escape cannot enqueue a second toggle and accidentally re-enter fullscreen. Explicit Library/editor exits restore windowed mode. Hiding through another route now waits for pending entry acknowledgment, requests one exit, and waits for exit acknowledgment within a three-second deadline; it clears pending controller state only after restoration. A timeout retains ownership and reports an error. This replaces a one-shot 10 ms assumption that could miss entry or leave a pending controller stranded. Restoration is canceled if the user returns to watching before the exit request.

The revised `--smoke-editor-keyboard` fixture checks the exclusively owned host is invisible and is not the foreground HWND before enabling a private platform-suppression flag. It paints the actual Clip show key context, dispatches real retired UI F11 events, and verifies the production controller receives a pending fullscreen request. Repeated F11/Escape must respect the pending guard. Native bounds, project/Undo/Redo, and monitor volume/mute remain unchanged. Fixture state is cleared on success or assertion failure. No actual platform fullscreen request is queued, avoiding deferred placement changes to the hidden host. No OS input or visible user window is involved.

Pure controller regressions passed for simulated acknowledgment, repeated requests, Escape priority, windowed fallthrough, and delayed entry/exit restoration without duplicate toggles. The hidden keyboard/controller proof **passed in 7,777 ms**; evidence is `.downloads/plan-continuation/editor-keyboard-controller-final.log`. This checkpoint establishes controller behavior and hidden F11 dispatch, not native fullscreen transitions.

## Actual nonactivating native fullscreen evaluation

The separate `--smoke-editor-native-fullscreen --open <generated fixture>` evaluation **passed**, with total keyboard/native-fixture elapsed time **8,547 ms**. Evidence: `.downloads/plan-continuation/editor-native-fullscreen-position-fixed.log`. It uses Silent playback, retired UI-internal key dispatch, and a shown background host protected by `WS_EX_NOACTIVATE`, nonactivating placement at the bottom of the Z-order, an activation observer latch, and PID/unique-HWND-property ownership checks. No retired UI activation API, OS input, desktop readback, clipboard action, or other application's controls are used.

| Native transition | Observed elapsed time |
| --- | --- |
| First F11 entry | 27 ms |
| Escape exit and restoration | 28 ms |
| Second F11 entry | 29 ms |
| F11 exit and restoration | 28 ms |
| Entry before sidebar hide | 23 ms |
| Sidebar-hide restoration | 31 ms |
| Immediate hide during pending entry | 309 ms, including a deliberate 300 ms stability wait |

These are single-run transition observations, not a latency distribution. All strict native RECT, logical-bounds, style, monitor-coverage, history, volume, and ownership checks passed. The result reports `activation_observed:false`, `os_input_sent:false`, and `platform_toggle_suppressed:false`. Earlier shown-host attempts exposed a fixture initialization issue: a never-shown retired UI host cached its outer origin; restored visible windows reported client origin. Actual RECT/styles already matched. Explicit nonactivating initial movement establishes the correct baseline; no restoration comparison was weakened.

This closes the **owned nonactivating background native geometry/restoration** gate. The host can remain occluded behind existing applications. It does not establish manual foreground fullscreen experience, physical keyboard/mouse delivery, monitor scanout, every DPI/display arrangement, or general cross-machine portability. The original hidden failures and their narrower successful controller proof remain historical evidence, not native passes.

## Release-package checkpoint

The reviewed release executable is `dist/windows-reviewed-2026-11-05/rcr.exe`, SHA256 **`1EEA531A1D2AB8D8151BE1E84C8781A6A04F0E025D413450DC2B01A97698E63D`**. The package passed the same native fullscreen/keyboard fixture from the clean working directory `.downloads/plan-continuation/release clean work`, with embedded release shaders and the default installed FFmpeg on PATH. Total fixture time was **11,263 ms**; evidence is `.downloads/plan-continuation/release-clean-native-fullscreen.log`.

Release transition observations: F11 entry 29 ms, Escape restoration 25 ms, second entry 27 ms, F11 exit 32 ms, entry before hide 24 ms, sidebar-hide restoration 56 ms, and immediate hide during pending entry 306 ms (including the deliberate 300 ms stability wait). All native geometry/style/history/volume and ownership guards passed. The activation latch stayed false, no OS input was sent, and platform toggles were not suppressed. These are single-run observations with the same nonactivating background scope as the development proof.

The clean-working-directory release share snapshot `.downloads/plan-continuation/release-clean-share-900.png` also passed bounds checks and visual inspection at 900×600. This adds release rendering evidence to the eight development layouts; it is not an actual clipboard transfer or upload. The final packaged viewer suite subsequently passed **six runs and all 80 unchanged budget checks**, plus layouts/capture-menu/editor-keyboard proofs, at `.downloads/plan-continuation/viewer-release/20261105-191613-94ff28a1/summary.json`. Cold first-paint samples were 706.4409–814.4757 ms; 100 rapid switches ended on the correct paint in 61.8378–92.1208 ms. Post-runtime package verification passed all 22 payloads. The [hash-bound final acceptance JSON](../.downloads/plan-continuation/release-acceptance.json) closes this local review wave for the exact release SHA above.

Installed FFmpeg remains an external dependency; a clean launch directory is not a clean-machine dependency test. Loaded CS, microphones, hour-long capture, manual foreground fullscreen and exhaustive DPI arrangements remain outside these proofs. Studio-wide recent-export receipts, session organization and broader hardware/game automation remain roadmap work rather than silently completed features. Launch instructions retaining the existing data working folder are in [the package report](WINDOWS_PACKAGING_2026-11-05.md); this documentation does not launch the app.

The broader product and release evidence remain in [the improvement plan](PRODUCT_IMPROVEMENT_PLAN_2026-11-05.md).
