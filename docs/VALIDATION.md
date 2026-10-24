# Windows proof results

Development machine: Windows, NVIDIA GeForce RTX 5070, NVIDIA driver 610.88. Tests use the 2560×1440/60 FPS/25 Mbit/s SDR AV1 profile. These are short local desktop tests, not a game performance benchmark.

For the current capture/Library/editor/tray rationale and official product screenshot references, see [UX research and implementation](UX.md). Dated results below apply to the builds and fixtures named in each entry; they do not automatically validate later source changes.

| Proof | Observed result |
| --- | --- |
| Original headless video proof | 600 encoded frames over 10 seconds; zero missed deadlines; independently decoded AV1 |
| Original GPUI GPU surface proof | 1,800 frames over 30 seconds; zero missed deadlines; 704 published preview frames and 1,860 GPUI draws |
| Native MKV recording with system/microphone | 360 frames over 6 seconds; independently readable video and two PCM tracks; replay saved during recording |
| Replay-only after crash recovery/audio epoch fix | 240 frames over 4 seconds; zero missed deadlines, zero audio discontinuities/errors; one saved replay; FFmpeg independently decoded all three streams |
| Editor fixture | Two-second AV1 MP4 with default AAC mix and two separate AAC stems; linked cuts, fades, crop, position/opacity keys and title; 960×540 composed preview frame |
| Earlier GPUI GPU surface build | 180 frames over 3 seconds; zero missed deadlines; 73 published GPU preview frames and 223 GPUI draws; clean timed exit |
| Current six-second validation script | 360 frames; replay saved during recording; zero missed deadlines/discontinuities/audio errors; independent full decode and monotonic per-stream packet timestamps for recording and replay |
| Editor monitor and cancellation | Preview advanced and cancelled with a full frame queue; FFplay audio-only render completed with SDL's silent test device; subprocesses reaped |

Local proof files and reports are in the ignored `recordings/` directory. They can contain desktop/audio content and should not be added to source control. The original tests occurred during development; source changes after each proof must be revalidated as relevant. Current desktop layout and global shortcut delivery require manual verification; no Computer Use is being used.

## Repeat the short proof

`scripts/verify-windows.ps1` serializes build, native capture/replay, metadata checks, and complete decode. It also checks video keyframe start and monotonic per-stream packet timestamps. The optional `-Audio` switch records both default system and microphone sources; the default is video only.

To generate synthetic independent audio (440 Hz and 880 Hz) from a previously captured IVF and exercise edited export:

```powershell
cargo run -p rcr-media-io --example mux_fixture -j1 -- recordings/capture-proof-002.ivf recordings/tones.mkv
cargo run -p rcr-editor-core --example editor_fixture -j1 -- recordings/tones.mkv recordings/edited.mp4
ffprobe -v error -show_streams -show_format recordings/edited.mp4
ffmpeg -v error -threads 2 -i recordings/edited.mp4 -map 0 -f null NUL
python scripts/verify-editor-tones.py recordings/edited.mp4
```

The tone check verifies that the mix contains both synthetic frequencies and each separate AAC stem suppresses the other source by at least 40 dB. It is intended only for the generated tone fixture.

To repeat audio-monitor validation without playing sound or rendering another export:

```powershell
$env:SDL_AUDIODRIVER = 'dummy'
cargo run -p rcr-editor-core --example editor_fixture -j1 -- --monitor-only recordings/edited.rcr.json
Remove-Item Env:SDL_AUDIODRIVER
```

The silent-device test checks the monitor graph and process lifecycle, not physical-device output quality or A/V clock accuracy. Recent FFplay builds require the PCM demuxer's `-sample_rate` and `-ch_layout` options; the implementation uses those rather than the removed `-ac` alias.

The editor fixture deliberately exercises rendering, not just graph string construction. Unit tests cover bounded/keyframe-aligned replay, PCM boundary trims, transactional linked edits/undo, source restoration, animation continuity at a cut, atomic project replacement, AV1 header parsing, and generated FFI layouts.

## DPDFNet offline microphone processing

The optional denoiser runs on replay save or on selected editor audio tracks at export. It is not instantiated by the live audio worker. The continuous recording and replay ring retain original PCM.

- Independent reference: pinned upstream DPDFNet-2 48 kHz ONNX, SHA-256 `7f0575a5cec0ba4ffd8f8bd657e06d007e4ccdd955d76faab922b9d3291dc14b`.
- Rust spectrum maximum absolute error over the test frames: about `5.34e-8`; recurrent state error about `3.81e-6`.
- Causal PCM maximum absolute error over the one-second fixture: about `7.45e-8`, after removing the 2,400-sample algorithmic delay and flushing the tail.
- One synthetic second processed in about 0.226 seconds in a development run (single worker, FP32). This excludes model initialization and is not a general CPU/quality benchmark.
- A six-second Windows recording continued at 1440p60 while saving a denoised replay: 360 encoded frames, zero missed deadlines/discontinuities/audio errors, one saved replay. Independent checks verify unchanged AV1 packet hashes and system-audio PCM, microphone packet timing and complete decode.
- Editor validation exported a one-second range crossing a linked split to a 960×540 AV1 MP4, with mix/system/microphone AAC tracks. Suppression is persisted per track; unselected tracks retain the normal render path.
- Regression tests cover arbitrary PCM chunking, exact output lengths, reset across gaps, untouched source `Arc` allocations, old project files defaulting to suppression off, and cancellation when the UI disconnects.

Repeat the hardware/save check with unique output names:

```powershell
cargo run -p rcr-windows --bin rcr-capture -j1 -- --seconds 6 --audio system,mic --suppress-mic --output recordings/dn-raw.mkv --save-at 3 --save-replay recordings/dn-clean.mkv
python scripts/verify-dpdfnet-replay.py recordings/dn-raw.mkv recordings/dn-clean.mkv
cargo run -p rcr-editor-core --example editor_fixture -j1 -- recordings/mux-audio-fixture-001.mkv recordings/dn-edit.mp4 --suppress-mic --fast --range-test
```

The replay checker expects a short proof whose history starts at video time zero. Reference-data regeneration is documented in `crates/dpdfnet/NOTICE.md`; ordinary Rust tests need neither Python nor ONNX Runtime.

## Quick clip redesign

The desktop editor now exposes a single fitted clip strip, thumbnails, direct edge trims, reorder, split/delete, speed presets and global audio controls. The old inspector/media-bin/project UI is removed. Per-recording autosaves live in `projects/clips/`; the visible title uses a local date and time instead of the filename. Export defaults to MP4 with a mixed audio track; original recordings keep their independent sources.

The headless `quick_clip_fixture` was run against the native synthetic two-tone MKV. It reads both audio streams, decodes their waveforms and four source thumbnails, trims/splits/speeds/reorders/deletes linked pieces, atomically saves/reopens the edit, and decodes a **1600×900** preview. Its two-second exports independently select game audio at 50% and microphone at 100%, across a join and a 2× section.

The independent `verify-quick-clip.py` check verifies full video/audio decode, two-second sample coverage, volume and pitch on both sides of the join, and more than 40 dB rejection of the muted source. It caught an existing audio graph problem: timestamp-based trimming after tempo/delay could discard leading silence and produce an apparent two-second file with missing audio samples. The graph now uses 48 kHz sample counts, padding and continuous timestamps for each piece before mixing. The corrected proof is `recordings/quick-editor-proof-003-{game,mic}.mp4`.

```powershell
cargo run -p rcr-editor-core --example quick_clip_fixture -j1 -- recordings/mux-audio-fixture-001.mkv recordings/quick-proof
python scripts/verify-quick-clip.py recordings/quick-proof
```

Use a fresh output prefix. The fixture exports at 1280×720 for a compact validation; Original quality in the desktop editor retains the first recording's source dimensions. Unit tests also cover restoring the first trimmed edge, transactional source bounds, global audio settings across joined recordings, unnamed audio stream separation, and video-only sources without invented audio lanes.

Computer Use remains disabled. This proves media processing and model behavior; the redesigned GPUI layout, pointer gestures, physical audio response time and Windows file clipboard pasting still need manual verification. It does not establish the requested ten-second user workflow timing.

This revision passed all 84 workspace tests, workspace Clippy with warnings denied, and a workspace debug build. A follow-up one-second export with pure Rust microphone suppression also decoded all four video/mix/system/microphone streams and complete audio sample coverage. The updated quick-clip audio monitor completed through FFplay's silent test device and reaped its subprocesses.

Shutdown cleanup passed Clippy and linked successfully.

## Persistent playback and pause/resume

The GPUI editor now retains its decoder session and native WASAPI output on pause. A direct sequence preview runs at up to 60 FPS, with two video pieces per batch and one future batch already decoding. This bounds live video input contexts to four and avoids process startup at every cut. Video queues are limited to 64 MiB; individual audio sources buffer at most one second. The audio device stays on its own worker, and native Rust mixing applies track mute/volume without restarting FFmpeg. Late video frames are skipped against the playback clock instead of queued behind real time.

Windows position comes from [IAudioClock::GetPosition](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclock-getposition), divided by the device frequency. Stop/Start preserves this stream clock. Video-only sessions use a pause-aware monotonic clock. Output-device failure reports an error rather than leaving playback indefinitely waiting for audio initialization.

Short headless local measurements at 1280×720 bounds:

| Case | Result |
| --- | --- |
| Earlier player, synthetic native recording | 30 FPS; each start/restart took about 917–1,162 ms |
| Persistent session, silent simulator | 99 presented frames in 1.65 seconds; warm resumes around 7–16 ms; no dropped frames in that run |
| Attached 1200×780 screen recording, new player | 97 frames in 1.65 seconds; warm resumes around 16–18 ms; no dropped frames; 95th-percentile frame interval about 17.2 ms |
| Native WASAPI, audio muted, eight short pieces | 96 frames in 1.65 seconds; warm resumes around 33–75 ms; 95th-percentile interval about 22.8 ms; longest measured interval about 25 ms |
| Native WASAPI, audio muted, full 2560×1440 preview | 95 frames in 1.65 seconds; warm resumes around 33–45 ms; 95th-percentile interval about 21.5 ms; shutdown reaped workers in about 21 ms |
| Cold initialization | About 0.6–0.7 seconds in these runs; opening/seeking is still slower than warm resume |
| Shutdown after warmed/buffered playback | Decoder/audio workers and child processes reaped in about 9–15 ms |

Native tests muted **only audio tracks**, leaving actual video decoding enabled. The timing check waits for a frame beyond the paused position, so an already-due buffered frame is not counted as a resumed frame. Eight-piece testing exercises prefetched decoder-batch boundaries. Four cancellations during process startup also reaped all workers/processes; that separate test took about 7.2 seconds in total, including Windows process creation returning after cancellation. These are development measurements, not a guarantee for every file or PC load, and are not end-to-end GPUI display latency measurements.

```powershell
cargo run -p rcr-editor-core --example playback_bench -j1 -- recordings/mux-audio-fixture-001.mkv --legacy
cargo run -p rcr-editor-core --example playback_bench -j1 -- recordings/mux-audio-fixture-001.mkv
cargo run -p rcr-editor-core --example playback_bench -j1 -- recordings/mux-audio-fixture-001.mkv --native-muted --cuts
cargo run -p rcr-editor-core --example playback_bench -j1 -- recordings/mux-audio-fixture-001.mkv --native-muted --full-size
cargo run -p rcr-editor-core --example playback_bench -j1 -- recordings/mux-audio-fixture-001.mkv --churn
```

The default benchmark uses a silent simulator. `--native-muted` exercises the real default output device with zero audible samples. Tests cover paused-clock freezing, PCM tails, consuming muted sources without replaying stale samples on unmute, and dropping only frames that are already due. All 88 workspace tests passed. Computer Use remains disabled; visual playback and physical A/V synchronization need user verification.

The workspace build and Clippy check passed.

## Supplied design implementation (18 October 2026)

Native Capture, Library, Editor and Settings layouts were rebuilt around the supplied HTML reference. Its nested HTML/CSS and font assets were unpacked as data; bundled JavaScript was not executed. Fonts and authored SVGs are compiled into the app. No Computer Use or screenshots were used, so pixel-level fidelity and mouse interaction are not claimed as visually verified.

- **92 workspace tests passed**, including new tests for atomic preferences replacement, reversible Library removal with overwrite protection, portrait crop bounds, old-project compatibility, undo/redo and preservation of original source/audio clips.
- `cargo clippy --workspace --all-targets -j1 -- -D warnings` passed.
- `design_fixture` decoded a real 320×180 BGRA Library thumbnail, saved/reopened portrait framing and exported a one-second **720×1280 AV1 / AAC MP4** from the existing two-track synthetic recording. FFprobe independently confirmed codec, dimensions, audio presence and exactly 1.000 seconds. The source project was unchanged.
- The native muted eight-piece playback benchmark presented **97 frames in 1.65 seconds**, reported zero dropped frames, a **22.68 ms** 95th-percentile interval and warm resumes of **32.25 / 34.07 / 34.25 ms**. Cold readiness was 674.57 ms; worker/process shutdown took 12.3 ms. These are headless local development measurements, not end-to-end GPUI display latency.

```powershell
cargo run -p rcr-editor-core --example design_fixture -j1 -- recordings/mux-audio-fixture-001.mkv recordings/design-portrait-proof-001.mp4
cargo run -p rcr-editor-core --example playback_bench -j1 -- recordings/mux-audio-fixture-001.mkv --native-muted --cuts
```

The workspace build passed.

The new settings do not silently enable mock game metadata, retention deletion, storage limits or configurable shortcuts. Those capabilities remain pending. Capture's native GPU preview and the persistent editor playback session remain in place.

## Library-open crash regression (18 October 2026)

The redesign set the Export button's hover style twice: once in the shared editor button helper and once on the Export element. GPUI 0.2.2 asserts that a hover style is only set once. Opening any Library item rendered that button and panicked (`hover style already set`, `editor.rs`), aborting the app inside the native event callback. The issue was reproduced by rendering the editor in a hidden native GPUI window before the fix.

The editor helper now accepts the hover color at construction, so each button registers exactly one hover style. Library cards, Capture's recent clips and CLI opening share the same open-recording action. A panic hook appends stack traces to `projects/crash.log`.

The new hidden smoke check first renders the Library, invokes that shared action, repeatedly runs real GPUI layout/paint passes, and waits for a decoded preview image with no editor error. It exits nonzero on failure. It does not use desktop automation, take screenshots or capture the screen. Serial checks passed on the multitrack synthetic MKV, the user's supplied ShareX MP4 and a video-only IVF recording; preview readiness was approximately 1.24–1.28 seconds including the Library transition. This is cold-open timing, not warm play/pause latency. Strict workspace Clippy and desktop unit tests passed.

```powershell
.\scripts\verify-editor-open.ps1
.\scripts\verify-editor-open.ps1 -Recording 'recordings/capture-proof-002.ivf'
```

The application also accepts `--open <recording>` for normal use. The script supplies `--smoke-open` to hide the window, run the check and exit. Both launchable desktop executables were refreshed with the fix.

## Visible trim interaction regression (18 October 2026)

Both trim handles received pointer events and changed clip duration, but the timeline immediately refitted every shortened edit to its full width. On a single piece, the end handle consequently stayed at the same screen position. The UI now retains a stable time extent, draws the grabbed start edge at its drag position until the ripple commits, and offers an explicit **Fit** control. The time ruler, playhead, thumbnails and audio lanes use the same extent. Both grips are larger and have a visible vertical mark.

`verify-editor-open.ps1 -Trim` dispatches events through actual GPUI painted hitboxes and redraws between drag steps. It checks that each painted handle follows the pointer, inward dragging shortens the piece, outward dragging restores the source, audio remains linked, and undo/redo restores both states. It repeats the checks after splitting into two joined pieces, then restores the original project. The checks passed serially on the multitrack synthetic MKV (599 → 509 → 599 frames; joined first piece 299 → 254 → 299) and video-only IVF (600 → 510 → 600; joined 300 → 255 → 300).

The hidden native window now explicitly resizes to 1440×900 before interaction tests: GPUI's Windows `show: false` creation initially leaves `CW_USEDEFAULT` sentinel sizes in its logical viewport. Earlier open-only checks caught the Export hover panic but did not validate usable geometry. Readiness now requires a usable preview and timeline width, and the trim test measures the actual painted grip bounds. The checks neither operate the desktop nor capture screenshots. Future smoke failures return nonzero and stop promptly.

```powershell
.\scripts\verify-editor-open.ps1 -Trim
.\scripts\verify-editor-open.ps1 -Recording 'recordings/capture-proof-002.ivf' -Trim
```

