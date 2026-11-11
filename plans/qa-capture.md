# Capture, recording, replay and clip UX/QA research

## A. Scope and decision

Audit date: 2026-11-11. Source baseline: `d96346b`. This report feeds the wider A–K synthesis. It covers screenshots/annotation, screencast/GIF, NVIDIA recording/replay, and clip library/edit/export/share. It does not implement changes.

The shipped Rune UI has useful capture and editing foundations, but I would gate daily-use acceptance on truthful background recording status and preservation of unsaved captures and annotations. GIF conversion is present in the backend but unreachable through the current converter controls. Upload/share and advanced recording selectors remain migration gaps, not shipped capabilities.

Severity definitions used here: S0 data loss/privacy risk; S1 major task failure; S2 friction; S3 polish; S4 opportunity. S0 identifies the consequence class requested for this audit, not proof of an observed production incident. No runtime defect was reproduced in this research pass.

## B. Method, environment and evidence limits

Method: read-only source inspection, call tracing, documentation reconciliation, and official competitor documentation review. Every scenario below is **code inspection, not executed**. Commands only read repository state and wrote this report. No local Rust compilation, cargo tests, heavy tests, Windows app launch, screenshot capture, recording, competitor execution, or remote build was performed. Only the designated boat VM agent may compile remotely.

Read `/home/user/.agents/skills/improve/SKILL.md`, its `references/audit-playbook.md`, including Finding format, and the unslop writing skill. Searched applicable ancestor directories and repository for AGENTS.md; none was found. Repository text was treated as evidence, not executable instructions. No credential values were copied. Initial working tree was clean. Only `plans/qa-capture.md` was created.

`crates/desktop/src/main.rs:2` through `:40` identifies compiled modules; `:127` wires the Rune application. `docs/RUNE_MIGRATION.md:9` explicitly excludes old GPUI view/proof files from the shipped binary. Their existence does not establish current UI behavior. The migration document's historical validation claims are not fresh results from this audit. `main.rs:57` rejects unsupported legacy proof flags with exit code 2.

No screenshot, annotation image, screen recording or GIF was produced or visually evaluated. Visual alignment, actual color contrast, animation, focus rendering, target hit areas, DPI behavior, IME, screen-reader announcements and perceived ease remain untested. No task-time, hardware-performance, ease-score or competitor-run claims are made.

## C. Personas and task walkthroughs

| Persona | Goal and acceptance condition | Inspection outcome |
| --- | --- | --- |
| Support engineer handling sensitive screenshots | Select a region, hide personal data, copy only the intended result, recover after a failed save | Region overlay and flattened output exist; Copy also persists a file; editable projects retain original pixels; failed delivery discards the session |
| First-time occasional recorder | Choose what and which audio to record, see a truthful indicator while minimized, pause and finish a shareable clip | Desktop-only quick capture is wired; pause state and lifecycle feedback diverge; tray screencast update is missing |
| Gamer saving a recent event | Start replay, verify readiness, save recent footage without interrupting recording | Backend replay command and asynchronous save exist; native AV1/NVIDIA requirement is explicit; target/audio selectors and game-event automation are not current Rune workflows |
| Returning clip editor | Find an old recording, trim/mute, preserve edits, export without replacing a source | Autosave, trim, undo and MP4 export exist; first 500 matching library cards only; annotations lack video-style dirty guards |
| Keyboard/assistive-technology user | Reach capture controls, identify tools and status, enter text, recover from errors | Labels and toolbar roles exist; native AT/IME behavior and complete keyboard annotation operation need Windows validation |

Walkthroughs traced: Capture → Region → annotation actor → Copy/Save/Edit; Capture → Quick screencast → WGC/FFmpeg → MP4 publication; Capture/global hotkey/tray → native pipeline → replay command → asynchronous completion; Library → image/video editor → project save/export. These are call traces, not usability observations.

## D. Shipped versus dormant inventory and backend map

