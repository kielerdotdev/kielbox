# Capture state implementation

Owned changes: `rune_capture.rs`, `tray.rs`, `quick_recording.rs`.

F03/F18: explicit Idle/Starting/Recording/Paused/Finishing/Failed quick state; pause only available after startup; every new worker resets state, counters and warnings; Started/Progress/Finishing and terminal events update presentation. Retirement uses the existing tail-draining Worker::poll helper. Stop immediately enters Finishing. UI shows phase, recorded duration and warnings. The 600-second cap is disclosed before capture and automatic limit completion adds a saved-output warning. No-clobber publication remains intact.

Native capture source controls refresh display and render/capture endpoint discovery on a separate thread. Controls preserve exact backend identities, offer Windows primary/default policies, display saved labels even when unavailable, and lock during native capture. Shared audio toggles lock while either recorder is running. Quick capture explicitly states its separate primary-display/cursor and Windows default audio endpoint policy; it does not claim to use native endpoint/display selections.

## Shell integration required

In the existing Windows poll block, immediately before `tray.update`, add exactly:

```rust
tray.update_quick_state(s.capture.quick_tray_state(), &s.preferences.hotkeys[6]);
let _ = tray.update(s.capture.tray_state(), &s.preferences.hotkeys);
```

The setter caches the quick phase/menu label; `update` applies the combined icon/tooltip/status. Call both in this order on each poll. Native replay/record action labels and enablement remain independent. Paused/finishing quick capture keeps an amber indicator unless native recording still requires red. The existing bool setter remains compatible with legacy callers. No rune_ui.rs changes were made by this implementation owner.

## Validation

Added targeted regression tests for quick lifecycle presentation, pause availability, native recording indicator preservation, and terminal events arriving between drain and worker retirement. These tests have NOT been executed. No local compilation, cargo build/test, capture or FFmpeg execution was performed. Rustfmt parsed/formatted the capture and tray edits; git diff --check passed for the owned files. Central Boat validation still needs Windows compilation/tests and a native recording, pause, stop, minimize-to-tray, fresh-session and device-disconnect walkthrough.