## Claims requiring further measurement

- CPU/GPU utilization, frame-time impact during actual gameplay, encode latency percentiles and throughput on other GPUs.
- Long recordings, audio drift over hours, sleep/wake, endpoint changes and display/driver recovery.
- Native GPU editor decode/composition, physical end-to-end A/V synchronization and color-accurate HDR editing.
- The absence of CPU raw-video readback follows from the implementation and reported code counters; no external GPU trace has been collected.


## Combined SSX toolkit validation (2026-10-23)

The combined Windows build passed `cargo test --workspace -j1` (100 tests) and `cargo clippy --workspace --all-targets -j1 -- -D warnings`. The integration checks cover queued startup commands, cancellation while an interactive step waits for GPUI, image save/history fidelity, import of `.sxcu` definitions without overwriting, preservation of settings on rejected imports, editable `.ssxe` projects, annotation undo/redo, nested Library discovery, and region coordinates with scaling/negative monitor origins.

`scripts/verify-toolkit.ps1` passed on the final executable. It renders the five Tools pages, opens a synthetic 640×360 image in the GPUI editor, and dispatches mouse events through the painted canvas to create a rectangle. The actor publishes an undoable annotation. All seven stages completed in 326 ms in this run. This uses a hidden native window and does not operate the desktop or collect screenshots.

The `rcr-toolkit` `capture-proof` example captured the real 5120×1440 Windows desktop through `windows-wgc`, returning an SDR8 frame. Cold initialization plus capture took 807 ms in this one run. The frame stayed in memory; the proof neither saves pixels nor changes the clipboard. This is a service smoke check, not a screenshot latency benchmark. Toolkit production services persist between jobs and refresh when configuration/imports change.

The final executable also passed `scripts/verify-editor-open.ps1 -Trim`: one-piece left/right trims restored 599 → 509 → 599 frames, and joined two-piece checks restored 299 → 254 → 299 frames. Painted grips followed input; audio linkage and undo/redo remained correct. The run completed in 1,626 ms.

The region popup uses its owned native window handle to cover the screenshot's physical desktop rectangle, including negative origins. The pure coordinate test passes; interactive selection on mixed-DPI/HDR hardware, tray menu interaction, live remote uploader accounts, long-running background capture and the combined Linux UI have not been validated by these checks. OCR/pinning remain upstream stubs. Imported SSX workflow hotkeys, SSX history migration and GIF recording are not integrated.

Builds default to one Cargo worker and disable incremental caches. The first broad rebuild filled the system disk and failed while writing a PDB. Removing only this repository's generated `target/debug/incremental` cache recovered 5.6 GB; the subsequent builds and checks succeeded. No source files, recording files or settings were removed.


## Screenshot overlay UX revision (2026-10-23)

