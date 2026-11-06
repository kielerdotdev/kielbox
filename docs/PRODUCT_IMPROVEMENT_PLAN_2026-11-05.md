# RCR improvement plan and execution ledger

Date: 2026-11-05, Europe/Copenhagen. Owner: primary agent coordinating recording,
editor/library and screenshot/reliability subagents. Requested scope is the whole
application, with hardware recording-stop verification as the first release gate.

**This implementation/review wave is complete for the documented local profile.**
All local review gates passed for `dist/windows-reviewed-2026-11-05/rcr.exe`,
SHA-256 `1EEA531A1D2AB8D8151BE1E84C8781A6A04F0E025D413450DC2B01A97698E63D`.
The hash-bound [final acceptance JSON](../.downloads/plan-continuation/release-acceptance.json)
records affected-suite tests, actual packaged hardware, clean-working-folder
diagnosis/render/native checks, six viewer runs/80 unchanged budget checks and
post-runtime verification of all 22 package payloads. This does not close the
remaining capability roadmap or public-distribution license/source review.

## Outcome and evidence rules

RCR should make capturing, finding, watching, editing and sharing a moment feel
immediate, while preserving the original recording and saved edits. Reliability
comes before new integrations. A passing unit test cannot establish hardware
capture behavior; a fast decoder cannot establish a fast rendered UI; a completed
capture cannot establish a durable, playable file.

Every implemented item needs a concrete trigger, a regression or measured proof,
and a stated boundary. Compare the same fixtures and settings. Preserve all
existing recordings, projects and uncommitted development work. Do not upload
recordings, send messages, enable microphones or change account settings during
evaluation. Actual desktop recording is explicitly authorized for this pass.

## Research and design implications

These are observations from official product documentation, accessed 2026-11-05,
followed by proposed RCR decisions. They are not measured competitor benchmarks.