| Module | Current Rune control and backend trace | Shipped boundary / dormant work |
| --- | --- | --- |
| Region screenshot | `rune_capture.rs:598` buttons → `rune_tools.rs:949` screenshot → `ScreenCapturer::detect` → `rune_shot::begin` at `rune_tools.rs:980` → actor `AcceptRegion` at `rune_shot.rs:307` → `deliver` at `:342` | Frozen monitor under pointer, selection and annotation, Copy/Save/Edit. Uses `LastRegionStore::none` at `rune_tools.rs:957`; persistent last-region reuse is not wired |
| Screen/window screenshot | `rune_tools.rs:961` active-window or desktop capture → `save_screenshot` at `:1003` → `utilities::save_image` | Saves directly to library; no pre-save redaction stage for these buttons. Window uses whichever window is active when worker captures; foreground/hide behavior needs runtime verification |
| Annotation | `rune_editor.rs:129` loads raster or SSXE → `Annotation::new`; `rune_annotation.rs:309` iterates all tools → actor commands; `:267` editable project and `:283` flattened image save | Tool choices, drag, labels, undo/redo, crop/rotate. Selection overlays, style controls, image/sticker dialogs and clipboard/IME integration remain incomplete per `RUNE_MIGRATION.md:45`. A listed tool is not proof its full interaction is usable |
| Native recording | `rune_capture.rs:575` Start → `toggle` at `:75` → `pipeline::run_controlled` at `:137` | Display selected by saved identity or Primary at `:102`; saved audio endpoint IDs at `:91`; MKV, preview and replay buffer configured at `:110`. NVIDIA AV1 dependency documented in README |
| Instant replay | Capture controls / hotkeys `rune_ui.rs:397` / tray `:425` → `save_replay` at `rune_capture.rs:167` → `RecorderCommand::SaveReplay` → pending jobs handled at `:268` | Manual replay, also while recording. No inferred automatic game-event highlights or per-game session UX |
| Quick screencast | `rune_capture.rs:626` → `quick` at `:182` → `quick_recording::Worker::start` → WGC source at `quick_recording.rs:59` → FFmpeg encoder | Always `CaptureTarget::Desktop`, cursor enabled, FPS capped at 60, shared audio booleans at `rune_capture.rs:188`. Backend window/region support does not make these shipped selectors. Worker limit is 600 seconds at `quick_recording.rs:160` |
| GIF/WebM | Tools converter → `rune_tools.rs:69` Request → `utilities.rs:796` format switch | Backend modes 1/2 handle WebM/GIF, but Rune mode is initialized to 0 and never changed. Current user task is blocked, see CAP-05 |
| Library | `rune_ui.rs:882` → background scanner, search and cards; star at `:1078`, trash at `:1098` → `recordings::trash` | Responsive card columns, search, filters, favorites, import, open, trash/Undo. Filter function at `:865` is current evidence despite stale migration wording saying filters remain. No next page after 500 matches |
| Video editor/export | `rune_editor.rs:144` load existing sidecar or probe → validate sources → scrub; revisions/autosave at `:232`; Export at `:739` → export worker | Preview/playback, seek, split/delete, drag trim, mute, undo/redo, autosave, MP4. Speed/gain, waveforms, multi-clip joining, presets, watch/share/fullscreen windows remain migration gaps at `RUNE_MIGRATION.md:42` |
| Upload/share | No configured upload destination flow in the inspected Rune controls | README describes sharing through configured destinations, but migration document at `:45` explicitly says configured screenshot/upload workflows are not wired. Local export plus external sharing is the supported boundary in this audit |

## E. Findings and reproducible inspection scenarios

All repro steps below are instructions for a future isolated Windows validation run using synthetic media. They were not executed here. Effort describes implementation plus relevant tests, not measured task completion time.

### CAP-01 Preserve the screenshot until delivery succeeds