Research used the official [ShareX region capture guide](https://getsharex.com/docs/region-capture), [Flameshot interaction overview](https://flameshot.org/) and [Flameshot keyboard shortcuts](https://flameshot.org/docs/guide/key-bindings/). The revised capture mode keeps selection and annotation together over the frozen desktop. Region release retains the selection; a floating toolbar provides drawing, color/thickness, undo/redo and direct copy/save. Advanced editing of opened files remains separate.

All 104 workspace tests and strict Clippy checks passed. New tests cover selection creation/movement/resizing/clamping, square selection, exact pixel cropping and rejection of invalid crops, and native 5120-pixel-wide previews. Desktop previews retain native pixels up to a 16-megapixel / 64 MiB raster budget, independent of the output crop's full resolution.

The native hidden-window smoke now creates a separate synthetic 1280×720 capture overlay. It asserts that the painted canvas times window DPI exactly matches the source dimensions. It selects a region, checks that mouse release keeps the overlay open, moves it, resizes through the lower-right handle, draws an arrow, dispatches Ctrl+Z and Ctrl+Shift+Z, and confirms with Ctrl+C. The returned crop is 670×308 and contains the annotation. The result never enters the system clipboard or a file during this test.

These tests exposed two concrete framing defects: synchronous SetWindowPos inside a leased GPUI window lost resize callbacks, and GPUI's popup windows inherited invisible titlebar insets. Placement now runs outside App/window leases; the vendored Windows popup uses WS_POPUP and skips custom titlebar insets. Activation uses the owned native window without restoring GPUI's normal-window initial placement. Capture-phase mouse release also prevents a floating toolbar from swallowing the end of a resize gesture.

Validation uses native layout, paint and input APIs in a hidden window; no desktop automation or screenshot collection is involved. Interactive operation across mixed-DPI/HDR monitors remains a manual validation item. Screenshot-triggered annotation no longer opens or activates the main Tools image editor.

## Annotation performance revision (2026-10-23)

The screenshot editors previously rasterized, converted and replaced the whole desktop image on every command, including tool selection. A synthetic 5120×1440 small-arrow drag measured a 425.13 ms median / 437.28 ms p95 raster publish and 29,491,200 bytes of image data per update in the development build.

Both GPUI annotation surfaces now retain 512×512 GPU tiles and replace only dirty tiles. The actor consumes SSX dirty regions and session events, merges undelivered tile updates, and skips rasterization for metadata-only commands. View/base-image changes reset the surface; spotlight changes invalidate the full visible canvas, including removal of the last spotlight. The worker batches input at an 8 ms cadence and preserves individual freehand samples. Motion no longer independently requests a redundant UI redraw before the worker result arrives. Rendering remains on a bounded two-thread pool, with optimized raster kernels in development builds. Local SSX changes avoid generic sampling of opaque native-scale base regions and redundant output allocation/copying.

The final synthetic benchmark measured a 0.95 ms median / 1.30 ms p95 raster publish, 28.05 ms initial publish, and 1,048,576 bytes per small-arrow update. The worker-response p95 was 16.64 ms, including the benchmark's polling/scheduling delay. These are local worker measurements with synthetic pixels, not end-to-end GPUI input-to-display latency or GPU transfer timings. Large effects and full view changes still require more work than a small annotation.

All 107 workspace tests passed. New tests reconstruct the incremental framebuffer and compare it byte-for-byte with full renders over patterned pixels, edits, undo/redo, grayscale, rotation, tile-boundary blur/pixelate/magnify, spotlight deletion and zoom. They also cover undelivered initial/dirty tile merging and preserving intermediate freehand samples. Strict workspace Clippy passed.

The rebuilt executable passed `verify-toolkit.ps1` (native overlay selection/move/resize, annotation undo/redo and exact cropped output) and `verify-editor-open.ps1 -Trim` (both edges of single/split video clips, linked audio, undo/redo). These run hidden native GPUI windows without Computer Use. The launchable performance build is `target/debug/rcr-desktop-perf.exe`.

## ShareX-style utility coverage (2026-10-23)

The Utilities page and tray Tools menu implement the straightforward tools in the supplied ShareX menu. [Coverage and limits](UTILITIES.md) distinguish this from complete ShareX parity. DNS changes and advanced/batch profiles remain pending.

All 113 workspace tests passed, including streaming hash vectors/comparison, Unicode QR encode/decode, pixel coverage of odd-sized image splits, refusal to overwrite image outputs, escaped/depth-limited/cancellable indexes and reversible border removal on an owned hidden synthetic Win32 window. Strict workspace Clippy passed.

`utility-proof` exercised synthetic image viewing, beautification, thumbnails, conversion, effects and combination. Windows OCR extracted `RCR TEST 12345` from the generated image. A two-second sample of the existing synthetic multitrack recording converted successfully to MP4/H.264/AAC, WebM/VP9/Opus and GIF. Independent FFprobe inspection found three streams, including both separate audio tracks, in MP4 and WebM; GIF correctly contained only video. A one-second timestamp produced a 320×180 thumbnail. Temporary proof files were removed when the fixture scope ended. No desktop screenshot, clipboard access, network upload or computer automation was used.

The new native smoke passed all 25 stages: six Tools panes, all 17 utility pages (including synthetic image and inspector result controls), screen color/ruler/pin-region input, pinned-image rendering, all seven monitor colors, and the existing screenshot annotation/cropping regression. Video opening and both trim edges on single/split clips also passed with linked audio and undo/redo. These checks used the rebuilt `target/debug/rcr-desktop-utilities.exe`.

The native verification scripts accept `-Executable target/debug/rcr-desktop-utilities.exe` to target this build. Normal Cargo builds retain the original `rcr-desktop` target. Quit the existing app through its tray menu before switching executables so its global capture hotkeys are released.

## Persistent annotation properties and live region feedback

The capture overlay now shows contextual properties below its toolbar whenever an annotation/tool is active. Properties follow the selected object instead of stale front-end defaults: color, stroke, font, fill, opacity, blur strength, pixel block size and magnifier zoom are shown where applicable. Choosing a drawing tool clears the old object selection so its default settings can be edited before drawing. Highlight/step color edits also update their actual fill. Right-click is no longer required to reveal controls.

All annotation tools now display selected-object outlines. The engine reports the live creation bounds; the capture overlay additionally draws immediate pointer-driven box guides for region tools while the raster worker catches up. Both screenshot editors use a contrasting black/white boundary with colored corner markers. These guides are GPUI painting only and never enter image exports.

All 21 toolkit tests and strict workspace Clippy passed. New tests verify selection-specific properties, live blur/pixelate/highlight/spotlight/magnifier bounds, strength undo/redo, and identical image pixels before/after requesting the overlay. The hidden native GPUI smoke passed all 25 outer stages and 14 capture interaction phases. It checks that controls are painted without right-click, draws blur/pixelate with live bounds before release, selects the blur object, clicks its actual strength button, undoes the change and confirms the cropped output.

The launchable build is `target/debug/rcr-desktop-annotations.exe`. Validation used `verify-toolkit.ps1 -Executable target/debug/rcr-desktop-annotations.exe`; no Computer Use was used.

```powershell
cargo run -p rcr-toolkit --example annotation-bench -j1
cargo test --workspace -j1
cargo clippy --workspace --all-targets -j1 -- -D warnings
```


## Workflow browser revision (2026-10-23)

Tools → Workflows now uses a grouped list and a separate detail pane, replacing the wrapped preset buttons and duplicated toggle/debug lists. Each enabled action appears once in execution order, with move/remove controls and compact buttons to append common actions. The run control stays beside the selected workflow title. Follow-up upload actions use readable labels; folder and destination links provide context without exposing internal enum names. Recording entries route to Capture and explain that recording/export settings live there. Small windows stack the workflow list above the details.

Two focused desktop regression tests cover custom-name preservation and edits to ordered imported steps, including duplicate-add and invalid-index handling. Strict desktop Clippy passes. The hidden native GPUI smoke selects all ten default workflows by dispatching pointer events against their painted bounds, checks separate sidebar/detail columns and a compact run button within the viewport, then completes the existing 25-stage toolkit/overlay suite. These checks do not execute workflows, upload files, change saved settings, or use desktop automation. Pixel appearance and the compact stacked layout remain manual review items.

The review executable is `target/debug/rcr-desktop-workflows.exe`; existing running executables were left untouched.


## Paused seeking revision (2026-10-23)

Paused seeks previously waited for a 60 ms quiet period and restarted the complete video/audio transport. On `recordings/av-recording-001.mkv` (2560×1440, AV1, 60 FPS, separate audio), the final comparison measured full-transport first-frame latencies of 787.5, 1381.4, 1145.9 and 1295.9 ms at timeline frames 0, 90, 270 and 180, starting three decoder processes per seek. The persistent NVDEC path measured 195.4 ms cold, then 173.2, 119.4 and 157.5 ms. Cached revisits measured 1.5–2.9 ms. These are local development-build request-to-BGRA timings, excluding GPUI polling/paint; they are not universal latency guarantees.

A dedicated worker retains the demuxer/decoder, coalesces to one pending request and one pending result, and keeps up to 48 MiB / 24 frames in an LRU cache. CUVID emits CUDA surfaces so discarded preroll frames stay on the GPU; only the requested frame transfers to CPU BGRA before GPUI upload. This editor path is not zero-copy. Interactive preview uploads are capped at 1280×720; export resolution and playback sizing retain their existing behavior. Timeline mutations change a worker revision and invalidate cached compositions without spawning a decoder per trim event. Results from superseded project revisions are rejected. Video seeks do not start audio devices or audio decoders.

The Windows fast path checks pinned FFmpeg 7 library majors before using public struct prefixes. It requires a single active untransformed video layer at the source FPS and 1× speed. Source-file/stream switches reopen only the relevant reader. CPU decoding is available when NVDEC cannot initialize; missing/incompatible shared libraries, effects, retiming and FPS conversion use the video-only FFmpeg graph. The final video frame is retained when audio/container duration extends slightly beyond video. Other FFmpeg ABI majors, hardware outside this NVIDIA system and remote-file interruption behavior have not been validated.

`seek_proof` compares rendered BGRA pixels against the existing video graph, drives 120 rapid requests, verifies final-target delivery and intermediate presentation, and exercises trim, speed, split/reorder, opacity, cache invalidation and cancellation. A small AV1 fixture changes luminance every frame to expose off-by-one timestamps. Native-frame mean BGRA error on this fixture is below one byte (GPU resizing/conversion differs slightly); filter-graph fallback matches exactly.

All 22 editor-core unit tests pass, including existing transport clock/audio/presentation checks and the bounded cache regression. Strict Clippy checks pass for desktop/editor-core and all their targets. The hidden native GPUI trim smoke also drags the painted seek ruler, asserts that it follows the pointer immediately through the scrub worker with no transport/debounce, and verifies the project is unchanged. All four trim/restore, linked-audio and undo/redo cases pass. Initial editor readiness in that run was 471 ms. No Computer Use was involved.

Review build: `target/debug/rcr-desktop-seeking.exe`.

The cut-playback regression presented 99 frames in 1.65 s, with an 18.5 ms p95 presentation interval, zero dropped frames and warm resumes of 20.4, 14.6 and 15.7 ms. All transport workers/processes were reaped in 23.8 ms.


## Continuous timeline dragging revision (2026-10-24)

The prior seek cache retained only requested BGRA frames. Moving to an unvisited adjacent frame, especially backwards, could seek and flush the AV1 decoder again. The native reader now retains decoded source surfaces, including preroll, in an LRU cache. The accounted GPU cache budget is 192 MiB (with conservative pitch alignment), capped at 180 references; the persistent software decoder uses a 32 MiB surface budget. This is separate from the existing 48 MiB BGRA cache and decoder/driver-owned resources. A cached surface is converted for display without changing the demuxer/decoder position. Source surfaces remain valid across project revisions; rendered BGRA compositions are still invalidated by edits.

After 35 ms without a request, the same worker warms up to 0.75 seconds ahead, one decoded frame per pending-input check. Warming neither downloads frames nor allocates BGRA previews. Cached preroll supplies backward movement. The UI now receives a bounded asynchronous notification immediately when a seek result is published, while its ordinary timer continues to service playback and media jobs. No proxy files or extra decode workers are created for warming. Large jumps beyond the resident neighborhood still need source decoding; these measurements concern continuous nearby dragging, not arbitrary whole-clip jumps.

Validation used the user's actual `target/debug/recordings/replay-1792789538677.mkv`: 2560×1440 AV1, 60 FPS, 119.57 seconds. `playback_bench --drag` drove 93 forward/backward requests around the middle of the replay after warming. Decoder-side median was 3.17 ms, p95 4.78 ms and maximum 5.15 ms, with 80 neighbor-surface hits and 200,724,480 accounted bytes (under 192 MiB).

A new hidden native `--smoke-scrub` fixture dispatches pointer events through the painted ruler, waits for the requested frame to reach the actual GPUI `paint_image` call, and measures that call's timestamp against input dispatch. This includes UI delivery, polling/event handling, image preparation and paint scheduling; it does not measure monitor scanout. On the final executable, 93 drag updates measured 14.74 ms median, 15.55 ms p95 and 15.77 ms maximum. The earlier coarser render-observer loop measured around 31–33 ms; the final fixture uses the paint timestamp directly and samples more frequently.

The AV1 luminance-counter proof continues to pass frame/color comparisons through neighboring-cache hits, trims, speed fallback, split/reorder and opacity fallback. Its 120-request rapid-input phase delivered 96 intermediate frames and reached the final target in 423 ms. Decoder shutdown completed in 70.4 ms. All 22 core tests and strict Clippy checks for desktop/editor-core with all targets pass. Native linked-audio trim/restore and undo/redo checks pass in all four cases; final editor readiness in that fixture was 370 ms. No Computer Use was used.

Review executable: `target/debug/rcr-desktop-scrub.exe`. Run the native paint-latency fixture with `scripts/verify-editor-open.ps1 -Scrub -Recording <recording> -Executable target/debug/rcr-desktop-scrub.exe`.

## Whole-recording seeking revision (2026-10-24)

Historical checkpoint: the following 720p/8 Mbps recipe, size limits and per-session lifetime were replaced. See “Current seek recipe 7 and remaining latency limits” below for the current candidate and bounds.

Both distant timeline clicks and wide forward/backward drags now use a disposable all-intra H.264 preview once background preparation finishes. Original AV1/NVDEC seeking remains available while it prepares. The copy is 720p, 8 Mbps CBR, has no B frames, preserves source frame timestamps/rate, and is decoded by a persistent single-threaded software reader. Each frame can be independently sought without AV1 GOP preroll or CUVID flush/reinitialization. Proxy mapping applies only to the existing native single-layer, source-FPS, 1×, untransformed path. Effects and retiming retain the original graph; exports/playback retain the original media. This is a lossy preview optimization, not a new zero-copy path.

Preparation uses one below-normal-priority CUDA/NVENC FFmpeg process and starts when the scrub session opens. It currently accepts one source video up to four minutes long, requires 1 GiB available temporary space, enforces a 256 MiB acceptance budget and 90-second timeout, and publishes only successful complete outputs. The separate emergency FFmpeg file-size limit is above the acceptance budget so truncated outputs cannot be accepted. Missing encoders/libraries, insufficient space, preparation failure or ineligible compositions retain existing decoding. `RCR_DISABLE_SEEK_PROXY=1` disables preparation. Normal cancellation kills/reaps the child and deletes its temporary directory after closing the reader. A process crash can leave a temporary directory. Preview copies are per session and regenerate after closing/reopening the editor; persistent reuse and longer/multiple-source coverage remain future work.

On the user's actual 119.57-second 1440p60 AV1 replay, a 720p proxy took about 9.1 seconds to prepare and occupied 119,968,029 bytes. The toolbar displays “Optimizing seeking…” during preparation without moving the playback controls. A 301-request decoder benchmark covers a coprime permutation of distant targets and broad forward/reverse sweeps through the entire recording, with 4.70 ms median, 4.87 ms p95 and 24.63 ms maximum. These are local timings after preparation, not the earlier warmed-neighborhood-only benchmark.

The hidden native GPUI `--smoke-random` fixture now checks that pointer events actually move the playhead to their intended targets and that a fresh `paint_image` timestamp follows dispatch. It exercises distant mouse-down/up clicks, then continuous full-timeline dragging. The final executable passed 300 updates: 11.34 ms median, 12.15 ms p95, 13.11 ms maximum to GPUI paint (excluding monitor scanout), with clean process exit. Nearby dragging during active preparation also passed at 14.53 ms median, 15.36 ms p95, 15.70 ms maximum across 92 updates. Duplicate pointer targets are skipped rather than counted as zero-latency samples.

The frame-counter proof with `--proxy` matches original graph pixels exactly at tested positions, including trim offsets and split/reorder. Speed/opacity correctly use original graph fallback. It delivered 119 intermediate frames under 120 requests spaced 3 ms apart. Completed-reader shutdown took 7.4 ms; cancelling an active preparation process took 515.9 ms. Native decoder teardown occurs outside the request mutex and before proxy readiness is published. Editor destruction now joins the scrub worker, fixing an exit access violation exposed by the new native fixture.

All 22 editor-core tests and strict desktop/editor-core Clippy checks with all targets pass.

```powershell
cargo run -p rcr-editor-core --example playback_bench -j1 -- target/debug/recordings/replay-1792789538677.mkv --random
cargo run -p rcr-editor-core --example seek_proof -j1 -- .downloads/seek-frame-counter.mkv --proxy
./scripts/verify-editor-open.ps1 -Random -Recording target/debug/recordings/replay-1792789538677.mkv -Executable target/debug/rcr-desktop-fastseek.exe
```

## Product workflow revision: source review (2026-10-24)

The [UX record](UX.md) maps the current implementation to official ShareX, Flameshot, NVIDIA, Medal and SteelSeries references, including screenshot-bearing pages. The research supports workflow decisions; it does not establish feature parity or benchmark competitors. No competitor application or desktop automation was used for this source review.

The current source contains state-aware capture controls and tray feedback, configurable global shortcut hints, direct tray Library/Settings navigation, Library keyboard navigation/selection and capture display-name editing. Capture display names are preference metadata; media filenames and project sidecars are preserved. Tray state is an owned snapshot, and unchanged visible state performs no native icon/menu updates. Replay save enablement excludes off, starting and finishing states; replay disablement is unavailable during a full recording.

New pure tray tests cover stale inactive statistics, capture-state labels and action enablement, and custom/unbound shortcut hints. Library source also contains a hidden native keyboard fixture covering card navigation/selection, text-input isolation, rename-dialog focus containment and cancellation without changing capture files or saved names. Session-cache tests cover Undo/Redo and linked-edit mode across clip switches, and eviction of one least-recently-used inactive editor rather than clearing all history. The earlier dated entries remain historical evidence for their named builds; new completed checks are listed below without borrowing an earlier pass count or executable hash.

Tray registration and real menu interaction, mixed-DPI/HDR interaction and assistive-technology behavior require separate validation. Native layout/input fixtures establish only the assertions they exercise; they do not establish pixel-level visual fidelity or physical input-to-display latency.

## Product workflow and reusable seek-cache checks (2026-10-24)

Historical checkpoint: the 512 MiB/256 MiB seek-cache limits below were later replaced by recipe 7's 3 GiB/2 GiB limits. The workflow receipts remain specific to the tested executable.

The rebuilt desktop executable passed the Library keyboard fixture in 2,236 ms, the 25-stage Toolkit fixture in 2,224 ms and the isolated damaged-edit recovery/reset fixture in 2,673 ms. The recovery fixture works on a unique copy, verifies a recovery backup of damaged sidecar bytes, opens the original media, trims/resets and checks Undo/Redo while preserving the user's original media and sidecar. These are hidden native GPUI fixtures; no Computer Use was used. They do not validate physical tray-menu interaction or manual visual fidelity.

Seek proxies now persist across sessions in a bounded 512 MiB cache with source-identity checks, completion markers, reader leases and LRU eviction. This supersedes the per-session proxy lifetime described in the earlier whole-recording seeking entry. Each accepted proxy remains below 256 MiB; source eligibility, lossy 720p preview quality and original-media playback/export behavior remain limited as described there.

On the tested AV1 source, cold persistent-cache preparation took 9.40 seconds and a warm reopen took 0.02 seconds. The warm 301-sample decoder benchmark measured 4.6822 ms median, 4.8514 ms p95 and 23.2918 ms maximum. Preparation/reuse timing and per-request decoding latency are separate measurements; neither includes monitor scanout. These are local results, not competitor benchmarks or evidence for every project/source configuration. Later checkpoint results follow; the final review executable identity remains to be recorded.

## Subsequent regression checkpoint (2026-10-24)

The completed checkpoint passed 36 desktop tests, 31 editor-core tests and 21 toolkit tests. Strict Clippy passed for desktop/editor-core with all targets before the latest import and annotation-batch changes. A subsequently added core cancellation regression has not yet been included in these reported test runs. These counts describe this checkpoint, not a claim that the newest source/build has completed its final checks.

| Completed check | Result and measurement scope |
| --- | --- |
| Hidden native editor keyboard fixture | Passed in 5,127 ms. Exercises the native editor's painted controls and keyboard handling. |
| Recorder A/V source with cuts, playback regression | Cold readiness 672.8 ms; 98 frames presented in 1.65 s with zero dropped frames. Warm resume measurements were 21.212, 15.232 and 13.677 ms. Transport workers/processes reaped in 13.5 ms. These are local playback checks, not physical end-to-end A/V synchronization measurements. |
| Native whole-timeline pointer seeking | 300 samples: 14.21 ms median, 15.43 ms p95 and 16.06 ms maximum from dispatched pointer input to GPUI paint; fixture elapsed time 5,203 ms. Monitor scanout is excluded. |
| Source-counter frame correctness and latest-request delivery | The reported reference comparisons had mean BGRA error 0, including edited-timeline checks. The 120-request coalescing burst reached its final target in 421.6 ms; this duration includes the deliberately paced request sequence, rather than representing one seek. Decoder shutdown took 7.3 ms; the preparation-shutdown check took 3.6 ms. |
| Media-task cancellation | Nine serial cancellation jobs completed in 562–667 ms each, including FFmpeg startup. Normal waveform decoding returned 1,000 bins. These timings do not establish instantaneous cancellation or a universal shutdown bound. |

The native fixtures use the app's own GPUI layout, painting and input dispatch with Computer Use disabled. Explicit UI render fixtures, when requested, read only the app's owned render target rather than the desktop. Their assertions and owned-window pixels do not establish manual usability across all displays or physical input-to-monitor latency.

The latest forced-autosave-failure/retry fixture, import handling and optimized annotation-batch changes still awaited their native validation at this checkpoint. Later completed checks follow; source existence alone is not counted as a passing run. Final combined results and executable identity will be recorded separately.

## Autosave and media export checkpoint (2026-10-24)

The native forced-autosave-failure/retry fixture passed in 5,028 ms. It obstructs only a uniquely copied capture's owned sidecar with an empty directory, waits for the real save worker to report failure, confirms that the latest project remains pending and the capture is named, and verifies that an unrelated media error survives. Removing the owned obstruction and invoking Retry transitions through Saving… before the latest ACK reports Saved. Reloading the sidecar verifies the durable project; original media and original edit-sidecar checks remain unchanged, and the owned fixture is cleaned up. This run uses hidden native GPUI with Computer Use disabled.

All 33 editor-core tests passed in the later run, including the added cancellation/bounds coverage. This supersedes the earlier 31-test checkpoint for editor-core; it does not imply a new desktop test count or final all-target Clippy result.

`quick_clip_fixture` passed end-to-end trim, split, speed, reorder, delete and reopened-project checks, a 1600-pixel preview and two independent exports. `editor_fixture --fast --range-test --suppress-mic` also passed its one-second export. Independent FFprobe inspection confirmed AV1 at 960×540 plus three AAC streams named Mix, System audio and Mic. Preview frame 45, playback, cancellation and worker/process reaping passed in that fixture. These are actual media-path checks, rather than tests that only inspect an export command or project structure; they remain local fixtures and do not establish long-session synchronization or universal performance.

Current source additionally restores focus after accepting Add clip, resets export options when opening another clip, refreshes named picker choices after asynchronous Library scanning and caps the recent list at 64 with a Browse option. The extended native keyboard fixture adds actual choice acceptance and Undo assertions; its new run result remains to be recorded. Import/reference-removal/scanner changes and optimized annotation batching likewise retain their pending native-validation status until separately reported. Earlier passes are not attributed to these later changes.


## Library and annotation responsiveness checkpoint (2026-10-24)

The desktop unit suite now passes all 52 tests, including imported-reference transaction rollback, asynchronous discovery cancellation, reversible mixed local/external removal and Unicode display names. The hidden native Library import fixture passed in 4,973 ms in its own marked workspace: production background import, duplicate/skipped-file counts, actual video/image previews and card selection, an unavailable named favorite, reference-only removal and persisted Undo, and latest-scan replacement. Original media and fixture-sidecar hashes remained unchanged. Run it with `scripts/verify-library-import.ps1 -Executable target/debug/rcr-desktop-polish.exe`; the evidence directory is retained rather than deleting user files.

After event-driven annotation delivery replaced 12/16 ms polling, the native toolkit fixture passed all 25 stages in 2,196 ms. It checks overlay selection/move/resize, persistent properties, blur/pixelate draft guides, mouse/Enter/Space property changes and Undo, tool shortcuts, text Escape, toolbar recovery, and a 670×308 cropped output. The extended editor keyboard fixture passed in 7,853 ms: split, native focus/selection, audio slider fine/coarse limits and Undo, export-menu Escape, named Add clip picker acceptance, asynchronous append and exact Undo, durable save and owned-fixture cleanup. These fixtures dispatch native GPUI events with Computer Use disabled; they do not establish physical input-to-monitor latency.

A controlled CPU-only annotation actor comparison used the same 2560×1440 input and unchanged consumer polling before/after batching and globally anchored stroke rendering. Three blur draws reduced their final CPU render from 27.0–29.0 ms to 10.2–10.5 ms; pixelate reduced 30.0–31.9 ms to 11.0–11.6 ms. A 1,024-point zigzag pen burst retained all points while its final render reduced 104.5 ms to 47.8 ms, and command-to-preview completion reduced 326.1 ms to 179.3 ms. This comparison excludes GPU upload/presentation, observes latest previews rather than every intermediate render, and does not attribute additional gains to the later UI notification change. Small rectangle rendering became slightly slower in this comparison; global stroke rasterization preserves stable antialiasing across tile/viewport boundaries. Local JSON evidence: `.downloads/research/annotation-before.jsonl` and `annotation-after.jsonl`.

The renderer's exact full-vs-batched viewport comparison passes base pixels, overlapping blur/pixelate/magnify/spotlight, pen strokes, translucent/rotated/dashed shapes, fractional scaling and nonaligned pans at both 1 MiB and 16 MiB batch budgets. Later resource-bounding changes, final strict Clippy and the final normal executable still require their own recorded checks.


## Source recovery, keyboard access and bounded preview checkpoint (2026-10-24)

All 56 desktop tests, 39 editor-core tests, 26 toolkit unit tests and three public renderer integration tests passed. Strict all-target Clippy for desktop/toolkit/editor-core passed with `-D warnings` after these integrations. The public renderer tests cover nearest/bilinear/Auto global sampling, padded translucent bases, reused stroke scratch, hairlines, gradients and glyph fills. Their first run exposed real clipped-fill differences; canonical shape/glyph fills now pass exact crop comparisons. Later recorder/process-audio and settings changes require their own checks rather than inheriting these results.

The native source-recovery fixture passed in 6,444 ms: an owned capture copy was opened, edited and autosaved, then its modification timestamp changed without changing its bytes. Reopening through the cached-session path blocked use of stale source metadata and retained the previous project/history and sidecar. Explicit Open original preserved disk and, when queued, pending-memory edits as separate byte-exact recovery files before a freshly probed source opened. Damaged JSON recovery, Reset Undo/Redo, durable save and original-media/original-sidecar verification also passed; successful owned fixtures were removed. The later native autosave failure/retry test passed in 4,555 ms and the keyboard/picker/append/Undo fixture in 8,045 ms.

The updated native toolkit test passed all 25 outer stages in 10,000 ms. Its workflow tests traverse real Tab/Shift+Tab stops and activate all ten choices with Enter and Space. Overlay checks use F6 to reach controls, traverse properties, activate strength with Enter/Space without finishing the capture, isolate toolbar typing and restore canvas focus. The later extra Duplicate/Undo fixture and standalone swatch changes remain pending native execution at this checkpoint.

Interactive annotation workspaces now use an 8-million-pixel (32 MiB at four bytes/pixel) budget per workspace/layer. When an effect dependency exceeds this, raster density reduces while logical zoom, viewport, pointer targets and overlay coordinates stay unchanged. Tests cover an 8K canvas at 8× zoom, oversized/negative viewports, density transitions, fractional raster coverage and unchanged export dimensions. This is not a whole-process memory cap: base images, stored annotations, scratch buffers and multiple effect layers remain additional. Exports retain their existing full-resolution renderer.

The final controlled annotation probe retained the earlier consumer and input: blur final renders were 9.7–10.3 ms, pixelate 10.3–10.8 ms, and 96-sample pen strokes 6.6–6.8 ms. The 1,024-sample pen burst completed in 173.6 ms with a 46.6 ms final render and every sample retained, compared with the original 326.1/104.5 ms. GPU upload and presentation are excluded; these are local development-profile samples, not competitive end-to-end latency claims. Evidence: `.downloads/research/annotation-final.jsonl`.

A fresh actual export after source preflight passed `editor_fixture --fast --range-test --suppress-mic`: one second of AV1 at 960×540, plus AAC Mix, System audio and Microphone tracks confirmed independently with FFprobe (MP4 labels are `handler_name`). Frame-45 preview, advancing playback and decoder cancellation/reaping passed. Output: `.downloads/source-guard-export-20261024.mp4`.

The normal `target/debug/rcr-desktop.exe` was restored and built (temporary Cargo alias removed). Owned-window captures at 1100×680 show saved Replay Edit/Show in folder actions and source recovery after dismissing its error toast. That review caught an unanchored absolute placeholder; `.inset_0()` fixed the missing center message/button. The dismissed-toast proof now visibly retains Open original. This review executable's SHA-256 is `[redacted local build hash]`. Subsequent source edits are not represented by that hash. No Computer Use was involved.

## Application audio and capture lifecycle checkpoint (2026-10-24)

The complete workspace test run passed 209 tests. After the final picker search guard, the desktop suite passed all 59 tests; the separately built process-audio example's argument test also passed. Strict `cargo clippy --workspace --all-targets --offline -- -D warnings` passed after these changes. These targeted later runs are recorded separately rather than described as a fresh complete workspace run.

Application audio uses Windows process-loopback activation with an exact PID/creation-time fingerprint and the target's process tree. It replaces the desktop track while retaining the independent microphone track. The runtime selection resets to Desktop when RCR restarts; the picker explicitly asks the user to reselect after either app restarts. Exiting or replacing the selected process reports an error and never falls back to capturing the desktop. An initial actual activation test exposed unsupported `RoGetAgileReference(IAudioClient)` marshaling. The final implementation transfers the owned interface only between verified threads in the same MTA and releases cancelled/late results in that apartment.

Audio-only native tests on Windows build 26200.9711 used two owned, quiet FFplay tone processes and did not capture the desktop, microphone or write captured audio. Direct selection produced 143,040 frames / 298 packets at 48 kHz, zero discontinuities and a selected-440 Hz to unrelated-880 Hz amplitude ratio of 3,483.236. Selecting an owned parent of the tone process verified process-tree inclusion with a ratio of 3,650.632. A deliberately wrong creation fingerprint was rejected before activation in 1.2344 ms. Target exit delivered 67,680 valid frames / 141 packets before an explicit no-fallback error; worker join took 0.0061 ms. These are controlled local audio tests, not proof of long-session synchronization or continuing full-video/microphone recording after an application exits. Reproduce with `scripts/verify-process-audio.ps1`, optionally `-IncludeChildren` or `-ExpectExit`; JSON/text evidence is retained under `.downloads`.

The final native audio-picker fixture passed in 368 ms. It dispatches actual GPUI keyboard events against a synthetic entry: opening from the source button, typed filtering, no-match Enter staying open, selecting the precise process fingerprint, Escape/focus restoration, microphone independence, and opening/late-selection guards during active capture. `scripts/verify-toolkit.ps1 -AudioPicker` does not enumerate or capture user applications. An owned render-target screenshot at 1100×680, `.downloads/research/rcr-audio-picker-1100.png`, was visually inspected. The final ordinary toolkit fixture also passed its 25 outer stages / 17 internal overlay phases in 9,538 ms, including selected-object Duplicate through keyboard activation and Undo.

Accepted replay-save commands now drain on Stop/Replay→Record finalization, with explicit path-specific errors for busy/invalid requests and cancellation on application shutdown. Audio setup precedes writer creation, and explicit Stop before the first encoded frame joins any created writer and reports an empty cancelled session. Queue and startup-resource tests pass; this checkpoint does not claim a new physical desktop-capture test of those races. Settings also recheck capture state when a folder chooser returns, roll back a failed folder preference save and avoid reporting successful defaults when persistence failed.

The reviewed normal desktop executable has SHA-256 `[redacted local build hash]`. Later source changes, including a recording validation-message correction, require another build before this hash can represent them. All native tests, rendering and media jobs were run serially; Computer Use remained disabled.

## Interruption, file safety and asynchronous Library checkpoint (2026-10-24)

The recorder suite passed 22 tests with its hardware-only GPU proof ignored by default. Tests cover the fixed five-second recovery deadline (including repeated unconfirmed access loss), 250 ms retry pacing, terminal error classification, both stream-finalization attempts despite prior failures, and retention of the one packet rejected by a full writer queue. Final delivery retries do not double-count packets. A disconnected disk writer no longer prevents the remaining final packets from entering the replay ring; writer/finalization errors remain explicit. Matroska's six tests include a destination created during recording: its exact bytes survive while the completed recording remains recoverable as `.partial`. IVF has corresponding publication tests. The later save-on-stop cumulative-success fix still requires its next combined check.

DXGI recovery now recreates duplication on the same healthy device, adapter and output, preserving the encoder textures, clock, source image, audio workers and replay ring for unchanged SDR geometry. A fresh copied and released frame confirms recovery and requests a keyframe. Geometry/rotation/HDR changes, device removal and expiry remain explicit terminal failures; this does not implement arbitrary monitor/device migration. An explicitly invoked GPU-only native proof **failed** at its initial frame wait: display opening succeeded, but no initial frame arrived within five seconds. It did not reach either injected recreation. That result is retained as a failed environmental/runtime check, not a successful recovery claim. No desktop pixels were read back, no recording/audio file was produced, and no display/session settings or OS input were changed. Additional diagnostics and any subsequent run belong in a later checkpoint.

The editor-core suite passed 45 tests with one owned blocked-stdout subprocess fixture ignored as a standalone test and invoked by its passing supervisor test. Actual cancellation killed/reaped that owned blocked decoder and joined its reader; consumer failure also cleaned up a backpressured reader. Concurrent export publication tests establish one winner without replacing another destination and preserve the losing completed partial. Each FFmpeg export now owns a unique adjacent `.rcr-export-*` directory, performs a final cancellation check before no-clobber publication, bounds its diagnostic log tail and exposes a retained partial path to GPUI.

A fresh real `editor_fixture --fast --range-test --suppress-mic` run passed after these changes. Independent FFprobe confirmed `.downloads/export-safety-proof-20261024b.mp4` contains exactly one second of AV1 at 960×540 with three AAC streams labeled Mix, System audio and Microphone. Frame-45 preview, advancing playback and cancellation/reaping also passed. The successful export removed its empty owned staging directory. This does not establish every driver/error combination or a long-running synchronization bound.

All 66 desktop tests passed at this checkpoint: stale import/navigation ordering, newer re-import versus delayed removal, reference-delta reconciliation preserving unrelated preferences, no-clobber Trash restore, a synced original-path Trash journal and structured partial-export recovery information. Source-drive inspection/moves/rollback for Remove and Undo now run on a single admitted background operation. Local atomic preferences persistence retains its existing synchronous behavior. Native Library validation of this newer worker path and the later durable Trash browser remain pending.

Five local-upload tests passed through `rcr-toolkit --test local_upload`: the harness compiles the actual vendored local-uploader source against RCR's existing dependency lockfile, including its private collision/cancellation regressions. Creating a shared-folder output now uses `create_new`; a destination claimed after naming is preserved rather than truncated or removed. The public upload path also retained its existing destination, copied under the next name and left the original intact. These tests use only temporary local files, with no network uploads. Direct tests through the unrelated upstream workspace could not resolve its separately locked Clap version offline, and dependencies outside RCR were not downloaded merely for that workspace.
## Native recovery, input isolation and GPU presentation checkpoint (2026-10-24)

A complete workspace run passed 249 tests, with two intentionally ignored standalone fixtures. Later targeted desktop/Windows runs passed 77 and 23 tests respectively; the latter adds session-diagnostic coverage. Strict workspace/all-target Clippy passed with warnings denied before the subsequent synthetic GPU-surface helper and presentation changes. Those later changes require a fresh lint checkpoint.

The native Library fixture passed in 6,181 ms, including journal-based Trash recovery after clearing session Undo to simulate restart, real Tab/Enter/Escape focus handling, byte-exact restoration and preservation of unrelated preferences and original media. The extended editor keyboard fixture passed in 14,608 ms. Its export-details modal traps focus, wraps oversized Unicode diagnostics, closes with Space/Escape, and blocks editing shortcuts without changing Undo/Redo, playback, selection or project state. It does not activate Copy details or alter the clipboard.

The latest whole-timeline pointer-drag fixture used the actual 119.57-second 2560×1440 AV1/60 replay at `target/debug/recordings/replay-1792789538677.mkv`. After preview preparation, 300 pointer-dispatch-to-GPUI-paint samples measured 10.28 ms median, 24.59 ms p95 and 25.42 ms maximum; the entire open/preparation/fixture took 17,187 ms. These development-profile timings exclude physical monitor scanout and should not be treated as uniformly better than every earlier sample. The original replay remains unchanged.

An owned shared-texture proof reproduced a concrete GPU-preview failure: while the producer held its keyed mutex, the old renderer cleared the backbuffer and skipped the video, yielding background pixels. The updated renderer keeps one consumer-private GPU texture, copies a successfully acquired shared frame into it, releases the shared mutex, then draws the private image. Contention redraws the last good image for the same producer identity. A new identity starts without cached pixels. There is no CPU video readback in this production path.

`rcr-desktop --smoke-gpu-surface` subsequently passed initial color, byte-equal last-good redraw while busy, newly published color, producer texture/dimension replacement, native resize and window shutdown. An initial resize assertion exposed asynchronous native resize in the proof itself; the corrected helper waits for the requested viewport with a five-second bound before requesting a snapshot. Passing owned-window artifacts are under `.downloads/research/gpu-surface-proof/run-1792807818352609500-36600`; the resized red frame was visually inspected. These snapshots contain only synthetic app-owned textures.

The isolated `gpu_convert_proof` unit and native runs passed. Synthetic NV12 array slices converted black/white/red/green to shared BGRA with opaque alpha and at most four-channel-value tolerance, including downscale, visible crop, bounded four-view caching, nonblocking busy handling, cancellation/resume and invalid input rejection. The complete hardware proof took 198.8 ms; this is not a per-frame throughput benchmark. CPU upload/readback exists only in this synthetic proof. The converter is not yet connected to playback.

The Desktop Duplication recovery hardware proof remains unverified: four bounded attempts never obtained an initial desktop frame, so none exercised recreation. The last metadata showed an active WinSta0/Default session, matching input/thread desktops, an attached 2560×1440 SDR primary output and a healthy RTX 5070 device, but AcquireNextFrame returned only WAIT_TIMEOUT. An opt-in owned test-window variant performed 41 updates without obtaining a frame. No monitor-power, driver or session cause has been established; successful source/state tests must not be represented as successful hardware recovery.

The fresh one-second export safety fixture passed AV1 960×540 plus independent AAC Mix, System audio and Microphone streams, independently checked with FFprobe. Preview, advancing playback, cancellation and decoder reaping passed. Five local shared-folder uploader tests passed, including create-new publication preserving a competing destination. No network upload was performed.

After Follow-playhead integration, all 79 desktop tests passed. The native Zoom fixture passed in 1,894 ms: anchored zoom, ruler seek, manual pan suspending follow, actual Tab/Enter/Space control activation, immediate reveal and unchanged project state. The 1100×680 owned editor target was visually inspected at `.downloads/research/rcr-editor-follow-1100.png`. This does not measure new decoder throughput.

Workspace-only FFmpeg binding preparation verified that the official 7.0.2 provider archive's four relevant DLLs are byte-identical to the installed runtime. The archive SHA-256 is `216feea30258c17628f666e20a2536da7bb50171e6e583a768968e11ceafc8a3`; pinned libclang 18.1.1 generated complete public layouts from that archive's original headers. Exact runtime version calls passed, and generated Rust size/alignment/field-offset constant assertions compiled. Bindgen 0.71.1 expresses these as compile-time checks, so the helper correctly reports zero runtime tests. The resulting isolated artifacts are `.downloads/tooling/ffmpeg7-bindings/generation-<id>/output`. This establishes a reproducible typed ABI input, not a passing hardware decoder or production playback path.

## GPU failure isolation and audio transport checkpoint (2026-10-24)

The extended owned GPU-surface proof exposed a second real defect: an allocated but unpublished producer could be copied/drawn through key 0 before any successful key-1 publication. The failing six-stage run is `.downloads/research/gpu-surface-proof/run-1792808190592261500-3008`. A fresh consumer now requires key 1 for its first image. The production preview producer also releases key 0 when color conversion fails, rather than advertising failed pixels as a new frame.

The subsequent eight-stage proof passed at `.downloads/research/gpu-surface-proof/run-1792808428131008600-23812`: unpublished identity stays hidden, the existing contention/color/resize checks pass, an owned WARP producer deliberately mismatches the hardware adapter and exposes a bounded surface error while the rest of the scene still presents, and a healthy replacement displays again. Import/synchronization failure no longer aborts the entire scene. Sticky error status and a one-identity failure cache avoid retry/log flooding. The compact Capture error state was visually inspected at `.downloads/research/rcr-preview-unavailable-1100.png`; preview setup failure alone no longer requests recorder Stop. This is failure isolation, not cross-adapter GPU sharing support.

All 81 desktop and 23 Windows tests passed after these integrations. Strict desktop/Windows all-target Clippy passed with warnings denied. The native toolkit fixture passed its 25 outer stages in 9,519 ms, now including synthetic channel-driven utility acquisition failure, cancellation and clipboard-follow-up routing. The injected cases neither capture the screen nor alter the clipboard. Subsequent staged-Escape/overlay completion-ACK work still needs its own build/native result.

The core editor suite then passed 48 tests, with two explicitly ignored owned-child entrypoints invoked by passing supervisor regressions. An underrun now leaves an empty audio endpoint paused until frames are queued; a failed Start cannot acknowledge a running state. Decoder process completion retains the owned child under short `try_wait` locks, allowing cancellation after stdout EOF instead of holding the process mutex across `wait`. The owned subprocess regression verified prompt kill and reap with bounded setup/completion assertions.

The actual WASAPI-muted transport benchmark used the owned explicit-BT.709 two-second 1440p60 AV1 fixture at a 1280×720 preview bound. It reported 676.5 ms cold readiness, warm resume samples 34.7/29.0/37.6 ms, 95 presented frames during 1.65 seconds, 22.8 ms p95/24.3 ms maximum frame interval, two dropped frames, two decoder processes and worker/process reap in 19.5 ms. Audio tracks were muted throughout. These are local development-profile transport measurements and exclude GPUI painting/monitor scanout; they do not validate forced hardware underrun or the future GPU playback route.

The isolated D3D11VA decoder proof passed three frames before cancellation and 117 after reopening, converting the latter without CPU video downloads or CPU color conversion. Hardware frames were NV12 array slices with coded 2560×1536 and visible 2560×1440, 60/1 rate, square pixels and explicit limited BT.709 metadata. Total elapsed was 15,404 ms, which is not acceptable evidence of real-time throughput; phase measurements and further work are required before production integration. The proof consumes each shared output serially and includes repeated setup/teardown. The initial FFmpeg NVENC-generated fixture correctly failed the strict conversion check because its bitstream omitted primaries/transfer tags despite command-line flags. A separate known-source fixture was remuxed with explicit AV1 metadata; original files remain unchanged. Existing native RCR replay metadata independently reports limited BT.709 primaries/transfer/matrix, so the FFmpeg fixture issue is not attributed to RCR's encoder.


### GPU completion, frame identity and screenshot recovery checkpoint

The isolated GPU proof's 15-second bottleneck was narrowed to event-query retirement, rather than decode or conversion. Calling `GetData` without `DONOTFLUSH` once per new query, then polling without further submission kicks and requiring both `S_OK` and a true `BOOL`, reduced the same default 120-frame/reopen proof to 377.2 ms. Its 117-frame conversion loop took 151.3 ms; retirement median/p95/max was 2.88/3.91/5.29 ms. A separate consumer performing GPUI-style private copies passed in 403.4 ms. Extra producer `Flush` alone did not resolve the delay. Logs are `.downloads/research/d3d11-decode-default-fast.log` and `d3d11-decode-separate-copy-kick.log`. These are isolated hardware measurements, not displayed playback/audio-sync measurements.

The nine-stage owned GPUI GPU proof passed at `.downloads/research/gpu-surface-proof/run-1792810871237291100-50340`. A producer overwrite released with key 0 cannot replace the last successfully published image: the renderer now acquires only key 1 and otherwise draws its private retained copy. Tracked nonzero frame IDs are read with the published pixels and acknowledged only after draw submission. The Window API exposes the actual renderer adapter LUID. Draw submission does not claim monitor scanout.

The native toolkit fixture passed all 25 outer stages in 11,147 ms, including staged Escape, failed capture completion retaining its Actor/annotations, identical retry pixels, and standalone partial-success retry retaining its saved path. Synthetic recovery cases perform no clipboard/file output. Standalone save/copy progress retains only a pixel digest and successful-step metadata; unchanged retries finish remaining steps while edited pixels start a new output. A single clipboard lease and session/operation generations prevent overlapping writes and stale status callbacks. At this checkpoint 91 desktop tests and 31 toolkit unit tests plus 5 local upload and 3 renderer integration tests passed. Subsequent direct ACK wakeups and retry visual routes require the next validation batch.

Library thumbnail decoding now rejects unsupported decoded layouts before pixel allocation, with a 128 MiB decoded-output cap and 32768-pixel axis limit. This is not a whole-process memory cap. An opt-in owned 7680×4320 RGBA PNG proof passed through the actual thumbnail loader: 132,710,400 source pixel bytes, unchanged source metadata, exact expected 320×180 result, 1,762.6 ms thumbnail decode in the then-current development profile. Image/PNG kernel profile optimization is being measured against that baseline. Library work now carries revision cancellation and ignores stale results; catalog requests are admitted on scanner/worker progress, not each paint. Uninterruptible filesystem/image calls remain a limitation of cancellation latency.

### Optimized thumbnails and opt-in GPU transport

The same owned 8K PNG thumbnail regression passed after optimizing only development-profile image/PNG kernels: 198.1 ms versus the earlier 1,762.6 ms loader measurement (about 8.9x). Source/output dimensions, decoded-output budget and exact expected thumbnail checks remain the same. This is a local development-build improvement; it does not claim an equivalent release-build gain. The next desktop/toolkit batch passed 92 desktop tests, 33 toolkit tests and 8 integration tests. Direct ACK wakeups passed the native toolkit fixture in 9,714 ms. Native Library import/removal/Undo/restart recovery passed in 3,790 ms using the owned AV1 source, with original media and sidecar unchanged.

The owned retry layouts were inspected at `.downloads/research/rcr-image-retry-1100.png`, `rcr-image-retry-900.png` and `rcr-overlay-retry-1100.png`. The standalone header and recovery actions fit at both sizes. The overlay diagnostic initially clipped horizontally; a wrapping fix and stale file-dialog session guards are awaiting their own subsequent native build.

The isolated color proof decoded one owned 2560×1440 AV1 SMPTE HD bars frame through D3D11VA and compared the GPU conversion to an independently CPU-decoded explicit-BT.709/full-range BGRA reference. It accepted 30 flat 9×9 patches, covered 11 distinct colors and full 255 channel span, rejected no patch, measured maximum channel error 3 against tolerance 4, and checked every GPU alpha byte was 255. Six nonflat patches were skipped under the fixed reference-flatness rule. Full proof setup/readback/comparison took 351.9 ms. CPU readback exists only in this proof.

Core regressions passed 59 tests with two deliberately ignored owned-child entrypoints; the isolated color example passed nine tests. Strict editor-core all-target Clippy passed with warnings denied after correcting example imports/iteration and a redundant generated-constant cast. The first opt-in transport hardware proof passed both the existing silent audio clock and actual muted WASAPI: initial paused frame, held pause, resume, source-in 500 ms plus playhead frame 6 mapping to actual source PTS 600 ms, forced GPU-to-CPU fallback preserving the clock, and shutdown. Both runs used an 18-surface pool with at most three future frames and one pending converter read, with no GPU video readback. Shutdown took 21.2/19.2 ms. These are transport proofs with a separate GPU consumer, not yet GPUI editor playback validation. CPU-default playback remains unchanged.

The subsequent source-change race proof passed: after first publication while paused, one byte was appended only to a unique owned workspace copy and GPU fallback was requested immediately. The typed source failure became terminal in 2.627 ms, with no CPU fallback frame or decoder process, unchanged original input, 27.4 ms shutdown and owned-copy cleanup. The ordinary audio lifecycle proof remains separate; this guard test deliberately removes audio in its in-memory project so subprocess-count assertions describe video only.

A six-second 360-frame 1440p60 owned fixture then completed against muted WASAPI. All 360 frames were decoded, published and consumed, final project position was 360 and both final publication/consumption IDs were 360 (image frame 359). Pool size stayed 18, future-frame maximum 3 and pending-read maximum 1. No CPU fallback/readback occurred; the exact final output identity remained stable after a 100 ms hold. Total setup/playback/hold was 7,394 ms and explicit shutdown took 26.1 ms. This demonstrates transport lifetime beyond the isolated proof's 120-frame cap, without claiming a long-running soak or monitor cadence measurement. Strict editor-core all-target Clippy passed again after both new proof modes. The latest desktop source batch passed 94 tests with one opt-in 8K codec proof ignored; new native GPUI integration and dialog/layout fixtures remain pending the next executable build.

### Device selection and native presentation checkpoint

Optional saved microphone and desktop-output endpoint IDs now preserve the existing Windows-default behavior when unset. Explicit devices resolve by exact opaque ID, require active state and correct audio direction before activation, and fail without substituting another device. Process capture rejects an endpoint ID and remains independent of a saved desktop-output choice. Discovery runs on an MTA worker with a global admission guard, cancellation between OS calls, at most 128 devices and bounded friendly names. IDs are not matched by names; disconnected selections stay visible. Discovery does not activate an audio stream.

Ten media-core and 26 Windows unit tests passed (the existing GPU-recovery proof remains ignored). Native read-only enumeration listed active inputs/outputs and their defaults. A subsequent real explicit-resolution test resolved four selected devices, rejected four direction mismatches and two unique missing IDs, and verified both current defaults, with zero activated streams or captured samples. This validates discovery/resolution; selected-device recording, permissions/format behavior and unplug recovery have not been exercised.

The native device-picker fixture passed in 895 ms using seeded devices and the production Studio controls/subscriptions. It covered Capture/Settings keyboard activation, unavailable/no-match guards, Tab/Shift+Tab containment, duplicate-name exact-ID selection, explicit Default, focus restoration and active/old-entity selection rejection, with no enumeration/audio activation/preference writes. Owned snapshots then revealed a separate layout defect: every title/detail collapsed to ellipsis in both device and application pickers. Explicit row/text widths and column layout, plus a native usable-label-width guard, are awaiting the next visual pass. Keyboard/model correctness did not establish readable rendered labels.

The initial native GPUI playback proof stalled with GPU publication 1 but zero actual surface draws. Its hidden window called `Window::draw`, which constructs a Scene without native presentation. An explicit owned-window diagnostic presentation API now resizes the hidden target and executes the regular GPU draw/present path without readback or desktop input. The corrected proof passed in 1,991 ms: actual submitted GPU ID 1, paused ID 8, resumed playback and drawn CPU replacement frame 15 preserving the clock, with an 18-surface pool, future-frame maximum 3 and pending-read maximum 1, and no pixel readback. A separate CPU-image frame identity now prevents retained old CPU pixels from being acknowledged as a newer GPU frame after seek/hide/fallback. The proof's metadata/phase diagnostics remain bounded and failures are no longer swallowed by window-update fallback.

The latest toolkit native fixture passed all 25 outer stages in 9,948 ms, including stale image-project/insert-dialog rejection after reopening, retained-Actor retry and Utilities stale-save/screen-selection admission with actual generated QR decoding. The wrapped overlay recovery layout was inspected at `.downloads/research/rcr-overlay-retry-wrapped-1100.png`; a 180-character unbroken diagnostic wraps inside the capped panel while Save/Copy stay visible. Capture with device controls fits 1100×680 at `rcr-capture-devices-1100.png`. Strict workspace all-target Clippy passed with warnings denied before the subsequent sustained-GPU/row-geometry fixture additions. Source-open autosave is normal app behavior; newer GPU/device native fixtures now isolate their project/preferences/cache working directories so this cannot alter existing user project state. Their media source is still read-only.

### Sustained owned GPU playback and device-picker review (2026-10-24)

A fresh `cargo test --workspace --all-targets -j1 --offline` passed, followed by strict workspace/all-target Clippy with warnings denied. Native/media/build work remained serial. The current desktop executable then passed the isolated six-second GPUI GPU EOF fixture: all 360 frame IDs were observed in order, the final position and submitted ID reached 360, and the final image remained retained for 107.6 ms. The fixture reported no CPU video replacements, no pixel readback and no in-memory project edits; the original media SHA-256 remained unchanged. The proof now creates a unique owned working directory before application initialization, isolating its ordinary project autosave and caches from user projects.

Observed submission intervals in that hidden-window diagnostic were 15.64 ms median, 30.32 ms p95 and 61.33 ms maximum, with 396 draws including redraws and 8,283 ms total fixture time. These results establish sustained frame identity and completion, not smooth monitor playback. The fixture directly presents an owned render target and uses WinRT timer wakeups, whereas normal visible windows use the DWM-driven repaint path. Additional bounded timing instrumentation is needed to separate timer/dispatch delay, audio-clock pacing and renderer work.

The isolated audio-device fixture passed in 888 ms and the application-source picker fixture passed in 382 ms, including their keyboard/state checks and a minimum 200-pixel label-container assertion. Subsequent rendered inspection of `rcr-capture-microphone-devices-readable-1100.png` and `rcr-capture-audio-picker-readable-1100.png` still showed ellipses instead of readable row labels. The container-width assertion did not establish correct glyph layout. Both picker layouts therefore remain under correction; the successful input tests must not be represented as a successful visual review.

### Shaped text correction and playback phase measurements

The complete workspace/all-target test run passed after shared FFmpeg discovery, the optional playback preference and new diagnostic sampling. A newly added test module initially resolved GPUI's re-exported test macro recursively; importing Rust's built-in test attribute fixed compilation. The normal desktop executable then built successfully in 39.2 seconds.

Rendered device/application pickers are now readable. The underlying GPUI text cache ignored changing truncation widths when wrapping was disabled, retaining an intrinsic-pass ellipsis. It now keys both wrap and truncation widths and starts each measurement from original text runs rather than destructively reusing previously truncated runs. The native device fixture passed in 900 ms, including actual shaped-text equality and the same two-run StyledText measured first at 1 pixel, then at 600 pixels; the wide pass must restore its full original glyph text. The application-source fixture passed in 397 ms. Owned targets `rcr-capture-microphone-devices-text-fixed-1100.png` and `rcr-capture-audio-picker-text-fixed-1100.png` were visually inspected and show labels and details correctly. Unavailable long detail text still truncates normally within its row.

An instrumented muted-WASAPI GPUI EOF run again observed all 360 submitted IDs without skips, CPU replacement or readback, finishing in 8,212.6 ms with unchanged original media SHA-256. Its 527 bounded phase samples measured scene construction median/p95/max 12.12/12.80/14.35 ms, native presentation 0.419/0.572/2.569 ms, actual requested-1-ms background timer completion 2.91/13.97/15.79 ms and foreground dispatch 0.161/0.413/2.174 ms. Scene work is a substantial cost in this development build; the result does not justify attributing every interval outlier to the timer or claim smooth physical scanout. The owned fixture directory is `.downloads/gpu-playback-smoke-J9XUIg`.

### Lazy startup, pointer deduplication and contextual properties checkpoint

A fresh complete workspace/all-target test run passed after lazy D3D initialization, shared-runtime contention/cancellation corrections, repeated ruler/trim pointer deduplication, contextual image-property controls and transactional hotkey persistence. The desktop suite contains 103 tests at this checkpoint (one opt-in 8K codec proof ignored). Strict workspace/all-target Clippy subsequently passed with warnings denied. Two newly introduced editor test modules needed explicit Rust built-in test imports because GPUI exports a different attribute; the hotkey test module was also moved after production items for the strict lint. The pre-existing third-party `proc-macro-error2` future-incompatibility notice remains.

Before these latest changes were built into the normal executable, the same owned six-second AV1 fixture failed the existing continuous-drag performance assertion: 92 samples, 19.89 ms median, 36.21 ms p95 and 151.91 ms maximum, 4,341 ms total. Logs are `.downloads/editor-open-<id>.stdout.txt` and its stderr sibling. The threshold has not been relaxed. The next executable adds the redundant frame-copy/pointer fixes and development-profile GPUI/Taffy optimization for a direct owned-fixture comparison. No improved latency is claimed before that run.

### Native drag and contextual-property results

The optimized-development executable built successfully in 2 minutes 24 seconds (one-time GPUI/Taffy optimization rebuild). With the existing latency threshold unchanged, the same owned six-second AV1 drag fixture passed: 92 samples, median 15.37 ms, p95 15.91 ms, maximum 16.05 ms, total 2,006 ms. The prior executable's same fixture measured 19.89/36.21/151.91 ms and failed its maximum-latency bound. This A/B combines pointer/cache-copy corrections and optimized UI dependencies; it does not isolate each change's contribution or measure monitor scanout.

A separate 92-sample drag run on the actual 119.57-second 2560×1440 AV1/60 RCR replay measured median 15.36 ms, p95 16.08 ms and maximum 16.54 ms, total 2,607 ms. Original media SHA-256 was unchanged; project/cache effects were isolated in `.downloads/real-seek-<id>`. This is a different sample workload from the earlier 300-sample random-pointer result and must not be presented as a directly matched comparison. Native trim passed both edges on one/two clips, exact pointer following, linked audio, Undo/Redo and repeated subframe moves without rebuilding identical trims.

The complete toolkit native fixture passed all 25 outer stages in 4,507 ms. Its added standalone blur test focused the actual strength button and activated it by keyboard, verified a changed strength and accepted-pixel digest, then Undo restored the exact baseline pixels. No clipboard/file output services ran. Selected blur, pixelate and magnifier panels were rendered at 900×600 and 1100×680; inspected artifacts include `rcr-tools-image-blur-context-900x600.png`, `rcr-tools-image-magnifier-context-900x600.png` and `rcr-tools-image-pixelate-context-1100x680.png`. Relevant controls fit and irrelevant text/fill controls are absent from redaction tools.

Automatic GPU startup then caught a genuine early-eligibility regression: it rejected the already proven AV1 fixture as lacking explicit square pixels, because it read only codec-parameter SAR and ignored the declared AVStream 1:1 SAR. The failing run is `.downloads/gpu-playback-<id>.*`. A conservative stream/codec SAR resolver and regression test now compile in the passing core/all-target suite; native GPU rerun remains pending. Both-unknown, conflicting and non-square SAR remain rejected, and per-frame validation remains strict.

The opt-in precompiled native reader cancellation test passed in 20 ms against the owned six-second AV1 source: one software-native frame, the installed AVIO callback observed cancellation, no second image decoded, close completed, the native admission released and the source stamp remained unchanged. Its callback state stayed owned through close. This verifies the local cancellation/lifetime path, not interruption of an already-blocked Windows filesystem call. Native seek setup/seek/decode deadlines are cooperative. A subsequently identified transient native-admission retry issue is under correction before the next full checkpoint.

### Automatic runtime and corrected GPU startup verified

The read-only runtime probe passed Directory → Automatic → different Directory → Scrub requirement pin reuse within one process, reporting the same retained C:/bin family (util 59.8.100, codec 61.3.100, format 61.1.100, scale 8.1.100), without media or GPU/audio activation. A new owned one-frame 1440p60 H.264 control rejected GPU eligibility with zero device-factory calls in 19.13 ms. The explicit-BT.709 owned AV1 fixture reached exactly one sentinel factory in 11.42 ms; the recorder's actual RCR AV1 replay did so in 11.47 ms. Sentinel probes create no D3D/audio device.

After the SAR correction, native muted-WASAPI transport completed all 360 frames in 7,348.7 ms including startup/hold, with no CPU fallback/readback, pool18/future3/read1 and shutdown31.8 ms. Pause/resume, retained clock, trimmed source preroll and forced CPU fallback also passed; trim500 ms plus project frame6 produced exact source600 ms after36 preroll frames, shutdown34.8 ms.

A fresh full workspace/all-target run and strict workspace/all-target Clippy passed before the following native desktop verification. The normal desktop executable built in57 seconds. `verify-gpu-playback.ps1 -Automatic -UntilEof` then passed the Settings-equivalent opt-in without GPU/DLL environment overrides: all360 submitted IDs, no skipped IDs, no CPU replacements/readback, final image retained110.3 ms, unchanged original SHA-256, total7,730.9 ms. Owned output is `.downloads/gpu-playback-smoke-Yx1Tjv`.

Its498 bounded phase samples showed scene construction median/p95/max1.535/1.866/4.507 ms versus the earlier12.12/12.80/14.35 ms development baseline. Native present median/p95/max0.348/0.482/3.631 ms; requested1-ms background timer completion13.52/14.67/15.42 ms; foreground dispatch0.153/0.357/2.378 ms. The optimized scene has substantially more frame budget. Frame-ID observation intervals still measured median15.61/p9530.65/max92.68 ms, so this hidden timer-driven fixture does not establish uniform60-Hz physical presentation or resolve all frame-pacing questions. It uses explicit owned-target presentation, not the production DWM repaint cadence.

Subsequent independent review found two selected-text edge cases (batched Apply/Undo ACK ordering and losing dirty drafts on target switch); those are being corrected before that feature is considered complete. It also found negative native FFI returns could bypass the typed deadline check. Native configure/send/drain/download/colors/scale status paths now check stop state before validating generic status, preserving request-terminal deadline routing.

### History, Library selection and annotation draft validation

A fresh complete workspace/all-target test run passed (desktop 106 tests, one opt-in 8K test ignored), followed by strict workspace/all-target Clippy with warnings denied. Native full-database History passed in 397 ms against 257 in-memory entries, including actual keyboard paging, search for an entry beyond the old 100-item snapshot limit, superseding queries, and unchanged workflow admission and Library catalog. The isolated Library selection fixture passed in 205 ms using two owned synthetic PNGs: Ctrl-click, forward/reverse Shift ranges, missing anchors, keyboard focus/selection, search isolation, modal focus containment and cancellation.

The full native toolkit fixture passed all 25 outer stages in 4,833 ms. Extended selected-text checks covered A-to-B-to-A retained drafts, hidden-draft output guards, stale Apply rejection, matching ACK preserving newer typing, batched Apply/Undo revision conflicts, Reload, and exact accepted/Undo pixels. New pure tests additionally cover ACK-before-preview output blocking and recovering deleted-target draft text without touching the clipboard. These results precede the subsequent contextual-control styling and Library folder-error additions; those await the next executable checkpoint.

GPU contention instrumentation separated keyed-mutex Busy waits from full read-ticket waits. Automatic GPUI EOF run `.downloads/gpu-playback-smoke-RA3Y7b` passed all 360 submitted IDs, zero skipped IDs, zero drops/CPU replacements/readbacks, pool 18/future 3/read 1, and unchanged source SHA-256. It measured 18 Busy retries totaling 129.55 ms; QueueFull retries/wait were zero. Clock-paced waits totaled 5,738.76 ms. Scene construction median/p95/max was 1.518/1.929/4.809 ms, with frame-ID observation intervals 15.60/30.64/93.34 ms. These are hidden owned-target submission observations, not monitor scanout. A targeted same-process consumer-release epoch/wake path is now under validation; no pacing improvement is claimed before rerunning this fixture.

### Consumer release wake and compact controls checkpoint

The integrated workspace tests passed (desktop 107 tests, one ignored; editor core 80 tests, three ignored), followed by strict all-target Clippy with warnings denied. The normal executable rebuilt in 2 minutes 24 seconds after the GPUI renderer change. The owned nine-stage GPU surface proof passed at `.downloads/research/gpu-surface-proof/run-1792819465299487000-48872`, including exact release-epoch counts: cached redraws, unpublished content and failed imports never signal; later replacement surfaces never increment a retired output's epoch. The notification is advisory; normal keyed-mutex acquisition still grants every write.

Three serial Automatic EOF runs passed all 360 frame IDs with zero dropped/skipped IDs, CPU replacements or video readbacks and unchanged source SHA-256. Busy waits measured 16.87, 16.91 and 13.22 ms total (15, 14 and 13 retries respectively), versus 129.55 ms/18 retries in the pre-wake instrumented baseline. QueueFull remained zero. Outputs: `gpu-playback-smoke-s3Auin`, `gpu-playback-smoke-aGALLT`, `gpu-playback-smoke-H1sPn6`. Overall elapsed times were 7,964.24, 7,557.52 and 7,676.44 ms including startup/hold; frame-ID interval maxima remained 107.97, 77.91 and 92.62 ms. The targeted waiting reduction does not establish overall smoothness or physical scanout. Frame-pair and audio-clock diagnostics are being added to separate startup holds from later pacing.

Native History passed its fixed-header scrolling, 56 px rows, Ctrl+F, page controls, return-after-capture refresh and explicit F5 checks in 497 ms against an in-memory database. Native toolkit passed all 25 stages in 4,882 ms, including an actual busy Remove click that leaves settings/job admission unchanged and a real Font+ activation while retaining a newer text draft. Owned compact images `rcr-tools-history-fixture-refined-900x600.png`, its 1100x680 sibling, and `rcr-tools-image-magnifier-refined-900x600.png` were rendered; the 900 px History and magnifier views were inspected. The attempted Library unavailable-folder snapshot timed out before rendering; review identified a delayed FilesChanged request after terminal scanner cancellation as a source-supported cause. New stopped-state guards and metadata/error-path corrections await a rebuilt native rerun; no failed snapshot is counted as successful evidence.

### Keyboard, capture readiness and atlas baseline

After the old offline-catalog test was updated to require retained title/date/size rather than disappearance, the complete workspace/all-target suite passed (desktop 117 tests, one ignored; core 80 tests, three ignored). The earlier failed expectation was `cached_title == filename` and `modified == None` after deleting the owned source; availability remains explicitly false in the strengthened test. Strict Clippy passed after a small nested tray conditional correction. The normal executable built in 3 minutes.

Native History passed in 1,257 ms: 100 real row-action Tab stops, visible forward/reverse traversal, no visible-row jumps or manual-wheel snapback, Ctrl+F from the last row, current query identities, pane-return/F5 freshness, and unchanged workflow admission/status/Library catalog. Native Library selection passed again in 209 ms. Both Library recovery snapshots now complete after the terminal scanner guards; the 900 px unavailable and refreshing images were inspected. The unavailable layout still pushed card names below the initial viewport, motivating a separate compact header/banner/placeholder adjustment now awaiting rendered verification.

Capture startup/recording/finishing and active Settings were rendered as presentation-only fixtures, without starting NVENC/audio capture. Startup and finishing images were inspected: labels are truthful and cancellation is available before the first frame. Cleanup remains configurable per saved replay; earlier review incorrectly treated it as a session-only backend setting. No such lock remains. The stale recording-switch intent regression passes: a failed switch cannot cause a later unrelated replay stop to record.

The expanded toolkit fixture failed its new F6 visibility precondition because the normal tall smoke window fit the whole tool list. This is not counted as successful keyboard validation. The corrected fixture constrains its actual tools viewport, retains the offscreen/fully-visible assertions, and restores it afterward; the rebuilt rerun is pending.

The pre-reclamation owned atlas baseline passed 300 cycles: two stable 512 px tiles and two replaced together, exactly one warmup texture/four uploads/4 MiB, then 300 additional texture creations, 600 uploads/removals and 600 MiB submitted bytes. Live/peak textures were both two. Update+scene+present median/p95/max was 1.614/2.120/2.742 ms; final stable and changed pixels were exact. Output: `.downloads/research/atlas-churn/run-1792820997072033700-48756`. This measures resource churn, not a monotonically growing GPU memory leak. Tile allocator deallocation is the next narrow comparison; empty-texture retention is not yet implemented.

The richer Automatic GPU EOF run `.downloads/gpu-playback-smoke-1wYIj4` passed all 360 IDs with no drops, replacements or readback. All 17 Busy retries observed a real consumer-release epoch change, totaling 15.50 ms. The largest image interval was specifically ID 1→2 at 76.29 ms; after the first source second, 300 intervals had median/p95/max 15.62/30.74/31.74 ms. Audio reported one device start, zero underrun pauses, zero PCM-wait ticks and final 6-second clock position. This localizes the long interval to initial readiness in this run; it does not prove uniform monitor pacing. Normal Automatic pause/resume and forced video-only CPU replacement also passed in 2,181.63 ms, retaining the same audio clock. Source SHA-256 was unchanged in the EOF proof.

### Atlas tile reclamation and bounded empty-resource reuse

The complete workspace tests and strict all-target Clippy passed again before the renderer rebuild (desktop 116 passed/one ignored; editor core 78 passed/three ignored). The corrected native toolkit fixture passed all 25 stages in 4,990 ms, including constrained-viewport F6 visibility; the compact unavailable Library snapshot was inspected and now exposes names, last-known sizes and explicit unavailable state in the initial 900 px view. The native cancellation test passed in 100 ms after the single-allocation BGRA padding correction; this remains a cooperative local lifetime test, not interruption of blocked Windows I/O.

Deallocating the actual atlas tile reduced the matched two-stable/two-changing 512 px workload from 300 additional texture creations to zero: `.downloads/research/atlas-churn/run-1792821862781964100-39824`. Its median/p95/max update+scene+present was 1.719/2.271/2.522 ms, so that first comparison established resource reclamation rather than a latency gain. A separate single 1280×720 preview still created 120 textures in 120 cycles when its last key retired, despite live/peak resources staying at one: `run-1792821879389287500-27836`, median/p95/max 2.675/3.043/3.882 ms. This motivated retaining at most one eligible empty resource per texture kind, capped at 16 MiB nominal pixel payload per kind. The cap does not measure driver VRAM.

The bounded-cache policy's four tests passed in an isolated dependency-free harness extracted from the exact production helper/types/tests (production directx_atlas.rs SHA-256 `[redacted local build hash]`). Cargo cannot directly test the vendored GPUI package because its dev-dependencies are outside this workspace; this is not claimed as a successful GPUI library-test run. Workspace/Clippy then passed and the normal executable rebuilt in 2 minutes 21 seconds. Independent review found and corrected a proof weakness: an even cycle count originally ended at the warmup color. Final changed tiles now use a different color, and both cache-consumption checks explicitly snapshot a new color immediately after reuse.

Native `verify-atlas-churn.ps1 -RequireReuse` passed 300 cycles with zero new textures, 600 uploads/removals, 600 MiB submitted bytes, live/peak one, and exact stable/new final pixels. Median/p95/max was 1.488/1.878/2.387 ms; output `run-1792823021982158700-49096`. The separate `-VideoPreview -RequireReuse` passed 120 cycles with zero new textures, exactly 120 cache consumptions, 120 uploads/removals and 442,368,000 submitted bytes. Median/p95/max was 2.254/2.705/3.142 ms, timed cycles 1,872.89 ms; output `run-1792823033423224700-36620`. Its additional owned GPU stages proved incompatible landscape/portrait eviction, rejection of an 18 MiB empty source without evicting the eligible cached resource, renewed reuse and exact changed pixels. The retained empty payload was 5 MiB; no device-loss injection or monochrome native proof is claimed.

The full toolkit fixture subsequently passed again in 5,061 ms and Library selection in 205 ms. A continuous-drag comparison on the same owned six-second AV1 fixture passed 92 samples at median/p95/max 15.27/16.15/16.25 ms, total 2,053 ms. This is similar to the prior 15.36/15.97/16.70 ms result; texture reuse is not represented as a universal scrubbing or physical presentation fix. New Library F6/F5, external canvas text clipboard and asynchronous shell/diagnostics changes are separate work awaiting the next checkpoint.

### Bounded I/O, actual capture retirement and sustained pointer pressure

The integrated workspace suite passed with desktop 149 passed/one ignored, core 78 passed/three ignored and toolkit 46 passed, followed by strict all-target Clippy and a normal executable build. Earlier compile checks caught a GPUI test-macro glob import and an inherent/trait window_handle name collision; both were corrected without raising recursion limits or substituting a foreground HWND. The Library native fixture passed in 248 ms, including real F6 search/capture return and filtered fallback, F5 retaining query/selection/focus, pointer ranges and modal containment. The complete native toolkit passed all 25 stages in 5,012 ms. Text clipboard tests use real Actor ordering with injected payloads/failures; they never touch the system clipboard. The normal native toolkit likewise invokes no clipboard/file output service.

Canvas text cut/copy/paste now uses a dedicated bounded worker and a reply barrier separate from the command queue. Windows text access is fallible and format/size bounded; Cut waits for successful write/close acknowledgement. The shared image/text lease outlives Actor cancellation, timeout and reopening until the actual OS call returns. A queued expired reply performs no OS access. This matters because [Windows delayed clipboard rendering](https://learn.microsoft.com/en-us/windows/win32/dataxchg/clipboard-operations) can involve another application; a bounded payload alone does not bound that application's response time. Saved-video Copy and file reveal/open likewise run on a bounded worker with the actual GPUI window's HWND and operation/source/session guards. Injected tests cover blocked admission, dropped replies, actual errors, UTF-16 DROPFILES payloads and original-file preservation. No claim of actual Windows clipboard interoperability is made from those injected tests.

Editor error logs and capture diagnostic reports use independent bounded writers. Report acknowledgements follow completed OS writes, with a ten-second receipt deadline; timeout leaves actual ownership with the worker and is not reported as success. Capture proof status now requires encoded frames, published/drawn GPU preview, zero raw CPU readbacks and no requested-audio/capture/preview error. It defaults to failure until a matching report acknowledgement, so early closure is not proof success. Early audio validation failures exit proof mode. Library List/Grid uses one typed, coalesced asynchronous preference intent; all other preference saves retain their existing synchronous rollback boundaries through the same storage owner. Nine isolated coordinator/atomic-write tests cover stale snapshots, failure recovery, panic recovery, shutdown and revision limits. Acknowledgements mean completed atomic replacement, not power-loss durability.

Capture retirement's gated worker tests passed in the subsequent complete workspace run (desktop 141 passed/one ignored before later Copy/status additions). An early Finished event retains capture admission and Finishing state while driver destructors remain live; a replay result arriving during that interval is retained. Poll joins only an is_finished handle, then reports/consumes restart once; panic/missing terminal results suppress restart. This uses Rust's documented [nonblocking join pattern](https://doc.rust-lang.org/std/thread/struct.JoinHandle.html#method.is_finished). Actual NVENC/DXGI retirement was not rerun: earlier desktop-duplication initial-frame failures remain unverified. Explicit application shutdown still intentionally waits for capture finalization; this change concerns normal stop/restart responsiveness, not forced interruption of drivers.

The new sustained-pointer fixture differs from the earlier sequential target/paint test: it keeps issuing distinct quantized targets for two seconds while decoding proceeds, sweeps forward and backward over most of the recording, requires changing actual paints during input and an exact final target after release, and checks project JSON plus original media SHA-256. It passed the owned six-second AV1 source with 130 targets/128 changing paints, request-to-paint median/p95/max 14.437/15.134/15.507 ms, maximum paint gap 30.190 ms and final seek 14.974 ms. On the actual 119.57-second 1440p60 AV1 replay it passed 129 targets/127 paints, request-to-paint 13.829/14.730/15.293 ms, maximum paint gap 17.015 ms and final seek 13.666 ms. Total open/preparation/test was 12,437 ms using a fresh seek cache; output `.downloads/real-pressure-<id>`. Both results wait for seek-proxy readiness. They prove bounded owned-target responsiveness, not monitor scanout or cold-start performance.

The cold mode intentionally does not wait for that readiness. It failed on the same actual replay: nine changing paints during two seconds. The diagnostic rerun at `.downloads/scrub-pressure-<id>` issued 130 targets, confirmed proxy_ready_at_start=false, first changed paint 494.17 ms, maximum paint gap 405.52 ms, request-to-paint p95 419.77 ms and final seek 329.60 ms. The original SHA-256 remained `[redacted local media hash]`. Thirteen native hardware trace samples measured median seek 0.029 ms, flush 15.744 ms, decode 142.209 ms and output 1.495 ms. The responsiveness threshold was retained. [FFmpeg 7.0.2 CUVID flush](https://raw.githubusercontent.com/FFmpeg/FFmpeg/n7.0.2/libavcodec/cuviddec.c) destroys decoder/parser state; recreation and GOP preroll occur in the broader seek/decode operation, and individual creation time was not separately measured. A guarded, opt-in native AV1/CUDA experiment is now under test; no cold improvement is claimed yet.

Presentation-only Library saving/failed and playback fallback/next-setting states were rendered and inspected at 900×600: `rcr-library-layout-saving-900.png`, `rcr-library-layout-failed-900.png`, `rcr-editor-playback-fallback-900.png`, `rcr-editor-playback-setting-pending-900.png`. Status and error text remain visible without fabricating media metadata or invoking persistence/decoding. Playback details explain actual CPU/GPU/fallback state separately from seeking; default GPU playback remains off. Its new native keyboard fixture and the native AV1/CUDA experiment await the next normal executable run. The latest workspace pass includes their compiled tests (desktop 149 passed/one ignored; core 80 passed/three ignored).

The subsequent normal native keyboard run passed in 9,587 ms, including actual Playback details badge Tab/Enter activation, modal Tab/Shift+Tab containment, blocked edit/seek/export shortcuts, Space close and Escape focus restoration. Transport, selection and Undo/Redo remained unchanged; it used no clipboard action. The same run passed the existing split, volume, preset, Add clip and export-diagnostics checks.

The opt-in native AV1/CUDA cold run did **not** pass the responsiveness bar: `.downloads/scrub-pressure-<id>` issued 130 targets with nine changing paints, first paint 451.68 ms, maximum gap 327.30 ms, request-to-paint p95 342.87 ms and exact final seek 323.67 ms. All twelve trace samples used the intended native AV1/CUDA backend. Median flush fell to 0.046 ms, but decode was 182.69 ms and output 3.12 ms. Removing decoder recreation alone is insufficient; the experiment remains opt-in. A separate legacy CUVID run with proxy preparation disabled (`scrub-pressure-<id>`) yielded fourteen paints, first paint 405.98 ms, maximum gap 204.91 ms and exact final seek 263.53 ms, also failing. Background proxy generation contends for GPU decode/encode resources; it uses CUDA scaling and NVENC, not software x264. Original source SHA-256 remained unchanged in both controls. Interactive approximate preview work is separate and has not yet been accepted as a cold-performance fix.

### Truthful landing frames and export recovery

The next integrated workspace/all-target suite passed, followed by strict Clippy and the normal desktop build (22.29 seconds). Core tests passed 83 with three ignored. The new opt-in landing path binds request serial, requested position, actual decoded position and unique GPUI image identity; an Exact upgrade rejects older approximate results, including a same-integer-frame release. Both release paths, Escape, lost button state and window deactivation finish dragging. Rational source-in/source-out checks reject out-of-clip pixels rather than inventing an identity; incompatible timestamp grids complete the existing exact route. Normal exact behavior and both decoder flags remain unchanged by default.

Cold landing results on the same original replay, with source SHA-256 and project JSON unchanged:

| Backend / background proxy | Changing paints in 2 s | Request→paint median / p95 | Largest paint gap | Exact final |
| --- | ---: | ---: | ---: | ---: |
| Legacy CUVID / preparing | 28 | 77.63 / 109.75 ms | 94.70 ms | 290.65 ms |
| Native AV1 CUDA / disabled | 84 | 31.57 / 46.50 ms | 47.66 ms | 186.01 ms |
| Native AV1 CUDA / preparing | 82 | 30.83 / 46.39 ms | 47.45 ms | 259.59 ms |

Artifacts are respectively `.downloads/scrub-pressure-<id>`, `scrub-pressure-<id>` and `scrub-pressure-<id>`. First changed paint was 246.59 / 156.78 / 200.32 ms. The previews can differ from the requested position by up to 119 / 119 / 118 frames (almost two seconds at 60 fps), which is reported and labeled rather than hidden. All three still **fail** the unchanged 100 ms exact-final bar. The final target is deliberately a distinct distant jump to 70% of the recording; these results are not exclusively same-position mouse-up latency. A direct mapped-output experiment is separate, opt-in and unvalidated at this checkpoint.

The default exact warm path still passed after these envelope changes: fresh-cache preparation plus test took 11,874 ms, with 131 targets/128 changing paints, zero approximate paints or temporal error, request→paint median/p95/max 13.992/14.783/15.166 ms, maximum gap 16.702 ms and exact final 13.420 ms. Output: `.downloads/scrub-pressure-<id>`. This waits for proxy readiness and measures the owned GPUI paint target, not physical monitor scanout.

The expanded native editor keyboard run passed in 9,167 ms. It verifies the retained incomplete-file action, three-action forward/reverse traversal, scrolling and Close visibility with a very long fictional path plus large errors, skipping pending controls, and injected clipboard success/failure/stale-result guards. It invokes neither Explorer nor clipboard services. Actual Windows diagnostic copying uses the same bounded admission and allocation/transfer/close acknowledgement as saved-file copying; UTF-16, payload/admission bounds and dropped-observer tests are injected, not an interoperability proof. The 900×600 `rcr-editor-export-failed-900.png` owned-target rendering was inspected: explanation, incomplete path, Copy and Close remain legible and inside the panel.

Both new 900×600 Capture audio-error presentations were rendered and inspected with no capture worker or audio activation: `rcr-capture-audio-error-900.png` and `rcr-capture-audio-error-stopped-900.png`. Actual bounded source errors remain visible after Stop and are separate from replay-save notices. The subsequent native keyboard fixtures passed both active/stopped presentations, including focus, Home/End, Page and arrow scrolling, and Escape. Owned-target screenshots `rcr-capture-audio-error-keyboard-900.png` and `rcr-capture-audio-error-stopped-keyboard-900.png` were inspected. These fixtures do not establish real microphone recovery or device-unplug behavior.

The copied native AV1 path was also checked against the original 119.57-second recording, with proxy generation disabled. Seven ordinary/trim/reorder comparisons reported mean BGRA error 0.000 against independently decoded references; speed and effects intentionally used the existing fallback. Artifacts: `.downloads/native-av1-correctness-<id>`. Original SHA-256 was unchanged. The separate explicit mapped experiment passed the owned six-second pixel comparisons and a real held-output cancellation/admission test; it retains at most one returned mapping (pool 23), but did not improve original-recording exact release latency: 202.91 ms versus copied native 186.01 ms in the earlier landing trial. Mapped output remains opt-in. These checks do not prove forced device-fault recovery.

Controlled 20-second 1440p60 AV1 encodings of the same source section tested 120/60/30-frame keyframe intervals at VBR 25 Mbps/max 37.5 Mbps. Actual 1,200-frame outputs and keyframe positions were verified; encoding took 9.31/9.31/9.22 seconds, with sizes 65.26/64.51/64.30 MB. CPU comparisons against the already-compressed source measured PSNR 47.235/46.768/46.294 dB and SSIM 0.990435/0.989614/0.988559. This establishes a detail tradeoff for this section, not pristine-source fidelity or a universal size increase. Encoding and quality receipts: `.downloads/gop-encoding-<id>/summary.json` and `.downloads/gop-quality-<id>/summary.json`. Full-resolution reference/variant PNGs were exported without enhancement; the image viewer resized the inspected images, so a native-pixel visual review is not claimed.

At explicit worst-case frame 959, copied native AV1 exact seeking without proxies measured final paints of 187.36/138.55/94.21 ms for those 120/60/30-frame intervals; legacy CUVID measured 266.60/187.83/122.06 ms. Only the native 30-frame case passed the unchanged 100 ms bar. These results motivated an optional next-capture Editing speed setting; Image quality remains the default. Existing files are unaffected.

The user's subsequent quality comparison exposed a limitation of the previous successful warm-scrub receipts: the 720p, all-keyframe 8 Mbps H.264 copy could meet latency thresholds while visibly damaging detail. Those latency results do not establish acceptable image quality.

The next controlled six-second section reproduced the blockiness in native-pixel HUD/scenery crops. The old derivative measured 35.543 dB/0.896955 SSIM against its 720p resized reference. A 1080p fixed-QP-26/GOP-6 derivative measured 41.397 dB/0.977515 against its 1080p reference; the different reference resolutions prevent treating those scores as a direct equal-detail comparison. Its native-pixel crop was visibly clearer. Receipts: `.downloads/seek-proxy-quality-<id>`. Each metric used 360 aligned frames, and actual keyframe gaps were checked. Fixed QP 18 was too large for the intended cache. A full QP-24/GOP-6 attempt hit the emergency limit after only 88.117 seconds; its rejected temporary file was removed after recording the receipt. QP 26 completed all 7,174 video packets/119.567 seconds at 544,723,460 bytes in 11.401 seconds. The accepted app cache was independently published in 11.587 seconds. This motivated bounded 576 MiB per-file/832 MiB total limits, not a quality-reducing retry. Source SHA-256 was unchanged.

Original refinement now has separate source-quality/geometry cache keys and publication barriers. After a quick frame is actually painted and 80 ms of idle time, the UI requests the original source while retaining quick pixels. New input cancels obsolete refinement using a scoped atomic generation; Play revokes refinement without restarting the proxy worker. Originals do not trigger full-resolution forward warming. Native and filter fallback paths were corrected to avoid fitting already-rounded preview geometry twice.

The independent same-target proof at frame 1,884 passed with an active completed proxy: fast 1280×720/76.86 ms, original 2560×1440/191.72 ms, hardware decoding true and mean BGRA error 0.180 against the independently rendered reference. The opacity-effects fallback passed at noncanonical bounds 1742×981, producing exactly 1742×978 and mean error 0.000; fast/refined render times were 1,075/1,093 ms. Effects remain a slower CPU composition route. Receipts: `.downloads/original-quality-<id>`; original SHA-256 unchanged.

Large-viewport pressure tests now explicitly preserve the requested window size instead of an unconditional 1440×900 fixture resize, and report actual painted dimensions. On the same completed one-slice proxy at the actual 1920×1080 drag size, one software thread produced 39 changing paints/2 s, median/p95 62.76/92.99 ms and final 108.92 ms. Two frame threads produced 40 paints, 63.41/93.36 ms and final 120.40 ms. Both retain zero temporal error and a correctly identified original-source refinement; both still fail the unchanged 100 ms final bar. Artifacts: `.downloads/scrub-pressure-<id>` and `scrub-pressure-<id>`. An intervening synthetic-pointer run failed its input-control assertion and was not used as a latency result. Two frame threads remain opt-in. The then-proposed two-slice experiment was subsequently measured, as recorded below.

## Current seek recipe 7 and remaining latency limits (2026-10-24)

This candidate supersedes the earlier 720p/8 Mbps, QP-26/GOP-6 and smaller disk-cache descriptions above. Recipe 7 uses H264 NVENC P3/ULL, maximum 1920×1080 without enlarging smaller sources, explicit bicubic scaling, all-intra frames, two slices per picture and constant QP 24 with `-i_qfactor 1 -i_qoffset 0`. Sampled slice headers confirmed initial QP 24; the default intra quantizer adjustment is not assumed. Decoder threads default to one. Two-slice/two-thread GOP-6 decoding had already failed the final-response bar at 138.87 ms; counters confirmed one through six decoded frames per seek, so coarse Matroska clusters were not the explanation.

The aligned six-second all-intra/QP-24 section occupied 62,217,909 bytes and measured PSNR 41.010379 dB / SSIM 0.973697 against the 1080p resized, already-compressed source reference. All 360 frames were checked. This quantifies additional derivative loss, not pristine-source quality; native-pixel text/scenery crops and motion review remain necessary. Receipt: `.downloads/seek-proxy-quality-<id>/summary.json`. The actual completed 119.566-second recording produced a published 1,235,247,828-byte proxy, rather than relying on the six-second size projection. Full-cache receipt: `.downloads/scrub-pressure-<id>`.

The disk acceptance cap is strictly below 2 GiB per file and the cache budget is 3 GiB. Preparation reserves 2 GiB plus 16 MiB and requires that reservation plus 512 MiB available space (2,576 MiB). The emergency encoder limit is 2 GiB plus 8 MiB, above acceptance, so size-stopped outputs cannot be published. The encoder/overall deadlines remain 90/120 seconds. Source identity, completion markers, encoder/index locking, active-reader leases and LRU eviction remain enforced; oversized or failed derivatives fall back to original decoding instead of lowering quality. Decoded-frame memory budgets are unchanged. A pinned 1,235,247,828-byte source plus the full preparation reservation exceeds the 3 GiB cache budget: another source cannot prepare while that lease remains active, even if both eventual derivatives might fit. Existing readers are preserved and the new source uses its original decoding path.

Original refinement remains separate from compressed preview pixels. The independent same-target check produced native 2560×1440 output with mean BGRA error 0.180 against its independently rendered reference; the opacity-effects fallback produced exactly 1742×978 at noncanonical bounds with mean error 0.000. Those original/effects receipts establish the exercised fidelity and geometry paths, not recipe 7's end-to-end latency. Original source SHA-256 remained `[redacted local media hash]` throughout these encoding, cache and pressure checks.

Tracing exposed a measurement perturbation. Traced full-viewport recipe-7 runs produced a fresh-cache final response of about 77 ms, then cached responses of about 109 and 106 ms that failed the unchanged 100 ms threshold. Another instrumented run measured a 95.838 ms final response, 41 changing paints during two seconds, and request-to-paint median/p95/maximum of 63.298/112.622/124.407 ms. Those failed receipts remain valid descriptions of the earlier trace-enabled operation, but fragmented synchronous diagnostic writes materially slowed the path and do not represent default production performance. Whole-line formatting with one stderr write has now replaced that fragmented output and passed the traced control below. The writer remains synchronous.

Three default, untraced production controls passed. Each dispatched 131 targets, painted the drag image at 1920×1080 and the original refinement at 1986×1118, and retained zero approximate frames/temporal frame error with unchanged source SHA-256.

| Receipt under `.downloads/` | Changing paints / 2 s | Request-to-paint median / p95 / maximum (ms) | Exact final (ms) | Original after release (ms) |
| --- | --- | --- | --- | --- |
| `scrub-pressure-<id>` | 116 | 29.8348 / 46.4142 / 48.1233 | 28.8026 | 432.5111 |
| `scrub-pressure-<id>` | 111 | 29.8624 / 45.7476 / 46.9383 | 27.8404 | 445.5328 |
| `scrub-pressure-<id>` | 114 | 30.0004 / 46.4048 / 48.6651 | 28.7999 | 443.1281 |

The first control's original refinement took 314.0007 ms from its own request to paint. All timings stop at the successful GPUI image-paint call and exclude GPU completion and monitor scanout. These repeated controls establish the exercised default configuration, not a universally smooth result across sources and hardware. Shared stage timestamps distinguish request queueing, decoding/cache copying, UI scheduling, binding and atlas upload, but their diagnostic overhead must be checked against the untraced workload.

### Final executable and trace-control verification (2026-10-24)

At this checkpoint, workspace/all-targets tests passed (desktop 170/one ignored; editor-core 97/four ignored), as did strict workspace Clippy with `-D warnings`.

The rebuilt executable's whole-line trace control passed: `.downloads/scrub-pressure-<id>` dispatched 131 targets and produced 114 changing paints in two seconds, request-to-paint median/p95/maximum 29.8468/46.5556/52.4329 ms, and a 46.5556 ms exact final response. Original refinement painted 544.9759 ms after release and 404.812 ms after its request. Actual quick/original image sizes were 1920×1080/1986×1118, temporal error was zero and source SHA-256 was unchanged. This agrees with the untraced controls' overall responsiveness rather than the earlier fragmented-log slowdown.

The bounded pipeline summary contained 117 paint receipts, zero malformed records and no truncation. Fast-path stage medians were queue 3.3848 ms, core decode 13.8458 ms, cache copy 2.1247 ms, UI handoff 0.0894 ms, UI bind wait 8.0288 ms, binding 0.6244 ms and image paint call 1.6618 ms. Native-reader median was 13.4914 ms; the previous roughly 33 ms gap between native/core timing disappeared. The pipeline's 172.2466 ms maximum includes initial-open warmup, a different sample scope from the pressure phase's 52.4329 ms maximum. Stage medians are separate distributions and should not be added as one request's timeline. GPU completion and monitor scanout remain outside these receipts.

The source-quality rerun at frame 1,884 passed: quick 1280×720 at 87.85 ms, original 2560×1440 at 197.89 ms, hardware decoding active and mean BGRA error 0.180. Request serial advanced from 1 to 2, confirming a new original-refinement result. The effects fallback produced 1742×978 at 1072.41 ms with mean error 0.000; it remains CPU composition, not a fast-path latency result. These checks were reported in the terminal; source identity and bytes were unchanged.

## Stationary trim positions and manual gap closing (2026-10-24)

Quick trims, deletes and speed changes now preserve other pieces' timeline positions; only the explicit core API `QuickEdit::Reorder` packs pieces. UI body dragging uses `Edit::Move` for the selected linked audio/video group. A trimmed start stays at the released edge, and blank gaps remain until manually closed. Source/neighbor bounds remain enforced. Generic restoration handles static clips without keyframe anchors while preserving animated transforms, and source-nanosecond restoration bounds use rounding with a 24-hour cap.

The first native check failed outward restoration (`.downloads/trim-position-<id>`), exposing a floor-rounding off-by-one in the source-duration bound. After correction, the rebuilt normal executable passed `.downloads/trim-position-<id>` in 880 ms. Both handles shortened/restored a single piece from 7,173 to 6,097 and back to 7,173 frames, and a two-piece case from 3,586 to 3,048 and back to 3,586 frames. The fixture checked the actual handle position after release, manual body movement to zero with Undo/Redo, linked source positions and unchanged neighbors. Its owned working directory isolated project writes; original source SHA-256 remained `[redacted local media hash]`.

The actual `quick_clip_fixture` also passed both exports under `.downloads/trim-export-<id>`, covering separate audio streams, waveforms, thumbnails, stationary trim/delete behavior, explicit moves, save/reopen, a 1600×900 preview and independent game/microphone export choices. The affected suites passed with desktop 172 tests/one ignored and editor-core 102 tests/four ignored. The complete workspace/all-targets run passed before the final small restoration correction; strict workspace/all-target Clippy with `-D warnings` passed after the final code. These receipts supersede earlier ripple/gap-free trim descriptions; they do not reclassify the initial failed native check as a pass.

## Native pane marking and region guides (2026-10-24)

The production Toolkit capture route snapshots native window/control rectangles on a background worker before opening the overlay. Enumeration has one admitted worker, at most 2,048 targets/4,096 visited handles/depth eight and a cooperative 50 ms budget checked between OS calls. A 100 ms UI-side receiver timeout opens manual selection with default geometry; an outstanding worker retains admission until its actual return and late geometry is discarded. Thus the timeout neither spawns replacement enumeration workers nor claims to interrupt an in-flight OS call. Top-level and sibling hierarchy order prevent selecting rear controls through foreground windows/controls, and child rectangles are clipped to ancestors.

Hover uses immutable geometry and existing annotation-Surface BGRA bytes, without OS queries or another full-frame copy. Its 11×11 magnifier submits 121 small pixel quads. Contrasting dashed bounds and global coordinates preview the candidate; click adopts it without completing capture, free dragging creates a custom rectangle, and committed selection handles retain move/resize priority. Annotation tools and pending copy/save operations remain isolated from target adoption. Guides are presentation-only.

The complete workspace/all-targets run passed with desktop 180 tests/one ignored and editor-core 102 tests/four ignored before one additional ignored interop test was added. That isolated test was explicitly run and passed in 0.03 seconds: owned offscreen STATIC windows exercised real native enumeration, nested controls, ancestor clipping, parent exclusion and destruction of every owned window. Offscreen/no-activation preflight used `WS_EX_NOACTIVATE`; the proof performed no capture, input automation or DPI changes. Final strict workspace/all-target Clippy with `-D warnings` passed including the added test.

Native GPUI guide fixtures with injected rectangles and negative physical origins first passed at 900- and 1440-pixel layouts under `.downloads/region-guides-<id>`. After label-width and 800-pixel toolbar fixes, all six final routes passed under `.downloads/region-guides-<id>`: hover-region at 900×600 and 1440×900, plus region, blur draft, pixelate draft and properties at 900×600. Both hover renders were visually inspected: coordinate text fits and all toolbar icons occupy one row. The properties render was also visually inspected with every control visible within the 600-pixel height. Both hover routes passed click adoption without prior pointer motion, free-drag override, no automatic capture, annotation-tool isolation and pending-operation guards. These native/owned-target checks did not operate WizTree or establish detection of windowless web/custom-drawn panes; such regions retain manual rectangle selection.

## Editor memory release (2026-10-24)

Leaving the video editor or closing to the tray drops playback/scrub sessions, cancels derived-media work and releases decoded frames, preview images, thumbnails and waveforms. Reopening rebuilds these caches while preserving edits and Undo/Redo. Image-editor close retires its tiles through a deferred app effect, without waiting for that editor to render again. Media epochs reject obsolete hide/show results, including cancelled source validation, while preserving owned edit history.

Reproduce with `scripts/verify-editor-memory.ps1 -Recording <recording>`. The final four-cycle run, `.downloads/editor-memory-<id>`, used the 119.566-second 1440p AV1 source, a completed recipe-7 proxy and a 2560×1440 viewport. Each cycle performed eight distinct exact seeks and settled original refinement, then checked release after a two-second cooldown. All releases had zero scrub workers, decoded-cache payload, editor images and waveforms; CPU-image weak references expired and the project was unchanged. This fixture does not test nonempty Undo/Redo history.

| Process memory (MiB) | Cycle 1 | Cycle 2 | Cycle 3 | Cycle 4 |
| --- | --- | --- | --- | --- |
| Active working set | 344.17 | 350.79 | 334.46 | 343.41 |
| Released working set | 157.07 | 113.81 | 114.09 | 114.27 |
| Released private working set | 91.54 | 48.26 | 48.53 | 48.71 |
| Released private commit | 454.86 | 467.42 | 453.62 | 468.78 |

These measurements show repeated resource release and a stable later-cycle resident baseline, not a return to startup memory or an exhaustive leak test. Two atlas textures remained: ordinary UI and one bounded reusable color texture (8,881,392 bytes nominal payload). Loaded FFmpeg libraries and allocator/driver capacity are outside cache counters; GPU payload, resident memory and private commit are different measurements. No working-set trimming is used. `RCR_TRACE_MEMORY=1` enables cache/worker accounting. The temporary CUDA reference-count probe was removed after renderer submission explained the apparent leak.

Earlier hidden runs appeared to grow roughly 80 MiB per cycle because they built GPUI scenes without submitting renderer work. The corrected fixture calls `present_for_diagnostics`; the earlier numbers are not process-leak evidence. Standalone-image close also passed all 25 toolkit stages with expired image weak references (`.downloads/image-release-<id>`). A scrub regression control retained 116 changing paints/two seconds and a 28.87 ms final response (`.downloads/scrub-pressure-<id>`). Source and project checks were unchanged.

## Audio selector layout (2026-10-24)

Settings audio selectors overflowed their cards because full-width buttons had block-layout ancestors inside flex rows. Shrinkable flex-column containers now keep labels and chevrons between the icon and toggle. Final Settings renders at 1440×1400 and 900×1400, and Capture at 1440×900, were visually inspected under `.downloads/settings-overflow-20261024`. Default labels fit without clipping. A 900×600 Capture render passed before the final label adjustment.

## Screenshot startup (2026-10-24)

Debug renderers now reuse a bounded process-wide cache of compiled shader bytes, keyed by stage and current HLSL/HLSLI contents. Source changes invalidate entries and compilation failures remain retryable. Release builds already embed shader bytecode. Screenshot pixel kernels are optimized in development builds; packed RGBA conversion reuses storage and packed BGRA conversion swaps channels in place, avoiding a second full-frame allocation. Padded rows still compact correctly.

Reproduce pixel/capture measurements with `cargo run -p rcr-toolkit --example capture-proof -j1 -- --startup`. On the tested 5120×1440 Windows desktop:

| Measurement (ms) | Before | After |
| --- | --- | --- |
| Alpha normalization | 77.07 | 5.09 |
| RGBA conversion | 194.82 | 5.53 |
| Three captures | 694 / 448 / 430 | 434 / 179 / 203 |
| Three Actor first previews | 43.09 / 40.83 / 40.78 | 37.94 / 33.84 / 33.61 |

An independent generated-image overlay run (`.downloads/screenshot-startup-<id>`) compiled fourteen shaders in the main renderer; the overlay had fourteen cache hits, zero compilations and 19.004 ms renderer setup. Overlay-entry-to-window creation was 34.99 ms, first preview 59.46 ms and first scene paint 95.84 ms. This excludes source capture, the configured 200 ms delay, geometry enumeration and GPU completion; it is not an end-to-end latency total. Its coalesced `render_us=1` field is not the initial Actor raster cost.

Two public Frame regressions cover allocation reuse, padding, alpha/channel order, origins and metadata. The initial two cache regressions passed via `scripts/verify-shader-bytecode-cache.ps1` (`.downloads/shader-bytecode-cache-<id>`). Desktop 182 tests/two ignored, toolkit 46 tests and strict workspace/all-target Clippy passed at this checkpoint. All 25 native toolkit stages passed under `.downloads/screenshot-toolkit-<id>`, including annotation Undo, clipboard-failure retry and image-resource release.

## Application staging (2026-10-24)

`scripts/package-windows.ps1` builds a release executable and stages only `dist/windows/rcr.exe`. `-SkipBuild` stages an existing release; `-DevelopmentExecutable <exe>` stages an explicit debug build in `dist/windows-dev/`. It verifies the copied executable and does not copy Cargo build artifacts or migrate user data.

A single-file development staging passed an isolated Settings render (`.downloads/package-check-<id>`). This establishes local staging, not cross-machine portability: debug GPUI shaders require the checkout, video-editor features require external FFmpeg tools/libraries, and data paths remain relative to the working directory. Release embeds shaders, fonts and icons, but release packaging was not validated in this checkpoint.

## Embedded MKV recording metadata (2026-10-24)

New native MKV recordings and replay clips embed versioned JSON in the global `_RCR_METADATA` TagString. It contains capture configuration, audio-track IDs/names/source kinds and actual clip duration/frame/keyframe/byte/packet counts. Full recordings add capture-stream-drain statistics under `session.statistics`; machine-local paths and opaque audio endpoint IDs are excluded. UTF-8 metadata is capped at 1 MiB and written during finalization without re-encoding video. Cues and Tags are indexed by SeekHead before synced, no-clobber publication. See the [Matroska tagging specification](https://www.matroska.org/technical/tagging.html).

Recording publication precedes offline replay processing. The `capture_stream_drain` statistics scope excludes later replay completions and metadata/publication time, which final UI statistics may include. MKV no longer writes `.stats.json`; existing media/JSON files are unchanged. Video-only IVF proofs retain external statistics, and editor autosaves remain separate.

Reproduce with `scripts/verify-recording-metadata.ps1 -Recording <existing-integer-fps-AV1-file>`. The native writer proof (`.downloads/mkv-metadata-<id>`) muxed an existing AV1 keyframe with two synthetic PCM tracks. FFprobe found the expected tags and separate system/microphone streams; both outputs decoded fully and extracted AV1 packets matched byte-for-byte. The source was unchanged. Media-I/O 9 tests, recorder 37 tests/two ignored and strict Clippy passed, covering EBML traversal, UTF-8, bounds, counts, publication collisions and statistics handoff.

## Cleanup verification (2026-10-24)

The shader-cache script now tests the shared production module directly: four tests pass, covering reuse, source invalidation, failed compilation/retry and bounded eviction. Media-I/O's nine tests also pass after removing an unused replay metadata API. The memory fixture no longer asserts preservation of an empty Undo/Redo history; its default executable is the normal desktop build. Obsolete CUDA reference-count instrumentation and an unused D3D debug-layer override were removed.

Broader Windows tests could not compile because rustc exhausted system memory while compiling the Windows bindings. No updated application executable or native verification is claimed for this cleanup.

## Disk usage analyzer (2026-10-24)

Six scanner tests pass through `cargo test -p rcr-toolkit --lib disk_usage::tests -j1`: aggregation, cancellation, invalid roots, hard-link logical counts, disappearing directories and conditional Windows symlink handling. The symlink fixture may skip when creation privileges are unavailable. Five standalone treemap tests cover proportional areas, bounds, overlap, empty/invalid inputs, extreme weights and 1,024 entries. Desktop/toolkit strict Clippy passes.

The desktop build passed with an isolated output because the normal executable was running. `scripts/verify-disk-usage.ps1` scanned an owned fixture and rendered the actual GPUI UI at 1440×900 and 900×600, including the compact treemap tab. The snapshots under `.downloads/disk-usage-<id>` showed the expected 15 MiB, four files and three folders; visual inspection found no clipped controls. This checks native scanning/rendering, not manual pointer interaction or whole-drive performance. Results use logical size and directory traversal; privileged NTFS enumeration and allocated-space accounting remain unimplemented.

The subsequent dense layout uses 21-pixel rows, aligned count/percentage columns, a full-width File View and a bounded nested treemap. Ten treemap tests pass, including subtree geometry, remainder area, depth limits and rendering budgets. Strict desktop Clippy and the isolated desktop build pass. Native renders under `.downloads/disk-usage-<id>` show the expected 26.2 MiB/84 files/three folders in Tree View and File View at 1440×900, plus Tree View at 900×600. Visual checks confirmed column alignment and removal of the redundant toolbox footer; File View hides its treemap by default. These remain render checks, not manual input or whole-drive benchmarks.

Native Windows type icons passed a pixel-conversion test and shell checks for closed/open folders, generic files and ZIP files. Strict desktop Clippy and the isolated build pass. Renders under `.downloads/disk-usage-<id>` confirm native icons and transparent backgrounds in both views and the extension legend at the same viewport sizes. Shell queries run on a dedicated worker using synthetic type names and `SHGFI_USEFILEATTRIBUTES`; caches and request queues are bounded, and Windows icon/GDI resources are released after conversion. Clear invalidates pending results and retires the view's image cache.

Explore adds an independently implemented sunburst/ranked-list workspace inspired by SquirrelDisk, with clickable breadcrumbs and an alternate large treemap. Five sunburst tests pass for angular proportions, grouping, work/depth limits, hit testing and empty trees. Ring widths adapt to the visible depth, and folder navigation resets list scrolling. Strict desktop Clippy and the isolated build pass. All six native captures under `.downloads/disk-usage-<id>` completed: existing Tree/File views, Explore at 1440×900 and 900×600, and Explore's treemap. Visual inspection confirmed readable charts and no clipped controls. These renders do not establish manual pointer behavior or live chart updates during a scan.

### NTFS fast path

The scanner now attempts read-only raw MFT streaming for whole local NTFS volume roots. It reads aligned 8 MiB chunks, validates update-sequence fixups and extent mappings, resolves bounded resident/nonresident ATTRIBUTE_LIST continuations, and builds the tree using full record/sequence references. Unsupported layouts and unavailable volume access fall back to Windows directory batches, which obtain file names, attributes and logical lengths in one enumeration rather than querying each file's metadata. Allocated-space accounting is still absent.

All 29 disk scanner tests pass, including nine parser tests, seven volume-reader/mapping tests, five tree-assembly tests, two Windows enumeration tests and the original six scanner tests. Fragmented-reader tests use temporary file-backed raw FILE records and attribute lists; they cover stale sequence numbers and unreachable extents. The temporary hard-link fixture requires execution outside the filesystem sandbox. Strict Clippy passes for all desktop and toolkit targets.

This session's Windows token is not an administrator. A read-only `\\.\C:` access probe returned access denied even outside the sandbox. Actual whole-drive MFT throughput, FSCTL behavior on a live volume, and performance parity with WizTree have therefore not been verified. Run RCR as administrator and scan a whole drive to exercise that backend; the UI reports the backend actually used and any fallback reason. `disk_scan_bench` prints backend, timing and logical totals for independent measurements.

The isolated desktop build succeeded. All six native renders under `.downloads/disk-usage-<id>` show the expected 27,443,200 bytes (26.2 MiB), 84 files and three folders, with the actual Windows directory backend in the status bar. Visual inspection found no clipped controls at 1440×900 or 900×600. The benchmark command independently returned the same totals and zero errors in 0.001 seconds on this small warm fixture; that is not a whole-drive speed measurement. The current packaged executable was open and could not be overwritten, so the candidate was staged separately at `dist/windows-dev-ntfs/rcr.exe` and its SHA256 verified against the built source: `[redacted local build hash]`. Launch from the repository root to retain the same relative data paths; this development build still uses checkout shaders and the existing external media runtime.


## Native screenshot workspace migration (2026-10-24)

All 23 former SSX crates are direct RCR workspace packages with `rcr-*` names. Cargo metadata reports 31 members and eight default application targets, with no SSX package or dependency on the retired vendor path. All literal embedded-asset paths resolve. The original GPL license is byte-identical to the retained top-level license; serialized project formats and credential/storage identifiers remain compatible.

The migrated workflow, image editor/effects, upload and toolkit packages passed 838 tests across 23 unit/integration suites, including legacy project fixtures. The desktop check and isolated executable build passed, as did the optional CLI check with `--no-default-features`. Strict Clippy passed for the desktop/toolkit targets with `--no-deps`; migrated libraries retain existing upstream style-lint warnings when linted separately. No broad lint suppression was added.

The rebuilt desktop passed the native toolkit's 25-stage smoke test, including workflow navigation, annotation overlays, standalone editing and resource release. All six disk-usage render cases completed with the expected 26.2 MiB, 84 files and three folders; representative wide Tree View and compact Explore renders were visually checked. These checks use synthetic inputs and hidden GPUI windows, without desktop automation. Linux backends, optional egui interfaces and linked FFmpeg recorder features were retained but were not rebuilt or exercised in this migration.

A secret scan of the exact staged export found only the two previously reviewed keyboard/AWS fixture false positives. Personal upstream UI screenshots remain excluded at their new crate paths.