| Reference | Observed workflow | RCR decision |
| --- | --- | --- |
| [Medal sharing](https://support.medal.tv/support/solutions/articles/48001169792) | Library selection opens a preview, with sharing available from that preview and a preferred sharing option. | Watching should be the default video action; editing and sharing should be nearby explicit actions. Preserve Library order in a watch session. |
| [Medal clip management](https://medaltv.freshdesk.com/support/solutions/articles/48001164598-how-to-manage-clips-on-medal) | Library organization, filtering and clip management form a separate task from editing. | Keep browsing and lightweight watching responsive without constructing editing timelines or proxies. |
| [OBS recording guide](https://obsproject.com/kb/standard-recording-output-guide) | Recording path, audio tracks, quality and remuxing are separate choices; MKV is recommended for recording resilience. | Separate capture shutdown, durable local publication and optional conversion. Show the actual stage and retain usable recordings on post-processing failure. |
| [OBS hybrid formats](https://obsproject.com/kb/hybrid-mp4) | Recoverability and broad playback compatibility are distinct requirements addressed by the output format. | Evaluate recoverable MP4 as a later format project; do not replace the existing writer without crash/recovery proofs. |
| [OBS hardware encoding](https://obsproject.com/kb/hardware-encoding) | Hardware encoding reduces CPU work, and adapter choice matters. | Test the real GPU/display/audio combination and publish its identity with timing evidence. Avoid a software synthetic result presented as hardware proof. |
| [ShareX editor](https://getsharex.com/docs/image-editor) | Capture, annotation, save/copy and configured after-capture work form one continuous workflow; annotations remain editable before final output. | Preserve exact capture pixels, allow cancellation at stage boundaries, and publish files atomically. Keep uploads user-triggered. |
| [Outplayed capture modes](https://support.overwolf.com/support/solutions/articles/9000284659-how-to-use-outplayed) | Manual, highlight, match and session capture differ, and game-event failure may require a full-session fallback. | Make capture state and requested mode truthful. Session grouping and bookmarks can precede brittle game-event integrations. |
| [NVIDIA encoder guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.1/nvenc-video-encoder-api-prog-guide/index.html) | EOS flush, resource release and completion waits have explicit ownership requirements. | Instrument actual stop stages, bound completion waits and retain capture ownership until teardown really ends. |

## Priority and dependencies

P0 prevents lost work or an unusable capture. P1 makes everyday workflows fast and
predictable. P2 expands capability after the earlier gates pass. Targets below are
acceptance criteria, not claims of achieved measurements.

| ID | Priority | Workstream | Acceptance gate | Dependency |
| --- | --- | --- | --- | --- |
| R1 | P0 | Hardware recording stop | Actual Stop dispatch to worker retirement and UI idle measured separately; short and 60-second clips; no-audio and desktop-audio cases; output fully decodes. No unexplained 30-second wait. | Real DXGI/NVENC/audio devices |
| R2 | P0 | Replay ownership and finalization | Replay save immediately before stop completes or reports a useful failure; no worker/session overlap; capture shutdown does not wait on optional replay processing. | R1 instrumentation |
| R3 | P0 | Failure preservation | Encoder/audio/mux failure retains usable media where possible and reports missing requested tracks. No silent success. | R1/R2 |
| S1 | P0 | Screenshot file publication | A failed replacement does not truncate the previous file. The completed file is published from the same directory. | Filesystem fault tests |
| S2 | P0 | Cancellation correctness | Cancellation during slow native capture or region selection prevents later output/persistence. | Injected late-cancel tests |
| S3 | P0 | Multi-display memory safety | Sparse/invalid monitor layouts are rejected before excessive allocation; negative origins and ordinary multi-display output remain correct. | Geometry tests |
| E1 | P1 | Watch-session responsiveness | Cancel obsolete loads, bound work, preserve queue order and selection, avoid editor-only processing while watching. | Rendered first-frame/clock evaluation |
| E2 | P1 | Editor and Library correctness | Fix concrete audit defects in navigation, filtering, source recovery or asynchronous result ownership; preserve edits and undo history. | Focused regressions plus hidden UI proof |
| E3 | P1 | Large-library behavior | Exercise thousands of catalog entries, off-screen/paged navigation and pending thumbnails without unbounded visible work or synchronous probing. | E1/E2 |
| O1 | P1 | Diagnostics resilience | A failed diagnostic job cannot permanently kill future reports; repeated editor errors cannot grow one log forever; no UI-thread joins. | Panic/rotation/blocked-worker tests |
| O2 | P1 | Whole-app validation | Repeat capture/menu, editor, Library, screenshot and shutdown checks; report build hash and scoped evidence. | Implemented wave below |
| U1 | P1 | Capture status clarity | Starting, recording, stopping, saved and failed states agree between Capture, tray and shortcuts. Button feedback is immediate even when finalization continues. | R1 and state-machine tests |
| U2 | P1 | Sharing workflow | Watch preview exposes a clear path to export/copy a compatible file; filename and size are visible; clipboard publication only on explicit action. | E1/E2, export fidelity |
| D1 | P1 | Storage and settings recovery | Explain actual data location; preserve invalid settings for recovery; make disk-full/permission/offline-drive failures actionable. Never silently replace recoverable state. | Storage format design and fault tests |
| D2 | P1 | Bounded caches and background jobs | Define memory/disk budgets for thumbnails, proxies, decoded frames, replay snapshots and diagnostics; cancellation releases ownership once. | E3, O1 |
| Q1 | P1 | Export trust | Compare trims, audio gains, multi-track mix and timing against references; canceled/failed exports never replace a good destination. | Existing editor reference fixtures |
| P1 | P2 | Portable distribution | A clean-machine launch identifies missing runtime/encoder capabilities; built artifact hash and dependencies are recorded; upgrades retain data. | Stable validated build |
| A1 | P2 | Audio-device recovery | Explicit selection, disconnected-device state and default-device changes have recovery paths without inventing a successful track. | Real device matrix |
| C1 | P2 | Session organization | Group clips by date/session/game and support favorites and lightweight bookmarks before adding remote watch synchronization. | Reliable Library catalog |
| C2 | P2 | Watch-session polish | Thumbnails, keyboard discoverability, optional fullscreen and audio monitoring state are consistent across clip changes and screen sizes. | E1/E2, product review |
| X1 | P2 | Broader hardware support | Capability-driven H.264/HEVC and non-NVIDIA backends; explicit unsupported messaging until actually validated. | New backend implementation/tests |
| X2 | P2 | Game-aware automation | Per-game settings and event bookmarks degrade to manual/session capture without losing footage. | C1, capture lifecycle reliability |

## Execution waves

### Wave 1: current authorized implementation

Run three parallel code workstreams with explicit file ownership, and centralize
Cargo builds to avoid lock contention and mixing binaries. Recording owns the
native pipeline, a repeatable hardware harness and its report. Editor owns
editor/Library fixes and audit evidence. Screenshot owns capture services, atomic
file output and display bounds. The primary agent owns diagnostics, integration,
the plan and cross-workstream verification.

1. Inventory current hardware and running capture processes. Do not disrupt an
   existing recording. Build a harness against current source.
2. Run actual short capture cases first, then 60-second cases and replay overlap.
   Never enable the microphone. Keep media local. Record configuration, stage
   timings, output duration/tracks and full-decode results.
3. Fix concrete defects found by each audit, with deterministic failure cases.
   Add explicit cancellation, bounded work and recoverable failure handling where
   evidence shows a gap. Do not add speculative compatibility workarounds.
4. Exercise the real Desktop Stop route and wait for true worker retirement and
   the saved/idle state. A CLI-only measurement is insufficient to close R1.
5. Run affected libraries, Desktop tests and primary-package Clippy. Repeat the
   hidden rendered viewer evaluation after editor changes and capture/menu proof
   after screenshot changes. Inspect native render output.
6. Stage a distinct evaluated executable, verify its hash, and record actual
   results in this ledger and the focused reports. Preserve old builds and data.

### Wave 2: stability and everyday product quality

After Wave 1 evidence, address the next measured bottleneck rather than a broad
rewrite. Prioritize large libraries (E3), storage/settings recovery (D1), export
trust (Q1), and consistent status/sharing (U1/U2). Each change gets its own trigger,
regression and bounded UI proof. A complete redesign of the editing timeline is
separate from making watch sessions fast and reliable.

### Wave 3: capability expansion

Portable packaging, broader codecs/GPU support, device recovery and session/game
automation follow the reliability gates. Remote synchronized watch parties,
cloud accounts, uploads and game-event services require explicit product and
service choices. They are not silently enabled by this task. Independent local
features and reversible code preparation can proceed without such accounts.

## Evaluation matrix

| Surface | Routine case | Stress/failure case | Evidence |
| --- | --- | --- | --- |
| Actual recording | RTX hardware, short/60-second, AV1, no audio/desktop loopback | Replay queued at stop, repeated sessions, stop during idle desktop | Monotonic timestamps, driver/GPU/config, JSON stage times, FFprobe and full decode |
| Desktop lifecycle | Same start/stop methods as buttons | Early terminal event, slow destructor, late replay result | Dispatch time, true worker retirement, saved/idle observation |
| Watching | H.264/AV1, AAC/two PCM tracks, two window sizes | 100 rapid switches, source missing, queue end, natural EOF | Successful hidden native paint, advancing silent clock, edit/source preservation |
| Editing | Seek, trim, mute/volume monitoring, autosave | Seek cancellation, unsupported native format, failed export | Pixel/audio references, generations, saved-project and undo checks |
| Library | Current sort/filter/favorites | Thousands of entries, pagination, unavailable folder/source | Pure ordering/selection tests plus hidden keyboard proof |
| Screenshot | Region/window/monitor, native frozen handoff | Cancel during capture, sparse topology, failed overwrite | Exact pixels, no persisted canceled result, bounded allocation, old bytes retained |
| Background work | Reports, thumbnails, preferences | Blocked disk, queue full, writer panic, repeated errors | Bounded admission, completion receipts, no UI joins, capped files |
| Distribution | Distinct staged build | Existing data, missing optional runtime | Source/artifact hash, explicit dependency notes |

## Execution status

| Item | State | Evidence / next action |
| --- | --- | --- |
| Research and whole-app plan | Written | Official references and acceptance matrix above; focused agent audits supplement this plan. |
| R1 actual hardware verification | Current packaged release and earlier development profile passed | RTX 5070 / driver 610.88, 2560×1440 60 FPS AV1. Packaged release four Desktop 6/60-second no-audio/system cases: UI idle 93–252 ms, Library 125–313 ms, all decode/content/PCM/preview/retirement gates passed. Earlier 12 worker + four development Desktop cases produced 20 clips (worker 48–139 ms, UI 31–62 ms). Twenty-four clips total across distinct builds, no 30-second wait reproduced. No loaded-CS, microphone or hour-long claim. |
| R2 replay ownership | Implemented; requested saves preserved in hardware profile | Short/60-second no-audio/system cases cover replay-only save on Stop and replay requested near Stop, retaining both full recording and replay where applicable. Hardware saves finished before pending-job handoff was needed; persistent slow offline cleanup ownership remains supported by deterministic tests rather than this hardware matrix. |
| S1/S2/S3 | Implemented; final source checks and actual screenshot measurement passed | Atomic Save As, late cancellation guards, invalid-mask rejection and composite allocation preflight before desktop batch capture. Latest platform 16 and screenshot 4 tests passed. Actual two-display 5120×1440 screenshot capture median 124.7371 ms, first 286.6926 ms; pixel analysis excluded and no image persisted. Earlier toolkit/services/workflow checkpoints remain separately scoped. |
| E1/E2 | Implemented; source/native checks passed | Obsolete-thumbnail cancellation/retry, source-labeled late export receipts and keyboard scrubber control. Library keyboard/import proofs passed in 521/2331 ms, hidden editor/controller in 7777 ms. Separate owned nonactivating native fullscreen evaluation passed in 8547 ms total: actual entry/exit, strict bounds/styles, sidebar hide and immediate hide during entry, no activation or OS input. Hidden historical attempts retain their narrower scope; manual foreground experience is unverified. |
| E3 cold Library worker | Implemented; bounded-worker/native/benchmark checks passed | One replaceable pending input/result, immutable catalog and generation-checked Updating/Retry views. For 50k entries, date/name-search admission measured 4.128/9.746 ms; background completion 607.194/402.166 ms; repeated cached-query medians 1.991/1.486 ms. These exclude complete painted-frame latency. No stale cards/watch queue exposed while pending. |
| D1 preferences recovery | Implemented; integrated checks passed | Invalid readable preferences get an exact synced unique recovery copy before defaults may save; unreadable/oversized state or failed backup blocks writes. Eight focused recovery tests and the integrated Desktop suite passed, including an 8 MiB save cap. Startup path remains fixed. Measured Installation sections passed at 900 and 1440 pixels; the 900-pixel render was visually inspected. General offline-drive/disk-full UX remains broader follow-up. |
| D2 seek-cache provenance | Implemented; focused checkpoint passed | Existing byte/lease/reservation budgets retained. New `seek-v3-managed` namespace, synced ownership sentinel, claimed partials/completions and conservative nonrecursive eviction preserve foreign/legacy/modified files. Core 120-test checkpoint and five native audio/parity checks passed. Reparse fixture was unavailable with privilege 1314; do not count its conditional skip as coverage. |
| Q1 export trust and compatible codec | Implemented; focused reference proofs passed | Cancellation/source-change checks precede no-clobber publication. Unity-speed sample alignment corrected: full/trim-gain/mute/solo references were exact; trim-fade maximum sample error 2.98e-8. Generated one-second H.264 NVENC/AAC share export fully decoded 60 frames, SSIM 0.987799 versus the lossless graph, preserving source/project and existing output. This is not exhaustive effect/speed/codec coverage. |
| A1 default system-audio recovery | Implemented for native default loopback; physical-change matrix open | Default-console-render notifications and typed WASAPI invalidation permit three reopen attempts per recording within an elapsed retry-scheduling window; original QPC gaps and overlap handling remain truthful. Explicit devices/microphones/process sources never switch. Injected regressions plus a fresh actual six-second current-endpoint baseline passed: 50.34 ms Stop, 361 decoded frames, 600 PCM packets, no errors. No physical unplug/default change was performed; synchronous driver calls have no promised hard timeout. |
| P1 doctor and safe staging | Same-PC release build/staging/hardware/clean-working-folder checks passed | SDK discovery, normal packager and 22-payload integrity checks passed; fixture suite passed 34 assertions. Actual packaged hardware plus separate-space-containing-working-folder diagnosis/launcher paths/rendered share/native fullscreen passed. Diagnostic phase created zero files. This uses installed FFmpeg on the same PC, not clean-machine or exhaustive-DPI acceptance. Exact corresponding source/transitive-license review remain public-distribution gates. |
| U2 local sharing | Implemented; source/layout/export/native checks passed | Clip show filename/size, explicit bounded Full-HD H.264/AAC preparation and source-labeled Copy/Show receipt preserve edits/history and monitor/export audio separation. All eight share views passed bounds/visual checks; actual H.264 fidelity and owned nonactivating native fullscreen geometry/restoration passed. Hide restoration now awaits entry and exit acknowledgment within a deadline. No clipboard transfer, manual foreground experience, or release acceptance is claimed. |
| O1 | Implemented and tested | Diagnostic job panic recovery and editor-log rotation to one 4 MiB backup, including bounded migration of old oversized logs. Regressions passed in the Desktop suite. The independent synchronous crash hook is outside this cap. |
| O2 / evaluated builds | Complete for this local review wave | Affected primary suites passed 446 tests (253 Desktop, 120 core, 53 Windows, 16 platform, 4 screenshot), 19 default-ignored; example pixel test passed separately, packaging 34 assertions and strict primary Clippy passed. Exact release SHA above passed actual hardware, clean-working-folder CLI/render/native and final six viewer runs/80 budget checks with zero failures. Post-runtime 22-payload verification and hash-bound acceptance JSON passed. Not a fresh entire-workspace or clean-machine qualification. |
| Remaining roadmap | Planned | Rows without implementation evidence remain open; this document does not claim they are complete. |

Earlier accepted development hardware evidence is under
`.downloads/hardware-resume-20261105-181802/`: `worker-short/`, `worker-long/`,
`desktop-preview-fixed/` and `desktop-long/` aggregate JSON all report success.
The worker matrices independently decode eight clips each; Desktop supplies four
more clips with real preview draws and saved/idle/Library checks. Earlier locked
black-source attempts and the initial zero-preview-counter Desktop proof remain
preserved as limited historical evidence, not acceptance. Test runners still
refuse LockApp/LogonUI/unavailable-display state without unlocking or waking it.
The focused hardware report gives exact timings, paths and build identity.
Current packaged release evidence is separately preserved at
`.downloads/plan-continuation/release-hardware/desktop-evaluation.json` with
`success=true` and the exact staged release SHA. Its maximum UI idle was
251.8607 ms, not the earlier development matrix's 62 ms. The slower six-second
system row spent 203.0252 ms in durable writer finalization; the reason for this
variation is unknown. All four release clips remain valid. Separate release
clean-working-folder CLI/render/native acceptance subsequently passed; see the
packaging report. These independent receipts still do not establish clean-machine
or loaded-game behavior. The final packaged viewer sweep and hash-bound artifact
receipt subsequently passed; exact evidence is linked at the top of this ledger.

Earlier Wave 2 Library measurements (25 runs, development test profile) reduced repeated
50,000-entry queries from 746 ms to 2.45 ms for date order and from 502 ms to
3.01 ms for name/search. Pre-worker cold construction took 588–673 ms. These are
query measurements, not total painted frame times. E3 now moves cold work off the
UI with bounded cancellable generation ownership. The latest 50k-entry benchmark
reports 4.128 ms date-order and 9.746 ms name/search request admission separately
from 607.194/402.166 ms background completion; cached medians are 1.991/1.486 ms.
This establishes bounded submission and measured worker/query cost, not total
painted-frame latency. Pending views expose no stale cards/watch queue. Exact
receipts are in `.downloads/plan-continuation/library-background-benchmark.log`;
corrected native navigation/import/controller logs remain alongside it.

Focused execution records:

- [Editor and Library audit](EDITOR_AUDIT_2026-11-05.md).
- [Screenshot and reliability audit](SCREENSHOT_RELIABILITY_AUDIT_2026-11-05.md).
- [Actual hardware stop evaluation](HARDWARE_STOP_EVALUATION_2026-11-05.md).
- [Actual screenshot capture timings](ACTUAL_SCREENSHOT_EVALUATION_2026-11-05.md).
- [Seek-proxy cache ownership](SEEK_PROXY_CACHE_AUDIT_2026-11-05.md).
- [Default desktop-audio recovery](AUDIO_DEVICE_RECOVERY_2026-11-05.md).
- [Safe Windows packaging](WINDOWS_PACKAGING_2026-11-05.md).
- [Local watch sharing and fullscreen proof limits](WATCH_SHARE_AUDIT_2026-11-05.md).
- Central checks under `.downloads/hardware-eval-2026-11-05/`, including
  `desktop-tests-final.log`, `reliability-final.log`, and `library-benchmark.log`.

## Known limits to report explicitly

An active ordinary-desktop hardware recording is real capture/encode/audio/file I/O but is
not a loaded CS match. No microphone test is included. GPU/driver/display changes
can alter behavior. A finite test matrix can establish observed stop latency and
catch the reported scale of delay without proving every device, codec, duration
or disk condition. Synthetic and hidden-window results remain separately labeled.

## Next bounded implementation after release validation

Release validation for this local wave has passed. C1 session organization, X1
broader hardware/codecs, X2 game automation and the Studio-wide recent-export
shelf below remain open; completion of this wave is not completion of all software.

Extend the existing successful export receipt into a bounded Studio-wide recent
exports shelf (for example, eight receipts) visible from Library/Capture as well
as Editor. Keep the current single export worker and its destination/source job
identity; the shelf should reuse cached completion metadata rather than stat files
while rendering. Navigation must not lose or relabel success/failure, and Copy
file/Show folder remain explicit actions. Test late completion after leaving
Editor, replacing the active source, cancellation/failure and bounded eviction,
then render the receipt from a hidden Library host. This addresses the remaining
discoverability gap after U2 without adding uploads, accounts or another encoder.

To use the reviewed build, run this from the **same existing RCR working/data
folder** you already use (the repository root for this evaluation). This is a
launch instruction only; the evaluation did not automatically launch the user's app:

```powershell
powershell -NoProfile -File .\dist\windows-reviewed-2026-11-05\Start-rcr.ps1 -WorkingDirectory (Get-Location).Path
```

If that data folder is elsewhere, use the package launcher's absolute path and
pass that existing folder explicitly. Keep its recordings/projects; no migration
or new data anchor is required. Quit an older running build from its tray first.