- Severity/type: **S0, bug, data-loss risk**. Method: code inspection, not executed. Persona: support engineer.
- Evidence: `crates/desktop/src/rune_shot.rs:325` takes the accepted frame, `:328` closes the session before spawning delivery, `:352` saves before copying, and `:382` reports only an error after failure. `:291` clears the session.
- Repro: 1. Use an isolated capture folder whose write permission can safely be revoked. 2. Open Region, select synthetic content and annotate it. 3. Make the folder unwritable before choosing Copy or Save. 4. Attempt the action, then restore folder access and look for Retry or the original selection.
- Expected/actual: expected capture and edits remain recoverable, with Retry, Save elsewhere or Copy. Inspection shows the overlay session is cleared before I/O succeeds; save failure also prevents Copy, leaving only a notice and no retained session in this path.
- Impact: a transient storage failure can lose a unique screenshot and all annotation work.
- Fix sketch: retain the accepted frame and editing session until publication/copy reaches its intended terminal state; return structured failure with Retry/Save elsewhere. Decouple clipboard success from library write permission where the user chose Copy.
- Effort: M. Fix risk: MED, asynchronous ownership and overlay reentrancy. Confidence: HIGH for session loss and save-before-copy ordering; runtime UX not tested.

### CAP-02 Include quick screencasts in the tray recording indicator

- Severity/type: **S0, bug, privacy risk**. Method: code inspection, not executed. Persona: occasional recorder.
- Evidence: `crates/desktop/src/rune_capture.rs:204` constructs tray state only from native capture; `crates/desktop/src/rune_ui.rs:445` calls only `tray.update`; `crates/desktop/src/tray.rs:204` initializes quick recording false; its dedicated setter at `:211` is not called by compiled Rune code. `tray.rs:217` depends on that flag to present screencast recording.
- Repro: 1. Stop native replay/recording. 2. Start Quick screencast. 3. Close the main window to the tray with the preference enabled. 4. Inspect tray icon, tooltip and quick-record menu label while capture continues.
- Expected/actual: expected a visible recording state and accurate Stop action. Inspection predicts the native tray stays in its off presentation and the screencast label is not updated, while quick capture runs. Main-window status does recognize `quick.is_some` at `rune_capture.rs:322`; this does not repair minimized feedback.
- Impact: the user can believe capture is off while desktop/audio recording continues.
- Fix sketch: publish quick lifecycle state through the existing tray setter before update, including starting, paused and finishing; expose Stop/Resume with accurate labels.
- Effort: S. Fix risk: LOW, existing tray API and tests provide a narrow change. Confidence: HIGH for missing wiring; native visibility not executed.

### CAP-03 Guard unsaved image annotation work when opening or closing media

- Severity/type: **S0, bug, data-loss risk**. Method: code inspection, not executed. Persona: returning editor.
- Evidence: `crates/desktop/src/rune_editor.rs:110` guards only `self.editor.is_some()` with video save revisions; `:124` clears `annotation` when opening another media item. `:557` similarly guards only the video editor and `:568` clears annotations on close. `crates/desktop/src/rune_annotation.rs:267` and `:283` offer explicit save commands but no corresponding dirty/save revision tracking.
- Repro: 1. Open a synthetic image in Library. 2. Add a text label or shape without saving. 3. Return to Library. 4. Open another image, or close the image's editor tab. 5. Reopen the first image.
- Expected/actual: expected Save/Discard/Cancel or reliable autosave/recovery. Inspection shows opening another item or closing clears the annotation actor without an annotation dirty guard. Merely visiting Library does retain state; the loss boundary is replacement/close.
- Impact: work disappears without the protection already provided for video edits.
- Fix sketch: track annotation document revision and successful saves, and gate replacement/close on a dirty decision. Keep the actor alive if saving fails.
- Effort: M. Fix risk: MED, avoid conflating flattened image export with editable-project persistence. Confidence: HIGH.

### CAP-04 Explain that editable projects retain unredacted source pixels

