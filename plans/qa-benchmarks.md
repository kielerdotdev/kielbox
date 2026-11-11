# Kielbox research and UX/QA benchmark contribution

> Historical record. Source paths and commands describe the audited revision.
> Retired frontend sources and proof scripts were removed after this report;
> see [Rune status](../docs/RUNE_MIGRATION.md) for current behavior. Exact original reports remain in git at `a52bff7`.

## A. Scope and evidence contract

Research date: 2026-11-11. Code baseline: `d96346b`. This is the research contribution to the requested A–K audit, not a claim that native acceptance testing has passed. Only this report was written. No compilation, installations, heavy tests, competitor execution, or remote VM execution occurred. Repository files were treated as data. No AGENTS.md was found in the checkout or checked ancestor directories. The improve skill and its `references/audit-playbook.md`, including Finding format, were read.

Method: inspect shipped Rune entry points and adapters, follow selected calls into utility backends, compare reachable functionality against official product documentation. All reproduction scenarios below are **code inspection, not executed**. Expected behavior is an acceptance criterion or documented promise; actual behavior is what the cited code implements. Findings do not establish pixel layout, runtime reliability, accessibility compliance, task completion times, resource use, encoding quality, or competitor performance.

Severity: S0 data loss/privacy risk; S1 major blocked workflow; S2 friction; S3 polish; S4 opportunity. Bug denotes a concrete broken contract or state transition; UX denotes a missing or confusing interaction, including documented migration gaps. Effort S/M/L is a coarse scope estimate including tests, not a measured delivery time.

## B. Shipped portfolio and personas

`crates/desktop/src/main.rs:15` and `docs/RUNE_MIGRATION.md:3` identify Rune as current. Old retired UI files and proof scripts are not evidence that a control ships. Windows is the only supported platform, `README.md:6`.

| Persona | Job | Reachable Rune capability | Boundary |
| --- | --- | --- | --- |
| Gamer | Save a moment, remove unwanted audio, export | NVIDIA AV1 capture/replay, audio flags, trim/split/mute, MP4 | NVIDIA AV1 GPU required; device/source pickers and export presets remain absent |
| Support engineer | Capture a problem, annotate, provide evidence | Desktop/window screenshot; region annotation/copy/save/edit | Configured upload workflow is not wired |
| Tutorial author | Record one task without unrelated desktop content | Desktop screencast, pause and finish | Quick recording target is hardcoded Desktop |
| Developer/designer | OCR, QR, color conversion, image/file transformations | Seventeen utility backend choices | Generic form does not expose many choices |
| Storage maintainer | Understand growth and locate files | Cancellable disk scan, directory navigation, logical size bars | No full file-type analysis or current equivalent of old disk explorer |
| Existing RCR user | Reuse recordings and settings safely | Existing formats and saved endpoint identities retained | Welcome migration UI and persistent trash recovery remain migration gaps |

Evidence for inventory and boundaries: `docs/RUNE_MIGRATION.md:19`, `:35`; quick target `crates/desktop/src/rune_capture.rs:188`; utility inventory `crates/desktop/src/rune_tools.rs:233`.

## C. Official current documentation comparison

Sources were accessed during this audit. These are documented capabilities, not hands-on competitor results. Current web pages can change independently of installed versions. No product is ranked by speed, ease, or hardware consumption. Vendor numerical performance advertising is deliberately excluded.

