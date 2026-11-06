# Screenshot and toolkit reliability audit — 2026-11-05

This audit covers frozen screenshot capture, selection cancellation, screenshot file publication, and compositor resource limits. It builds on the performance work already in the working tree. It does not claim hardware capture compatibility or a completed comparison of every feature in competing applications.

## Research and product implications

Only official documentation informed this comparison. Similar-looking `get-sharex.com` sites were excluded; the ShareX sources below use `getsharex.com`.

| Reference | Documented behavior | Implication for RCR |
| --- | --- | --- |
| [ShareX region capture](https://getsharex.com/docs/region-capture) | Region selection supports annotations, detected windows, reusable regions, confirmation, and cancellation. Dismissing selection produces no capture. | A canceled native request or late selector reply must not count as success or change the remembered region. Selection and annotation should preserve frozen pixels. |
| [ShareX image editor](https://getsharex.com/docs/image-editor) | Captures can be reviewed and edited before subsequent configured tasks. Copy, save, upload, continue, and cancel are available in the integrated workflow. | RCR should make each operation's outcome explicit and retain edited pixels when a local action fails. Recovering a save or clipboard failure must not repeat an upload. |
| [OBS standard recording output guide](https://obsproject.com/kb/standard-recording-output-guide) | OBS documents the output path and filename configuration, crash resilience of MKV, and optional remuxing after recording stops. | Treat publication of a usable local file as its own success boundary. Processing or replacement failures should leave prior usable output intact. This is a design inference for screenshot saving, not a claim that OBS uses RCR's file replacement implementation. |

The OBS guide is relevant to the wider product's separation of capture, saved output, and optional processing. It does not establish any particular screenshot latency target. ShareX documentation describes workflow behavior, not measurements of its speed on this machine.

## Findings fixed in this pass

### 1. Save As could destroy the previous file before replacement succeeded

**Evidence:** `StdFileSystem::write_file` previously used `std::fs::write`. The screenshot workflow's Save As step uses that method. Opening an existing destination for writing truncates it before the replacement image is fully written. A full disk or later write failure could therefore leave the old screenshot replaced by partial bytes.

**Change:** explicit writes now use the existing sibling-file replacement implementation in `settings::atomic_write`. The new image is written and synced to a temporary file in the destination directory before replacement. Explicit overwrite remains supported. A failed first save publishes no destination file; a failed replacement leaves the original readable. Existing Windows transient-rename retry behavior is reused. Temporary cleanup now starts only after this invocation successfully creates its sibling file, so a failed `CREATE_NEW` cannot trigger deletion of an unowned file.

**Regression:** `save_as_stages_replacement_and_preserves_original_on_disk_failure` injects a storage-full error after partial staging. It verifies that the original remains readable while staging, survives the failure byte-for-byte, temporary output is removed, a failed first save publishes nothing, and a later explicit replacement succeeds.

**Files:** `crates/workflows/src/workflow/services.rs`, `crates/workflows/src/settings/io.rs`, and the test-only helper re-export in `settings/mod.rs`.

**Limits:** this protects the publication boundary. It does not promise immunity to every filesystem or hardware failure. The replacement requires enough free space for a new staged copy, and readers on Windows can temporarily prevent replacement. A failed replacement returns an error rather than discarding the prior output.

### 2. Cancellation during native capture or selection could arrive too late

**Evidence:** `ScreenCapturer::capture` checked cancellation before entering a native backend, then returned the resulting image without another check. Interactive capture could also accept a selector result and update Last region after the caller had canceled. The explicit-target API used by the desktop workflow did not accept a cancellation token at all.

**Change:** cancellation is checked after native capture, before opening a selector, after selector completion, and before remembering a selected region. A new cancellable explicit-target method guards both sides of native capture and the Last region update; the desktop workflow uses it. The existing explicit-target API remains available for callers without a token.

**Regressions:**

- `native_capture_completing_after_cancel_cannot_report_success_or_change_last_region` injects cancellation as the fake native backend returns pixels. It covers fullscreen, monitor, window, Last region, and explicit rectangle capture.
- `canceled_native_capture_does_not_open_a_selector` verifies that a completed but canceled request cannot open a new selection UI.
- `selector_returning_a_selection_after_cancel_does_not_persist_it` simulates a late selector reply and verifies that the previous Last region remains intact.

**Files:** `crates/toolkit-services/src/capture.rs` and `crates/toolkit/src/worker.rs`.

**Limits:** native OS calls remain synchronous and cannot be forcibly interrupted through this API. The fix rejects their stale result once control returns. It does not claim instantaneous cancellation of a blocked driver or a transaction spanning every possible cancellation race.

### 3. Sparse display geometry could allocate an enormous compositor canvas

**Evidence:** `composite` previously allocated a frame covering the complete bounding rectangle before validating the input formats. Two tiny frames with distant virtual-desktop origins can imply a canvas many orders of magnitude larger than their source pixels. Extreme coordinates could therefore provoke a large allocation or an out-of-memory abort.

**Change:** canvas size is checked before allocation using saturating byte arithmetic. The compositor rejects an axis over 32,768 pixels or a canvas over 512 MiB, with a recovery message suggesting capture of one display or a smaller region. Input formats are validated before allocating the destination. The limits still accommodate four horizontally arranged 8K SDR displays.

**Regression:** `sparse_and_extreme_display_geometry_is_rejected_before_canvas_allocation` supplies only two 1×1 source frames and tests excessive width, excessive area, and opposite extreme integer coordinates. Existing gap, negative-origin, and mixed-format tests remain applicable.

**File:** `crates/screenshot/src/composite.rs`.

**Limits:** this bounds the stitched canvas, not the application's total memory. Native frame pools, source images, cursor variants, annotation history, and GPU uploads have separate allocations. A valid allocation below the cap can still fail on a memory-starved machine. Very large legitimate arrangements may require per-display capture.

## Earlier performance work retained

The current working tree already contains these improvements; they are separate from the three reliability fixes above:

- Reuse display topology for batches of Windows monitor captures.
- Avoid a second full-frame compositing copy for one-display screenshots.
- Skip a capture delay when the picker has already frozen the supplied pixels.
- Cache a bounded set of HDR half-float lookup tables without retaining screenshot pixels.
- Use fast lossless PNG for saved screenshots, trading compressed size for encoding time.
- Borrow packed RGBA input for PNG/WebP encoding instead of cloning an entire frame.
- Prepare the picker's two preview images on the snapshot worker and convert BGRA in the optimized pixel library.

The two native cursor/no-cursor captures are intentionally retained. Replacing the second with cursor compositing requires verified handling of native color and monochrome cursor masks, hotspots, DPI, clipping, and HDR. Synthetic tests alone cannot establish that behavior.

## Validation and evidence

The new regressions use synthetic pixels, fake capture backends, and temporary local files. They do not invoke a desktop capture backend, microphone, upload destination, visible window, or external application. This subagent did not start Cargo processes; the parent coordinates shared builds and checks.

The initial central reliability suites passed on 2026-11-05: screenshot **4**, toolkit **48**, toolkit services **85**, and workflows **352** tests passed, with **1** workflow test ignored and no failures. The suites ran outside the sandbox because service tests bind a local mock HTTP server. This is local test infrastructure, not an upload of user media. The injected storage failure, native cancellation, selector cancellation, and sparse geometry regressions appear individually as passing cases in [the central test log](../.downloads/hardware-eval-2026-11-05/reliability-tests-unrestricted.log).

These results do not establish hardware capture behavior. `git diff --check` also passed for the touched product files.

Relevant commands for central verification:

```powershell
cargo test -p workflows -p toolkit-services -p screenshot -j1
cargo test -p toolkit -j1
```

Repeatable earlier synthetic performance evaluations are available through `scripts/evaluate-capture-ux.ps1`, including HDR conversion, PNG compression tradeoffs, borrowed encoding, and picker preparation. Those measurements exclude native capture, compositor scheduling, GPU upload, and user-perceived completion latency. Compare decoded pixels and output sizes as well as wall time; do not convert a pixel-kernel speedup into an end-to-end capture claim.

The later [actual screenshot evaluation](ACTUAL_SCREENSHOT_EVALUATION_2026-11-05.md) passed five current-production `ScreenSource::capture_desktop` calls using **windows-wgc**, with two displays composited into **5120×1440**. Median capture time was **124.7371 ms**; the initial capture took **286.6926 ms**. Every frame had nonzero luminance variance and was not uniformly black. Only [JSON metadata](../.downloads/plan-continuation/screenshot-hardware.json) was persisted; no PNG or screenshot pixels were saved. These timings exclude the separate pixel-analysis scan and screenshot UI, selection, saving, and clipboard stages. They establish a current capture baseline, not a before/after improvement or end-to-end UI latency. Release packaging, hardware, and clean-folder UI acceptance are tracked separately in the [product plan](PRODUCT_IMPROVEMENT_PLAN_2026-11-05.md).

## Remaining work and acceptance criteria

| Priority | Work | Acceptance evidence |
| --- | --- | --- |
| P1 | Extend the measured native desktop baseline to frozen picker paint, confirmation, local save, and clipboard publication. | Per-stage timestamps and sufficiently sampled median/p95 for SDR, HDR, and mixed-DPI desktops; no unexplained single long stage. |
| P1 | Bound WGC/duplication fallback waits and expose the failed backend stage. | Inject no-frame, device-loss, and timeout cases; user sees the failing stage and can retry without restarting RCR. |
| P1 | Extend the desktop-batch allocation preflight to native-desktop, direct monitor/window, and region paths. | Path-specific peak accounting rejects pathological dimensions before native frame creation and covers the final region crop; normal captures remain pixel-correct. |
| P1 | Evaluate a single frozen desktop plus native cursor snapshot. | Hardware comparisons cover animated/color/monochrome cursors, hotspots, negative monitor origins, mixed DPI, HDR, and cursor hiding. Keep the current path until parity is established. |
| P1 | Exercise clipboard contention and save permission/rename failures end to end. | Failure preserves annotations; Copy/Save retry succeeds; completed workflow steps are not repeated. |
| P2 | Add explicit per-stage job status while native capture or file publication is pending. | A slow injected backend produces actionable progress and Cancel feedback without a frozen main UI. |
| P2 | Define screenshot memory budgets across retained sources, preview handles, annotation actors, and GPU images. | A repeat-capture soak demonstrates bounded retained memory and released canceled previews. |
| P2 | Recover screenshot edits across unexpected process termination. | A reproducible interrupted-edit test restores an explicit recovery draft without silently overwriting originals. |
| P2 | Evaluate fast PNG size policy on realistic UI, photographs, and game frames. | Report encode median/p95, compressed bytes, and exact decoded parity; decide whether a user-facing size/speed choice is justified. |

Broader recording/editor/library work and the integrated product execution plan are tracked separately in [the product improvement plan](PRODUCT_IMPROVEMENT_PLAN_2026-11-05.md).

## Follow-up read-only review

The three implemented fixes retain the boundaries described above: replacement is explicit and staged, cancellation rejects late results rather than interrupting drivers, and the compositor cap does not represent a global memory budget. No additional product changes were made during this review.

The primary agent's diagnostic worker catches a callback panic, delivers an error receipt, and continues processing future reports. The bounded queue retains an admitted write even when its UI receipt is dropped. These behaviors are appropriate for the current filesystem callbacks. One migration edge remains: an already oversized `editor-errors.log` is renamed whole to the backup, so that first backup can exceed the new 4 MiB cap. A seeded oversized-log regression and a bounded-tail migration would close that gap. The separate process panic hook still synchronously writes `crash.log`; that file is outside the editor-log rotation policy.

The next concrete screenshot candidate was malformed selection-mask handling. It was reported for prioritization and then authorized for the Wave 2 implementation below.

## Wave 2: reject invalid masks before returning a screenshot

**Trigger and prior behavior:** a freeform/ellipse selector returns a mask whose length disagrees with its original bounding rectangle. `apply_mask` previously logged a warning and ignored that mask; capture then succeeded with all rectangular pixels included. The old combined clipping/bad-mask test explicitly expected this fallback. Pixels outside the intended shape could therefore be saved or shared.

**Change:** `apply_mask` now returns an actionable error instead of permitting an unmasked fallback. It checks mask size with checked arithmetic, frame/clipped dimensions, nonempty bounds, containment of the clipped selection, and supported pixel layout before changing pixels. `capture_interactive` propagates the failure before remembering Last region. Valid clipping and binary coverage behavior remain intact, including clearing all channels of excluded pixels.

**Regressions:** `malformed_masks_fail_capture_without_publishing_unmasked_pixels_or_changing_last_region` covers empty, short, long, and incorrectly clipped masks. Every case must return an error, preserve the prior persisted region, and leave only the prior region file on disk. `invalid_mask_geometry_is_rejected_before_modifying_source_pixels` tests out-of-bounds geometry, mismatched output dimensions, and extreme original dimensions using tiny synthetic frames. `a_mask_is_cropped_with_a_clipped_selection` now checks every output pixel against the expected frozen source or transparent black.

**File:** `crates/toolkit-services/src/capture.rs`.

Validation status: Wave 2 **passes**. All 18 capture tests passed, followed by the
complete toolkit services suite (**87 passed**). Screenshot **4**, toolkit **48**
and workflows **352 passed / one ignored** also passed in the final combined run.
Evidence: `.downloads/hardware-eval-2026-11-05/capture-wave2-tests.log` and
`.downloads/hardware-eval-2026-11-05/reliability-final.log`. The earlier
85-service-test count above is the preserved Wave 1 result.

## D1: preserve unreadable desktop settings before using defaults

**Evidence:** `Preferences::load` previously collapsed file-read errors and JSON/deserialization errors into `Preferences::default()`. It then seeded the normal preference writer with those defaults. A later layout toggle or settings change could replace the original file without preserving the user's folders, favorites, imported media references, device choices, display names, and shortcuts. The original cause was invisible at startup.

**Change:** loading now produces a single session outcome shared with the existing preference writer. Ordinary missing files still use defaults without a notice or backup. Valid files retain the existing sanitization and are not rewritten during load. Reads are bounded to 8 MiB, including a file that grows after its size check. The source path is resolved once, so future writes use the same settings location.

For invalid JSON, invalid UTF-8, or incompatible field types, a uniquely created sibling `preferences.recovery-*.json` receives every original byte and is flushed and synced before future settings saves are allowed. Startup leaves the original file untouched and explains the exact source and recovery paths. A later explicit preference change can save new settings while the recovery copy remains available. Recovery files are not automatically restored or migrated.

If reading fails, the source is oversized, or recovery-copy creation fails, the session uses defaults with an actionable notice and a write guard. The guard is inside the existing `PreferenceWriter` callback: both synchronous save barriers and optimistic Library-layout writes receive their normal failure acknowledgements without writing defaults over the source. Fixing a file externally does not silently lift the guard; restarting explicitly re-evaluates storage. Preference writer admission, coalescing, durable-layout revisions, and rollback behavior are unchanged.

**Integration:** `preferences::take_startup_notice()` is the startup hook for the primary agent's `main.rs` wiring. Initial hotkey-registration errors should append to that notice instead of replacing recovery information.

**Files:** `crates/desktop/src/preferences.rs` and the focused `preferences_recovery.rs` helper; the `preferences_io.rs` state machine was not changed.

**Regressions:** seven new temporary-file/fake-failure tests cover ordinary missing files, valid formatting and fields, invalid/non-UTF-8/schema-invalid input, distinct byte-exact recovery copies, injected full-drive backup failure, injected read denial, preservation of oversized files, and continued rejection of both save modes after external repair. The existing preference-writer ownership and rollback tests remain applicable. No real user preferences or media are loaded by these tests.

Validation status: D1 passed the central Desktop checkpoint, including the recovery/size cases; the latest integrated Desktop suite is **253 passed / 8 ignored**. The startup notice is wired into `Studio` initialization, and initial hotkey-registration failures append rather than replace recovery information. The final integrated result is recorded in [the Desktop test log](../.downloads/plan-continuation/desktop-native-final-tests.log).

**Remaining limits:** recovery copies intentionally are not pruned automatically. Users may remove them after recovering settings. The 8 MiB limit is a protective bound, not a schema promise; larger configurations remain untouched and need manual review. A blocked native read itself cannot be forcibly interrupted by the byte limit. This work does not coordinate concurrent external editors or multiple RCR processes, auto-merge recovered settings, or redesign the crash hook. A valid settings file that becomes corrupt after startup needs a separate change-detection policy before a future save.

The subsequent D2 disk-cache ownership work is documented separately in [the seek-proxy cache audit](SEEK_PROXY_CACHE_AUDIT_2026-11-05.md).

## Desktop source-allocation preflight

**Evidence:** the desktop batch path collected every native monitor frame before converting them to SDR. The compositor's existing 512 MiB destination check happened afterward. A plausible destination size could therefore hide a much larger native HDR batch or conversion peak.

**Change:** the per-monitor desktop-batch path now validates reported monitor geometry before invoking the capture backend. It uses 8 bytes per pixel for an HDR-active monitor on a float-capable backend and 4 bytes per pixel for SDR. Checked arithmetic models the retained native batch, each conversion destination while its source remains alive, accumulated SDR frames, and the compositing destination. The shared limits remain 512 MiB and 32,768 pixels per axis. Single-monitor packed SDR reuse avoids inventing a second allocation. Returned frame payloads, actual formats, and row packing are checked again before tone mapping; in-place conversion accounts for retained native storage even if logical trailing bytes are truncated.

For example, two 8K SDR sources plus their canvas total 530,841,600 payload bytes and fit below 512 MiB. Their HDR equivalents require 663,552,000 bytes at the first conversion peak and are rejected before a native batch call. Normal HDR/SDR conversion remains pixel-identical; the code does not reduce resolution or change tone-map settings to meet the budget.

**Files:** `crates/platform/src/source.rs`, its focused `capture_budget.rs` helper, and shared layout-limit exports in `crates/screenshot/src/{composite,lib}.rs`.

**Regressions:** fake call counters verify that excessive single-monitor dimensions, sparse extreme coordinates, and an excessive HDR batch/conversion peak fail before any native capture call. Positive cases cover ordinary mixed HDR/SDR geometry, the two-8K-SDR boundary, and exact small-source pixels/metadata. Checked-arithmetic tests cover 512 MiB boundaries and overflow without allocating giant buffers.

Validation status: the final central rerun after the payload-retention accounting adjustment passed **16 platform tests**, recorded in [the reviewed platform log](../.downloads/plan-continuation/platform-reviewed-final.log). The screenshot suite passed **4 tests**. Strict all-target Clippy for the four primary packages passed after that adjustment, and the latest Desktop Clippy check passed after restoration. The integrated Desktop suite passed **253 tests / 8 ignored**. These allocation regressions use no native capture or user media; the separate actual-capture baseline above does not extend their memory-accounting scope.

**Scope limits:** this pass deliberately protects only the per-monitor desktop batch. Native-desktop backends, direct monitor/window capture, and region capture retain their existing behavior. In particular, an HDR region's final cropped allocation is not included in this desktop model. Monitor metadata can change before a native call; the returned-payload check cannot undo a backend allocation already made using stale or inaccurate geometry. GPU frame pools/staging, allocator capacity beyond reported payload length, two retained cursor variants, annotations, preview images, and the rest of the application are outside this model. This is a preflight for known CPU payloads, not a claim that total process memory is always below 512 MiB.