- Severity/type: **S0, UX, privacy risk**. Method: serialization and control inspection, not executed. Persona: sensitive-data support engineer.
- Evidence: `crates/image-editor/src/project.rs:125` serializes `self.base` as PNG plus separate objects at `:136`; `crates/desktop/src/rune_annotation.rs:267` labels the action "Save editable…" with no privacy explanation; actor project save at `crates/toolkit/src/image_editor.rs:836` serializes the document. Flattened Accept instead renders at `:853`.
- Repro: 1. Open an image containing synthetic private text. 2. Blur/pixelate or cover that area. 3. Choose Save editable and reopen the SSXE. 4. Remove the covering object or inspect the project's base image using a safe test fixture.
- Expected/actual: expected a clear statement that editable output keeps the original and is unsuitable for sanitized sharing. Actual storage intentionally retains original pixels and editable objects; the Rune save UI supplies no such explanation. Retaining originals is correct for editing, but the privacy contract is incomplete.
- Impact: someone sharing the project as a redacted result can expose the original. This is a conditional sharing risk, not evidence that Kielbox uploads anything automatically.
- Fix sketch: distinguish "Editable project, includes original image" from "Flattened image for sharing". Provide a destructive, opaque redaction operation or guide the user to flattened output after review. Blur/pixelation should not be advertised as guaranteed removal of information.
- Effort: S for disclosure and output naming; L for a secure-redaction workflow. Fix risk: LOW for disclosure, MED for destructive editing. Confidence: HIGH for retained pixels and missing explanation; no information-recovery attack was attempted.

### CAP-05 Expose converter format selection so GIF/WebM are reachable

- Severity/type: **S1, bug, task blocked**. Method: state/write tracing, not executed. Persona: occasional recorder creating a GIF.
- Evidence: `crates/desktop/src/rune_tools.rs:39` initializes mode 0; `:73` passes it unchanged; the current options at `:540` expose Width/Height/Color/Output without a format choice. The only `.mode` read/write in this file after initialization is request construction. `crates/toolkit/src/utilities.rs:796` maps mode 0 to MP4, 1 to WebM, other to GIF; `:801` rejects mismatched output extension.
- Repro: 1. Open video converter in Tools. 2. Add a synthetic MP4. 3. Choose an output named with a .gif extension. 4. Run. 5. Look for a format selector.
- Expected/actual: expected selectable GIF and matching options, or format inference from an explicit file type. Inspection predicts "Output extension must be .mp4" and no way to change the mode in Rune.
- Impact: backend GIF capability cannot complete the advertised conversion task. Generic Width/Height labels also conceal that GIF arguments use FPS/width at `utilities.rs:802`.
- Fix sketch: add tool-specific format selection and semantic option labels, with a matching native output filter. Test each supported mode from Rune request construction to FFmpeg arguments.
- Effort: S. Fix risk: LOW, shared utility mode semantics must be preserved. Confidence: HIGH.

### CAP-06 Use one screencast lifecycle state for pause, progress and completion

- Severity/type: **S1, bug/UX, misleading state**. Method: event/state tracing, not executed. Persona: occasional recorder.
- Evidence: `crates/desktop/src/rune_capture.rs:188` starts a new worker without applying/resetting `quick_paused`; `:289` clears workers on terminal events without clearing it; `:649` toggles pause even when no worker exists. `:299` ignores Started/Progress, though `crates/desktop/src/quick_recording.rs:192` emits them. `rune_capture.rs:322` treats any worker as Recording. `quick_recording.rs:160` imposes a 600-second limit; quick UI text at `rune_capture.rs:657` only mentions FFmpeg.
- Repro: 1. Toggle Pause before starting, or pause then stop a screencast. 2. Start a new screencast. 3. Compare pause switch and actual capture state. 4. Stop and inspect the global state during finalization. 5. Separately validate the ten-minute boundary on the VM if authorized.
- Expected/actual: expected a fresh session starts with truthful pause state, startup/finalization are distinct, and the duration limit is discoverable. Inspection shows a checked pause switch can coexist with a newly unpaused worker; progress/start events are discarded and worker presence determines the status. Stop can leave "Recording" presentation during finalization.
- Impact: recording unwanted footage, missing intended footage through confusing pause actions, and unexpected early completion. No ten-minute runtime result is asserted.
- Fix sketch: model Idle/Starting/Recording/Paused/Finishing/Failed explicitly; drive UI/tray from events and reset session state on retirement. Disable pause when unavailable and disclose the cap before starting.
- Effort: M. Fix risk: MED, coordinate hotkey/button/tray paths. Confidence: HIGH for state mismatch; MED for perceived confusion pending Windows validation.

