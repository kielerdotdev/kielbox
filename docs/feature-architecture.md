# Desktop feature architecture

## Why the desktop needs boundaries

Kielbox supports choosing tools independently. A user can keep ShadowPlay for recording,
import its videos for editing, and use Kielbox screenshots and storage analysis. Turning
off a tool must also remove its global shortcuts and prevent its workers from starting.

The existing Rust crate boundaries already separate much of the implementation, but they
are not product feature boundaries. `rune_ui::State` composes the desktop, `Preferences`
persists choices, and the UI loop handles tray, hotkey, palette, and file-dialog events.
Previously those entry points assumed every desktop feature was enabled.

Workers are mostly lazy already: capture starts on an explicit action, editor decoding
starts on open, disk scanning starts after choosing a folder, and utilities start on run.
The feature policy preserves that design rather than introducing a plugin loader.

## Capability ownership

| Feature ID | Owns | Independent of |
| --- | --- | --- |
| `screenshots` | Screenshot actions, global bindings 3–5, capture overlay and saving | Recording, screencasts, image editing |
| `image_editing` | Opening and annotating images in the standalone editor | Screenshots and video editing |
| `video_editing` | Opening, playing, editing and exporting imported or captured videos | All recording features |
| `storage_analysis` | Disk scan entry points, scan cancellation, treemap | Utilities and all media features |
| `utilities` | Utility pages and their jobs, including screen-reading utilities | Standalone editors and recording |
| `recording` | Native capture pipeline, replay buffer, recording and replay actions, bindings 0–2 | Editors, screenshots, screencasts |
| `screencasts` | Quick recording worker, binding 6 | Native recording and replay |

Native recording and replay deliberately share one switch. The current pipeline encodes
and buffers while recording, so separate switches would promise resource isolation that
the implementation does not provide. Splitting those requires changing pipeline ownership,
its replay-memory budget, command handling and finalization tests first.

Screen-reading utilities own their own capture requests. Disabling `screenshots` removes
the screenshot product, not screen access from an enabled OCR or color utility. Likewise,
screenshot-region annotation remains part of screenshots even with standalone image
editing off. The saved screenshot stays in the library instead of opening a disabled editor.

The app shell, preferences, library, imports, thumbnails, file recovery, updates, tray host,
and shared codecs remain infrastructure. Library thumbnail decoding can still happen when
editing is disabled. A feature switch is not an OS permission or a security sandbox.
Native capture, screenshot entry points, tray and global shortcuts retain their existing
Windows support; enabling a switch does not add support on another OS.

## Policy and persistence

`crates/desktop/src/features.rs` defines the finite capability set and its labels, stable
IDs, saved selection, shortcut ownership and process policy. No feature enables another
implicitly. `feature_controls.rs` owns desktop selection changes and settings.
`rune_settings.rs` composes the settings sections separately from the app shell.
`State::page_available` maps the composed pages onto that policy. The Capture
page exists if screenshots, native recording, or screencasts are enabled; Tools exists if
utilities or storage analysis are enabled. Library and Settings always remain reachable.

Preferences persist `features.disabled` as a set of stable string IDs. Missing settings
keep all existing features enabled, maintaining compatibility with old preference files.
Unknown saved IDs survive a downgrade without invalidating the rest of the user's settings.
The existing atomic preference-save and recovery mechanism is reused.

For process-level overrides, set `KIELBOX_DISABLE_FEATURES` to comma-separated IDs before
starting Kielbox, for example `recording,screencasts`. Overrides are read once per process,
never persisted over the user's choices, and cannot be reversed in Settings. Blocked rows
explain the restriction. Invalid IDs or invalid encoding block all optional features and
show an actionable error. Correct the environment and restart to recover. There is no
remote configuration fetch or remotely operated kill-switch service.

## Lifecycle and enforcement

Changes apply immediately after preferences save successfully. A failed save restores the
previous preferences and leaves runtime registrations unchanged. Disabling is rejected
while the feature owns active work: capture or device discovery for recording, quick
recording for screencasts, loads/playback/unsaved edits/exports for the affected editor,
and capture or utility jobs for their owning feature. Unrelated active work does not block
a change or lose its page. Shared screenshot/utility overlays conservatively block both
screen features until delivery completes. The user finishes or cancels affected work and
retries; no recording is abruptly terminated and no dirty document is discarded.

After a successful change, inactive affected editor state is closed, utilities are reset,
and disk scans are cancelled when disabled. Navigation falls back to Library when needed.
The command palette is dismissed. Changes affecting global shortcuts unregister and rebuild
only enabled bindings; utility changes only rebuild the tray. Editor and storage changes
leave native registrations intact. Tray actions are rebuilt when their feature set changes. Tray events use the native library queue so rebuilding does
not retain a stale sender in its one-time global event handler. Original shortcut strings remain saved, so enabling
a feature restores the user's binding choices. Shortcut conflicts on enable are reported
by the existing registration mechanism rather than silently overwriting another app.

Enforcement also sits at work-start boundaries: native recording, replay save, screencasts,
screenshot capture, media opening, utility opening/run, and disk-dialog completion. File
callbacks recheck policy rather than assuming the feature is still enabled. Visibility is
not the enforcement mechanism.

## Scope and follow-up boundaries

This policy governs the Rune desktop. The separate `ssx` CLI, workflow engine, screenshot
application and shell integrations are separate entry points and do not consume desktop
preferences. Uploading lives in that older service/workflow stack and is not a desktop
feature yet. Adding a desktop Upload switch now would falsely imply it governs those apps.
A future shared policy must be passed explicitly into their service composition and workflow
dispatch, with rejection tests for each action. Do not silently apply desktop policy to
standalone programs that have separate configuration and users.

This change provides runtime composition, not smaller installations. Cargo still links
its existing dependencies. Optional packaging requires a separate artifact matrix and
compile-time feature configuration, plus installer and update coverage for each variant.
Runtime choices should remain the primary supported user experience.

Further restructuring should move feature-specific view and settings code out of
`rune_ui.rs`, keeping the shell responsible for composition and the individual feature
responsible for jobs and cancellation. There is no need for dynamic plugins or a universal
service interface to perform that extraction. Add a feature ID only when there is a real
product boundary, a work-start guard, lifecycle behavior, and combination tests.

## Verification

Policy tests enumerate all 128 saved feature combinations, serialization, invalid override
handling and recording-free shortcut selection. Desktop tests cover disabled navigation,
editor opening, retained media and settings, settings persistence failure, and re-enabling.
Windows-only tests check recording start rejection without touching capture hardware.
Real Windows smoke testing should additionally exercise shortcut release against another
recorder, native tray rebuilding, screenshot overlay completion, and switching features
after finishing recording or export.

Validation in this worktree on 2026-11-11:

- `cargo test -p desktop --no-default-features`: 83 passed, 0 failed on Linux.
- Includes fresh-process environment override tests, all 128 selection combinations
  against individual kill switches, preference migration and rollback, palette filtering,
  pointer navigation with features off, storage analysis without utilities, and feature-scoped
  job ownership that permits unrelated changes during editor loading or unsaved work.
- `git diff --check` passed. Modified Rust files were parsed with rustfmt; existing
  workspace-wide formatting differences were left outside this change.
- `cargo check -p desktop --target x86_64-pc-windows-msvc` was attempted but stopped
  in the existing `ring` dependency because the environment has no MSVC `lib.exe`.
  Windows-only Rust code and the native UI still need validation on a Windows toolchain.