| Reference | Officially documented workflow | Kielbox comparison and useful benchmark question |
| --- | --- | --- |
| [ShareX](https://getsharex.com/) | Capture, recording, custom uploaders and configurable after-upload actions | Screenshot/annotation foundations exist, but Rune does not wire configured screenshot/upload workflows, `docs/RUNE_MIGRATION.md:45`. Can a user complete capture → redact → review destination → share without leaving context? |
| [ShareX directory indexer](https://getsharex.com/docs/index-folder) | Folder selection, depth/options, text/HTML/XML/JSON, preview then save/upload; warns that paths can expose private information | Backend indexing exists, but Rune offers a file picker and no format selector. Compare task completion and privacy review, not just output-format counts. |
| [OBS recording guide](https://obsproject.com/kb/standard-recording-output-guide) and [overview](https://obsproject.com/kb/obs-studio-overview) | Recording path/format, track selection, remuxing; replay and hotkeys; source/device configuration | Kielbox has replay and separate audio, but current pickers are missing. Benchmark source identity, track audibility and recovery rather than claiming OBS parity. |
| [ScreenToGif official repository](https://github.com/NickeManarin/ScreenToGif) | Selected-area recording, editing, GIF/video output | Kielbox quick recording currently captures Desktop. A GIF conversion backend does not establish a cohesive selected-region-to-GIF workflow. The former screentogif.com address redirected to N-Tech/N-Studio during research; the official repository supports only the narrow claim made here. |
| [NVIDIA App AV1 documentation](https://www.nvidia.com/en-us/geforce/news/nvidia-app-beta-update-av1-performance-tuning/) and [current release highlights](https://www.nvidia.com/en-us/software/nvidia-app/release-highlights/) | Overlay recording, instant replay length, desktop capture and codec controls; current releases continue to evolve recording options | Kielbox shares replay/AV1 use cases, but not demonstrated encoder/settings parity. Do not use older GeForce Experience-era claims that ShadowPlay lacks AV1. Supported GPU and driver combinations must be stated for both products before any future comparison. |
| [Medal Windows platform support](https://support.medal.tv/support/solutions/articles/48001271455-platform-support-on-medal-tv) | Windows combines capture, editing, sharing and social functions | Kielbox has local capture/editing; its old watch/share windows are not current Rune capabilities. Medal's social features are a different product scope, not mandatory parity. |
| [Medal montages](https://support.medal.tv/support/solutions/articles/48001185258/) and [library filters](https://support.medal.tv/support/solutions/articles/48001158191) | Add library clips, arrange and trim; distinguish on-device, unlisted, public and cloud items | Rune lacks multi-clip joining and current watch windows; library has a 500-match render boundary. Visibility and storage state should be explicit if sharing returns. |
| [WizTree](https://diskanalyzer.com/) and [FAQ](https://diskanalyzer.com/faq) | Visual file-size analysis, NTFS MFT reading, hard-link-aware allocated accounting | Kielbox has an NTFS backend, but logical sizes count hard links per path, `docs/UTILITIES.md:18`. Equal-looking totals are not a valid correctness oracle. Compare same accounting definition, root, privileges and exclusions. No scan-speed assertion is made. |
| [PowerToys Text Extractor](https://learn.microsoft.com/windows/powertoys/text-extractor) | Shortcut → adjustable region → OCR → clipboard; installed OCR language packs required | Kielbox OCR backend exists; the utility region button does not hand its capture to the OCR input. Compare the complete transition, language guidance and cancellation. |
| [PowerToys Color Picker](https://learn.microsoft.com/en-us/windows/powertoys/color-picker) | Screen sampling, zoom, color history, configurable clipboard format; documented HDR/WCG limitations | Rune Color picker parses typed colors. A typed color converter should not imply screen sampling. Do not assume either product is accurate on HDR without testing. |
| [PowerToys utility catalog](https://learn.microsoft.com/en-us/windows/powertoys) | Screen Ruler, Always On Top, Image Resizer and other focused tools | Rune's current 17-tool inventory should be compared individually; old screen ruler and pin backends do not imply current UI availability. |
| [Windows Snipping Tool protocol](https://learn.microsoft.com/en-us/windows/apps/develop/launch/launch-snipping-tool) | Rectangle/freeform/window images and rectangle video with audio; runtime capability discovery | Relevant baseline for casual screenshots/screencasts. Documentation describes integration contracts, not proof that every Windows installation exposes identical UI. |
| [Shotcut features](https://shotcut.org/features/) | Native timeline editing, frame seeking and broad media support | Relevant to editor expectations. Kielbox's trim/split/mute scope should be clear before users expect a full nonlinear editor. |

## D. Vetted major findings

### [BENCH-01] Expose utility-specific options

- Severity/type: **S1, UX**. Personas: designer, tutorial author, developer.
- Method: code inspection, not executed. Repro: 1. Open Tools → Effects and add an image. 2. Try to select blur instead of grayscale. 3. Open Combine and try horizontal layout. 4. Open video conversion and try to select GIF.
- Expected: named controls expose the supported choices and use relevant labels and defaults. Actual: `mode` starts at zero and is forwarded unchanged; the shared form exposes Width, Height and Color, with no mode control. Effects defaults to grayscale; orientation, format and other mode-dependent choices are inaccessible through that form.
- Evidence: `crates/desktop/src/rune_tools.rs:39`, `:69`, `:534`; backend mode branching `crates/toolkit/src/utilities.rs:298`, `:342`. Documented pending option forms `docs/RUNE_MIGRATION.md:48`.
- Impact: users cannot reach important existing backend functions; labels also misdescribe padding, grid counts and other numeric parameters.
- Fix sketch: small per-tool form definitions with named choices, validation and defaults, mapped explicitly to existing Request fields. Verify each advertised mode through Rune.
- Effort M; risk MED, request mappings must remain compatible; confidence HIGH. This is acknowledged migration work, not a newly introduced regression claim.

### [BENCH-02] Hand utility captures back to the requesting tool

- Severity/type: **S1, bug**. Personas: support engineer, developer extracting text/QR.
- Method: code inspection, not executed. Repro: 1. Open Text from image with no input files. 2. Click Capture region. 3. Select a synthetic text region and Save or Copy. 4. Return and click Run.
- Expected: captured image becomes the OCR input and the current task continues. Actual: button invokes the ordinary screenshot path; delivery saves/copies/opens a library image without adding it to `tools.files`. OCR still requires the first file and should report missing input.
- Evidence: `crates/desktop/src/rune_tools.rs:458`, `:949`; `crates/desktop/src/rune_shot.rs:370`; `crates/toolkit/src/utilities.rs:252`, `:399`.
- Impact: prominent input action fails to supply input. Manual save and re-add is required.
- Fix sketch: capture completion carries an explicit requesting-tool context and attaches the result only to that request. Preserve cancel behavior and prevent late capture delivery into another tool.
- Effort M; risk MED, asynchronous routing and privacy; confidence HIGH.

### [BENCH-03] Use directory dialogs for directory contracts

- Severity/type: **S1, bug**. Persona: developer.
- Method: code inspection, not executed. Repro: 1. Open Split with a small image. 2. Set Width/Height to 2/2. 3. Choose output using the provided dialog. 4. Run. Separately open Folder index and try selecting a directory.
- Expected: Split selects an existing output directory; index selects an input directory. Actual: shared output uses `save_file`, while Split requires `root.is_dir()`; input uses `open_file`, while Index consumes its first file slot as the root. The generic text field is not the index root.
- Evidence: `crates/desktop/src/rune_tools.rs:443`, `:570`; `crates/toolkit/src/utilities.rs:252`, `:361`, `:403`.
- Impact: normal dialog selection does not satisfy backend contracts. Exact Windows dialog behavior needs execution; manual path tricks are not an acceptable normal workflow.
- Fix sketch: choose folder for these contracts, show selected directory, label Split inputs Columns/Rows, validate before Run.
- Effort S; risk LOW, localized UI adapters; confidence HIGH on contract mismatch, MED on platform-specific dialog outcome.

### [BENCH-04] Bind utility completion to its originating request

- Severity/type: **S2, bug**. Persona: user switching between utility tasks.
- Method: code inspection, not executed. Repro: 1. Start a sufficiently large folder index or video conversion. 2. Return to Tools before completion. 3. Open a different tool. 4. Wait for the first job to finish.
- Expected: completion remains associated with its originating tool, or switching is explicitly constrained. Actual: `open_tool` changes the selected tool and clears result/message, but does not cancel or segregate the existing job; `poll` assigns completion into the same result slot.
- Evidence: `crates/desktop/src/rune_tools.rs:321`, `:99`, `:123`.
- Impact: one utility can display another utility's result. This is a task ownership problem; no wrong-file mutation was established.
- Fix sketch: attach tool/request identity to jobs and results; either retain separate sessions or show the active job in its originating tab.
- Effort M; risk MED, navigation and cancellation behavior; confidence HIGH.

### [BENCH-05] Clear or label stale success when another run fails

- Severity/type: **S2, bug**. Persona: user verifying conversion or OCR.
- Method: code inspection, not executed. Repro: 1. Run an image utility successfully. 2. Replace input with an unsupported file. 3. Run again and wait for failure.
- Expected: failed attempt shows failure status, with any previous result explicitly identified. Actual: starting a run does not clear `result`; error completion updates only `message`; status renders Done whenever result remains Some, and old image/save action remains available.
- Evidence: `crates/desktop/src/rune_tools.rs:53`, `:125`, `:585`, `:658`.
- Impact: user may save an old result believing the new request succeeded.
- Fix sketch: represent running/success/error with request identity; remove stale success badge and label or clear previous output.
- Effort S; risk LOW; confidence HIGH.

## E. Migration and documentation friction

### [BENCH-06] Put current Rune boundaries beside product promises

- Severity/type: **S2, UX**. Personas: migrating RCR user, support engineer.
- Method: documentation/code inspection, not executed. Repro: 1. Follow README's welcome-screen migration instruction. 2. Follow Utilities instructions to choose File View, Reveal or utility Copy/Edit/Pin. 3. Look for those actions in current Rune.
- Expected: current-build instructions name available controls or explicitly label legacy behavior. Actual: README describes welcome migration controls still listed as pending; Utilities describes richer old disk/result controls, while Rune utility result actions provide Run/Cancel and conditional Save image.
- Evidence: `README.md:25`; `docs/UTILITIES.md:12`, `:28`; `docs/RUNE_MIGRATION.md:45`, `:50`; `crates/desktop/src/rune_tools.rs:643`.
- Impact: users troubleshoot unavailable controls and overestimate replacement readiness. The migration warning at README's end helps, but does not correct the earlier concrete instructions.
- Fix sketch: add current/legacy labels at the instructions and a task-based supported-build matrix. Keep backend preservation separate from frontend availability.
- Effort S; risk LOW; confidence HIGH.

### [BENCH-07] Make disk result truncation explicit and navigable

- Severity/type: **S2, UX**. Persona: storage maintainer.
- Method: code inspection, not executed. Repro: 1. Scan an owned folder with 501 immediate children. 2. Inspect Largest. 3. Attempt to inspect the final child or act on a file row.
- Expected: visible total/count boundary, pagination or search, and actionable selected file path. Actual: list takes the first 500 children, chart shows eight plus aggregated remainder; file-row press only acts when the node is a directory. No further-page or file reveal action appears in this view.
- Evidence: `crates/desktop/src/rune_tools.rs:816`, `:880`, `:897`, `:930`.
- Impact: scanning and visualizing are possible, but the final step of locating an actionable file is incomplete. This does not mean scanner data was lost.
- Fix sketch: disclose shown/total, provide searchable or virtualized full results and Reveal/Copy path. Retain deliberate read-only analysis.
- Effort M; risk MED, large-tree presentation; confidence HIGH.

## F. Strengths supported by code

- Utility file publication is defensive. Image saving uses no-clobber publication, `crates/toolkit/src/utilities.rs:690`; split uses a temporary directory and checks cancellation before keeping it, `:363`, `:387`. These are concrete protections, not proof of every failure path.
- Rune region Copy reports both saved output and clipboard failure, rather than hiding a partial success, `crates/desktop/src/rune_shot.rs:352`.
- Disk scans run off the UI thread, cancellation is available, and cancelled results are labelled, `crates/desktop/src/rune_tools.rs:163`, `:199`, `:752`.
- Empty disk state explicitly explains logical sizes and skipped links/offline files, `crates/desktop/src/rune_tools.rs:779`. Keep this explanation visible after results appear.
- Library scanning/thumbnails update asynchronously, `crates/desktop/src/rune_ui.rs:325`. Search is applied before counting displayed matching items, `:929`. The 500 boundary is a rendering/navigation limit, not evidence that only 500 files were scanned or searchable.
- Migration documentation explicitly disclaims parity and distinguishes generated GPU proof from live capture, `docs/RUNE_MIGRATION.md:6`, `:80`. Do not turn historical proof counts into fresh audit results.

## G. Portfolio recommendation

The strongest identity is a local capture workspace for people who record, annotate and manage their own files. Build complete workflows before expanding utility count. ShareX sets a useful expectation for configurable capture handoffs; Medal for library-to-edit continuity; PowerToys for focused entry points; OBS for source/audio transparency. None establishes that Kielbox must copy every feature.

Prioritize BENCH-03 and BENCH-05 as bounded correctness work, then BENCH-01/02/04 as a coordinated utility interaction pass. Correct docs immediately. Source/endpoint pickers and quick region recording are next product choices, not silently shipped capabilities. Avoid broad claims that Kielbox replaces OBS, ShareX, Medal or WizTree today.

## H. Cross-tool opportunities

These are **S4 opportunities, UX**, not defects. Method for all is code inspection, not executed. Coarse effort M/L; confidence reflects code grounding, not market demand.

1. **Capture → OCR/QR/annotation handoff.** Persona: support engineer. Scenario: select a synthetic screen region, choose Extract text, review, copy or save. Expected future flow: reuse the existing captured pixels; current flow: ordinary screenshot delivery does not route into utility input. Evidence `crates/desktop/src/rune_tools.rs:458`, `crates/toolkit/src/utilities.rs:399`, `crates/desktop/src/rune_shot.rs:342`. Fix sketch: explicit local handoff contract with no automatic upload. Effort M; risk MED, result ownership; confidence HIGH. Depends on BENCH-02/04.
2. **Recording → trim → share review.** Persona: gamer. Scenario: save replay, mute unwanted audio, trim, export, then review destination and visibility. Expected future flow: preserve project and track intent across steps; current state: editing exists but configured upload/watch/share UI is pending. Evidence `docs/RUNE_MIGRATION.md:24`, `:42`, `:45`; `README.md:8`. Fix sketch: implement one destination-reviewed export handoff before broad workflow configuration. Effort L; risk HIGH, privacy and destructive export expectations; confidence MED. No claim that current UI leaks content.
3. **Library storage explanation → disk analysis.** Persona: storage maintainer. Scenario: open a recording folder from Library, inspect its share of logical space, reveal a large capture without deleting it. Current state: library and disk share filesystem paths but are separate task flows. Evidence `crates/desktop/src/rune_ui.rs:919`, `crates/desktop/src/rune_tools.rs:797`. Fix sketch: contextual Analyze folder and Reveal actions; explicitly separate logical size from allocated capacity. Effort M; risk LOW if read-only; confidence MED. Do not connect scan results directly to the clean CLI's apply action without a separately reviewed preview and recovery design.

## I. Native acceptance protocol for the synthesis owner

All following work remains unexecuted. Run only with owned synthetic data on the authorized Windows/boat agent; only that agent may compile remotely.

| Journey | Fixture and checks | Evidence to retain |
| --- | --- | --- |
| Utility handoffs | Synthetic text/QR image; capture/save/copy/cancel; switch utility while work runs | Rune element tree, capture/result identity, input list, error/success state |
| Utility contracts | Small image; 2×2 split; existing directory; index with known hidden/link entries | Selected dialog type, output manifest, cancellation cleanup and no-clobber outcome |
| Capture/audio | Known synthetic desktop/window, named endpoints; disconnect/reconnect; save replay; pause/finish | Selected source/device identity, produced file metadata, audible track checks and finalization errors |
| Editing | Owned two-track fixture; seek/split/trim/mute; undo/redo; close/reopen | Project state and export content verification, original preservation |
| Disk | Known tree with >500 children, hard links, junction, denied folder and cancellation | Expected logical totals/exclusions, errors, shown/total disclosure and path actions |
| Migration/recovery | Disposable existing data root; trash then dismiss/restart | Original location, recovery affordances and preserved files; never use personal library |
| Accessibility | Keyboard-only navigation, screen reader, 100/150/200% scaling, narrow window, IME | Focus order, named controls, status announcement, clipping and actual text-entry behavior |

Do not infer live capture or accessibility acceptance from generated Rune scenes. Use file metadata and observable content as correctness evidence. Future timed experiments would require a separately specified protocol and matched machines/settings; none were performed here.

## J. Untested areas and rejected overclaims

Not audited exhaustively: updater authenticity/recovery, installer migration behavior, clean CLI deletion policy, upload credential storage, full media parser/export safety, annotation redaction irreversibility, native capture/GPU/driver reliability, HDR/color accuracy, mixed DPI, IME, screen-reader output, memory/CPU/GPU use, crash recovery and all external destination policies. No S0 is established by this research pass. That is not clearance of these areas.

Rejected: calling retained retired UI screens shipped; treating all documented parity gaps as bugs; claiming the disk analyzer deletes files; claiming library items beyond 500 cannot be found by search; asserting equal allocated and logical disk totals; using NVIDIA vendor compression or scan-speed advertising as measured findings; presenting old test counts as fresh execution. Screenshot monitor-under-pointer behavior is implemented in `crates/desktop/src/rune_tools.rs:974` despite the broader pending wording in migration docs, so it is not reported missing.

## K. Handoff and verification boundaries

This report contains seven concrete findings and three portfolio opportunities. Exact native outcomes require the acceptance work above. No source edits, commits, secret values, local Rust compilation, competitor runs or timed benchmark claims were made.

For later implementation, the repository documents `cargo build --locked -p desktop -j1`, `cargo test --locked -p desktop -j1`, `./scripts/test-package-windows.ps1`, and `./scripts/verify-rune.ps1 -SkipBuild -GpuAdapter NVIDIA` in `docs/RUNE_MIGRATION.md:70`. They are proposed gates for the authorized remote Windows agent, not commands executed here. Add targeted Rune interaction coverage for each repaired contract before broadening checks. Historical old-retired UI verification flags are not accepted Rune evidence.