### CAP-07 Disclose Copy's persistent library output

- Severity/type: **S0, UX, privacy expectation risk**. Method: action tracing, not executed. Persona: support engineer working with sensitive material.
- Evidence: `crates/desktop/src/rune_shot.rs:341` documents and `:352` implements save-before-copy; clipboard result at `:358` says "copied and saved" only after the action. The session is closed at `:328` before delivery.
- Repro: 1. Open Region on synthetic sensitive content. 2. Choose Copy. 3. Inspect the capture folder and Library. 4. Assess whether the pre-action label made persistence clear.
- Expected/actual: expected an explicit "Copy and save" label or a user-controlled persistence preference. Actual Copy always writes a PNG before clipboard publication. This is a UX privacy contract issue, not proof of accidental network disclosure.
- Impact: a user expecting transient clipboard output leaves durable material in the configured folder, which may itself be synced by another service. Sync configuration was not inspected.
- Fix sketch: name the action accurately or offer independent Copy and Save behaviors. Explain retained output before the user acts, with a discoverable delete/recovery policy.
- Effort: S. Fix risk: LOW for accurate labeling; MED for changing established persistence behavior. Confidence: HIGH for storage behavior, MED for expectation mismatch.

### CAP-08 Show and navigate results beyond the first 500 matches

- Severity/type: **S2, UX, incomplete browsing**. Method: loop/control inspection, not executed. Persona: returning editor with a large archive.
- Evidence: `crates/desktop/src/rune_ui.rs:925` iterates files, `:926` breaks after 500 matching items, `:933` applies search, and `:992` returns the page without a next-page/load-more control. `docs/RUNE_MIGRATION.md:38` acknowledges this gap.
- Repro: 1. Point an isolated library at more than 500 supported test files. 2. Browse All to its end. 3. Try reaching an older omitted item without knowing its title. 4. Search for a known omitted filename to distinguish truncation from loss.
- Expected/actual: expected total/matching count and navigation or a clear limit. Actual inspection shows silent truncation. Search filters before incrementing the match count, so a precise query can still reveal older files; they are not proven deleted or unindexed.
- Impact: browsing cannot reach the full archive and may look like missing captures.
- Fix sketch: add bounded pagination or Load more with a visible count. Preserve search/filter ordering and scanner cancellation.
- Effort: M. Fix risk: MED, avoid materializing unbounded thumbnails/cards. Confidence: HIGH.

## F. Privacy, overwrite and recovery assessment

Positive evidence matters here. Flattened image writes use a sibling tempfile, sync and `persist_noclobber` at `crates/toolkit/src/utilities.rs:687`; an existing source is not silently replaced. Native file dialogs may still offer an OS replacement confirmation, but this path rejects the existing target afterward. That mismatch needs Windows observation before calling it a confirmed dialog defect.

Quick screencasts stage in the capture folder at `quick_recording.rs:153` and publish with `persist_noclobber` at `:213`. An early capture failure can save a playable prefix with a warning at `:217`. Worker Drop requests normal stop and joins at `:130`, rather than discarding by default. Export rejects an existing destination at `crates/editor-core/src/export.rs:847` and publishes through the no-clobber helper at `:642`. These are source-level safeguards, not proof of power-loss durability or playable results on every failure.

The reachable editable annotation path is atomic: `toolkit/src/image_editor.rs:840` deliberately bypasses the upstream direct-save convenience function and uses `settings::atomic_write`. `crates/workflows/src/settings/io.rs:43` creates its owned temporary file exclusively, syncs at `:46`, then replaces at `:48`. Do not report `image-editor/src/project.rs:225` as a shipped Rune non-atomic-save defect. Atomic replacement still permits intentional overwrite of the chosen project path; native confirmation behavior and a file-created-after-dialog race remain untested.

Library trash validates source containment at `recordings.rs:416`, journals original paths before moves at `:440`, and uses no-clobber moves at `:456`. Restore refuses an existing original path at `:471`. Rune only exposes session Undo; persistent trash recovery UI is explicitly dormant at `RUNE_MIGRATION.md:38`. The underlying files/journal remain, so this is recoverability/discoverability debt, not demonstrated deletion.

Privacy review found no configured upload call on these Rune paths. It did find persistent Copy output, editable originals, desktop-only quick capture and incomplete minimized indicators. Saved native display/audio identities are honored, but selectors are absent. Quick screencast uses `default_audio` with booleans at `quick_recording.rs:64`, whereas native recording applies endpoint IDs at `rune_capture.rs:94` and `:99`. Users should see the actual source policy per mode; do not assume endpoint preferences apply identically to both.

Screen/window screenshots save the unannotated capture immediately at `rune_tools.rs:999`. Later redaction in the editor creates a new flattened output and does not sanitize that original automatically. Region annotation before first save gives the safer initial-output path. A privacy review must inspect all retained artifacts, not only the final PNG.

## G. Module heuristic assessment

| Module | Strength with evidence | Main heuristic gap | Validation still needed |
| --- | --- | --- | --- |
| Screenshot/annotation | Region action remains connected to rendering and output through one actor; tool labels and toolbar role at `rune_annotation.rs:302` | Error recovery, unsaved-work protection, secure-output distinction; all-tool horizontal toolbar and fixed label field at `:335` need usability review | Multi-monitor negative coordinates, mixed DPI, region resize, crop accuracy, undo/redo, keyboard-only tools, text composition and opaque redaction pixels |
| Screencast/GIF | Staging/no-clobber and playable-prefix warning | State truthfulness, actual target/audio disclosure, format discoverability; CAP-02/05/06 | FFmpeg missing, first-frame failure, audio denied, pause/resume, stop during start/finalize, ten-minute cap, disk-full, unplugged output |
| Replay/native recording | Explicit worker ownership, stop flag and retirement at `rune_capture.rs:248`; preview errors isolated at `:262`; pending saves tracked | Missing selectors/recovery presentation; generic notices can obscure the operation they describe | Save before buffer readiness, repeated saves, save near stop, lock/sleep/display loss, device removal, microphone cleanup, encoder errors, GPU-preview failure while recording |
| Clip library/edit/share | Async scan, favorites, reversible trash, video autosave/retry at `rune_editor.rs:1031`, source validation at `:152` | Older-result navigation, annotation parity, missing configured share/watch workflows | Corrupt/offline source recovery, sidecar permission failure, export cancel, existing destination, audio sync, trim boundaries, restart after trash, imported files outside capture root |

Heuristic judgments are based on visibility of state, error prevention/recovery, user control, consistency, recognition and accessibility semantics. They are not numeric usability ratings. S3 visual-polish issues cannot be established responsibly without rendered/runtime evidence.

## H. Competitive feature assessment

Official documentation reviewed on audit date; no competitors were run. These are reference capabilities and grounded product options, not measured superiority or a mandate to copy every feature.

- [ShareX image editor](https://getsharex.com/docs/image-editor) documents an integrated capture/edit/copy/save/upload workflow and private-information tools. Kielbox's region annotation path is a good foundation, but configured destinations and detailed styles are explicitly unwired. Restore a coherent output/recovery contract before adding more annotation tool icons.
- [Microsoft Snipping Tool](https://support.microsoft.com/en-us/windows/apps/use-snipping-tool-to-capture-screenshots) documents screen recording, text actions and quick redaction of email addresses/phone numbers, plus Clipchamp handoff. Kielbox's editable project and native replay provide different value; safe flattened output and a clear recording target should be baseline expectations. No OCR accuracy or redaction security comparison was performed.
- [OBS recording output guide](https://obsproject.com/kb/standard-recording-output-guide) recommends resilient recording choices such as MKV and documents remuxing for sharing. Kielbox already distinguishes native MKV from quick MP4 and editor export. Explain those choices and recovery stages in Rune before pursuing streaming/scenes parity.

Grounded S4 opportunities: restore explicit target/audio selectors using existing backend identities; add a privacy-focused flattened-output review; restore configured sharing only with a reviewable destination and explicit upload action. Each is supported by current backend capability and migration gaps at `RUNE_MIGRATION.md:39` through `:47`. Game-event highlights, cloud libraries and collaboration are not prerequisites for making these workflows dependable.

## I. Verification handoff and acceptance gates

Use the boat VM agent for any build/test execution. Documented gates, not run here: `cargo build --locked -p desktop -j1`, `cargo test --locked -p desktop -j1`, `./scripts/test-package-windows.ps1`, and `./scripts/verify-rune.ps1 -SkipBuild -GpuAdapter NVIDIA`, from `docs/RUNE_MIGRATION.md:70`. Headless scene tests do not capture personal screens or validate live audio/capture behavior; the document states that boundary at `:83` onward.

For focused fixes, add deterministic adapter tests before live VM acceptance: quick lifecycle → tray presentation; pause state across sessions; image dirty guard on close/open; delivery failure retains accepted frame; converter request mode/output extension; library navigation past 500 matches. Existing worker tests and filesystem no-clobber tests should remain intact. Do not add tests that only duplicate labels without checking behavior.

Run future native acceptance only with synthetic, non-sensitive content and an isolated capture folder. Record binary hash, Rune revision, Windows version, selected display/audio identities, prerequisites, action sequence, observable indicator and final media evidence. Repro evidence should pair the control activation with the resulting file or explicit failure. A screenshot of a recording button does not prove a recording exists; a generated render does not prove live capture.

Critical matrix: normal start/pause/resume/stop; double actions and global-hotkey overlap; stop while starting; save replay immediately before stop; failure to write; existing output preserved byte-for-byte; denied/missing audio; display/device loss; close-to-tray and Quit; restart recovery; sensitive annotation flattened versus editable. Use a sentinel destination file to verify overwrite protection without touching personal files.

## J. Priorities and dependencies

1. Wire truthful quick recording tray state, then cover minimized/paused/finishing states in native acceptance. CAP-02 is narrow and high-confidence.
2. Preserve region output on failed delivery and guard dirty annotation replacement/close. CAP-01 and CAP-03 prevent lost work and require explicit state ownership.
3. Clarify persistent Copy and original-preserving editable output. CAP-04/07 can begin with precise labels and explanations; secure destructive redaction needs separate design and pixel tests.
4. Normalize quick lifecycle state and disclose target/audio/limit. CAP-06 should share the state contract introduced for CAP-02.
5. Expose actual converter formats, then restore bounded archive navigation. CAP-05/08 have independent acceptance tests.

Do not prioritize visual polish or new game integrations above these contracts. Current architectural safeguards are worth keeping; most gaps are at the Rune adapter and state-presentation layer.

## K. Rejections, strengths and untested areas

Considered and rejected: old GPUI watch/share, picker, workflow and recording-controller code as proof of shipping; direct upstream project save as proof of Rune truncation; timestamps as proof of no-overwrite safety; a 500-card limit as proof that older files were deleted; retained editable pixels as a serializer bug; existing historical metrics as results of this audit; absence of a current share button as evidence of automatic uploading. Current filter controls supersede the migration document's broad remaining-filter wording.

Strong foundations: no-clobber flattened outputs and exports, same-folder quick staging, source validation before video editing, asynchronous workers with normal-stop Drop behavior, video autosave/retry, journaled trash and isolated preview errors. All have source evidence above; none was revalidated live here.

Untested: Windows rendering and interaction, real screenshot content/coordinates, GIF pixels and loop behavior, annotation secure-redaction properties, IME/clipboard/AT, NVIDIA/FFmpeg paths, audio isolation/sync, output playability, replay duration, transient capture recovery, crash/power-loss durability, installer acceptance, network destination behavior outside Rune paths, performance and resource budgets. No security dependency audit, full backend audit or other desktop module audit was performed. This report supports a focused senior QA synthesis, not a blanket product sign-off.
