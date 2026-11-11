# Part A UX/QA research audit

> Historical record. Source paths and commands describe the audited revision.
> Retired frontend sources and proof scripts were removed after this report;
> see [Rune status](../docs/RUNE_MIGRATION.md) for current behavior. Exact original reports remain in git at `a52bff7`.

Reviewed 2026-11-11 at commit `d96346b`. This report feeds the requested A–K synthesis; it covers Part A, A1–A12 only. It is not a completed A–K audit.

## Scope and method

Read-only source and documentation inspection. Every reproduction below is a proposed Windows acceptance scenario, **not executed**. No app, installer, Rust build, performance benchmark, screen reader, competitor or remote machine was operated. No task durations, hardware measurements or ease ratings were collected. Ratings below are provisional reviewer judgments about evidenced implementation coverage, not user study results.

Read `/home/user/.agents/skills/improve/SKILL.md`, its `references/audit-playbook.md`, including Finding format, and the unslop writing skill. No AGENTS.md was found in the workspace or checked ancestor paths. Repository material was treated as data. No secret values were reproduced. Only this report was written.

The shipped Windows frontend is Rune: `crates/desktop/src/main.rs:15`–`:30` declares the Rune modules, `:127`–`:162` constructs the Rune app, and `crates/desktop/Cargo.toml:19` pins Rune with accessibility support. `docs/RUNE_MIGRATION.md:9`–`:13` identifies old retired UI views/proofs as uncompiled references. In particular, do not use `studio_ui.rs`, `history_ui.rs`, `trash_recovery_ui.rs`, old editor views, or vendored retired UI as evidence of shipped UX. Main rejects obsolete proof flags at `main.rs:57`–`:68`. Historical `docs/UX.md` retired UI descriptions and November 5 validation do not establish Rune parity.

Personas used: P1 first-time Windows user; P2 returning RCR user with existing data; P3 frequent recorder using global shortcuts and tray; P4 creator editing and annotating captures; P5 privacy-conscious user choosing exactly what is recorded; P6 keyboard-only or screen-reader user; P7 user with a large library or slow/unavailable storage.

Severity: S0 data-loss/privacy risk, S1 major task failure, S2 friction, S3 polish, S4 opportunity. No confirmed S0 is established by this inspection. Bug means code contradicts its apparent interaction contract. UX means a missing or confusing interaction, including acknowledged migration scope. Fix effort is S hours, M roughly a day, L multiple days, including verification. These are planning estimates.

## A1. Installer and onboarding

### [A1-01] Restore the promised existing-data onboarding

- **Severity/type/method/personas:** S1, UX and documentation defect; code/document inspection, not executed; P1, P2.
- **Evidence:** `README.md:25` tells returning users to choose Use existing data on a welcome screen. `rune_ui.rs:139` starts directly in Library. `main.rs:98` initializes installation but discards the returned installation state. `docs/RUNE_MIGRATION.md:50` explicitly lists welcome/data-folder migration controls as remaining work. Paths without a crate prefix in this report mean `crates/desktop/src/`.
- **Repro:** On a Windows test account with an existing RCR data folder, install the preview; launch; follow README's Use existing data instruction.
- **Expected/actual:** A discoverable first-run data-root chooser preserving the existing files. Source shows direct Library startup with no welcome chooser; the documented route is unavailable.
- **Impact:** Returning users can interpret an empty new workspace as missing recordings and cannot follow the migration instructions. This is not evidence that old files were deleted.
- **Fix sketch:** Wire the existing installation selection API into a welcome/restart flow and expose the active data root. Correct README until that ships. Keep data-root selection distinct from Capture folder.
- **Effort/risk/confidence:** M; MED, changing startup roots affects all relative data; HIGH.

Strength: `installation.rs:156` refuses to overwrite unreadable bootstrap state, `:163` explains unavailable saved roots, and `:176` rejects application-directory storage. Installer execution, unsigned-build prompts, uninstall retention, rollback, and clean-machine dependencies remain untested.

## A2. Information architecture

### [A2-01] Route tray destinations to the named pages

- **Severity/type/method/personas:** S1 bug; code inspection, not executed; P3.
- **Evidence:** `tray.rs:150`–`:151` emits `library` and `settings`. `rune_ui.rs:418`–`:441` handles neither and sends unmatched events to Tools. Only `open` and `show` call the window handle's open method at `:420`–`:423`.
- **Repro:** Close the window to tray; choose Library; repeat with Settings.
- **Expected/actual:** Restore the main window on the selected page. The inspected dispatcher changes the page to Tools, without the explicit window restore used by Open.
- **Impact:** Primary tray navigation fails, especially for users who keep capture running with the window closed.
- **Fix sketch:** Handle each destination explicitly, restore the window, and test both destination and restoration. Avoid a catch-all that silently turns unknown action IDs into navigation.
- **Effort/risk/confidence:** S; LOW, localized dispatch; HIGH for routing, MED for native visibility consequences.

Strength: section-aware navigation keeps Disk under Tools at `rune_ui.rs:218`–`:230`; the command palette indexes pages, actions and files at `rune_palette.rs:47` onward.

## A3. Visual design and hierarchy

The shared Quiet theme has named color and typography tokens at `rune_theme.rs:9`–`:54`, semantic headings at `:66`–`:78`, and decorative icons hidden from accessibility at `:96`–`:97`. Library adapts its column count to measured width at `rune_ui.rs:922` and builds wrapping header controls at `:883`–`:887`. These are strengths in code structure, not visual acceptance results.

### [A3-01] Raise the priority of small destructive targets

- **Severity/type/method/personas:** S2 UX; code inspection, not executed; P4, P6.
- **Evidence:** Library trash explicitly uses a 24-point button height at `rune_ui.rs:1098`–`:1109`; timeline trim grips are 14 points wide at `rune_editor.rs:593`–`:604`.
- **Repro:** At supported minimum window size, navigate a populated library and manipulate timeline edges using a pointer with limited precision.
- **Expected/actual:** Comfortable targets for frequent and destructive actions. Source defines small visible targets; actual effective hit regions and DPI scaling need native measurement.
- **Impact:** Increased precision demand and accidental neighboring actions are plausible, not observed.
- **Fix sketch:** Verify hit rectangles first; enlarge invisible hit regions where necessary while retaining visual density. Prioritize trash and trim handles.
- **Effort/risk/confidence:** S; LOW, check layout overlap; MED.

No claims are made about clipping, pixel alignment, contrast compliance, animation quality, light/high-contrast appearance, or mixed-DPI behavior without a render/native review.

## A4. Hotkeys and keyboard behavior

### [A4-01] Make redo dispatch distinguish Shift

- **Severity/type/method/personas:** S2 bug; code inspection, not executed; P4, P6.
- **Evidence:** `rune_editor.rs:375`–`:386` maps every `Key::Command('z')` to `undo(false)` without checking modifiers. The annotation implementation checks Shift at `rune_annotation.rs:90`–`:94`.
- **Repro:** Open video, split a clip, press Ctrl+Z, then Ctrl+Shift+Z with no focused control.
- **Expected/actual:** Redo restores the split. The video handler dispatches Undo again. Annotation and video disagree.
- **Impact:** A familiar recovery shortcut can move edit history in the wrong direction.
- **Fix sketch:** Match modifier-aware undo/redo, including the chosen Windows Ctrl+Y policy. Test actual key events and history outcomes across both editors.
- **Effort/risk/confidence:** S; LOW; HIGH.

### [A4-02] Show registration results beside each shortcut

- **Severity/type/method/personas:** S2 UX; code inspection, not executed; P3.
- **Evidence:** `hotkeys.rs:182`–`:203` records per-binding registration failure while still returning the service. `rune_ui.rs:1397`–`:1406` renders text fields without those statuses; `:1432` announces Shortcuts applied. A later aggregate notice is possible, so this is not a claim that errors are entirely silent.
- **Repro:** Have another test app own a valid chord; enter it in Settings and Apply; revisit the affected row after dismissing the aggregate notice.
- **Expected/actual:** The affected action remains visibly marked unavailable. Settings shows the configured text without persistent per-row availability.
- **Impact:** Users cannot readily identify which advertised global actions will work.
- **Fix sketch:** Render registration status per binding and distinguish saved configuration from available registrations. Preserve existing reserved-key and duplicate validation.
- **Effort/risk/confidence:** S; LOW; HIGH.

## A5. Cross-feature flows

### [A5-01] Expose the library's 500-result boundary

- **Severity/type/method/personas:** S1 UX; code inspection, not executed; P7.
- **Evidence:** `rune_ui.rs:925`–`:939` stops rendering after 500 matching items, with no continuation in `:960`–`:992`. `docs/RUNE_MIGRATION.md:37`–`:38` confirms pagination remains missing.
- **Repro:** Seed an isolated library with 501 supported files; open Library without a search; attempt to browse the final matching file.
- **Expected/actual:** Every result is reachable with explicit continuation and count. Only the first 500 matching cards are rendered. Search can narrow the collection, so files are not universally inaccessible.
- **Impact:** Large-library browse workflows omit captures without explaining the limit.
- **Fix sketch:** Add bounded paging or incremental loading with count/continuation. Preserve whole-catalog search and use stable identities.
- **Effort/risk/confidence:** M; MED, scan/order and focus stability; HIGH.

Configured screenshot/upload workflows, watch/share/fullscreen, multi-clip joining and rich utility forms are declared missing at `docs/RUNE_MIGRATION.md:42`–`:49`. Treat these as scoped migration limitations, not verified runtime defects. Screenshot-to-editor and export completion need native acceptance testing.

## A6. Settings

### [A6-01] Make failed settings writes transactional and visible

- **Severity/type/method/personas:** S1 bug; code inspection, not executed; P3, P7.
- **Evidence:** `rune_ui.rs:194`–`:200` returns no save result and only writes an error notice. Toggle handlers mutate preferences first at `:1343`–`:1345`. Folder selection changes both preferences and the recordings root before saving at `:1451`–`:1455`. Shortcut Apply overwrites any save-failure notice with Shortcuts applied at `:1430`–`:1432`.
- **Repro:** On an isolated profile, make preference persistence fail; change capture folder or an audio switch; apply a shortcut change; restart after restoring access.
- **Expected/actual:** Explicit unsaved state or rollback, with retry. Source retains the new live value despite failed persistence; shortcut success can immediately replace the save error. Restart persistence consequences require execution.
- **Impact:** Displayed settings and next-launch settings can disagree; storage choices may surprise users.
- **Fix sketch:** Return a typed save result, preserve failure messages, and choose either rollback or explicit pending-save state. Coordinate runtime changes with persistence instead of independently mutating both.
- **Effort/risk/confidence:** M; MED, affects capture settings ownership; HIGH.

## A7. Performance architecture

### [A7-01] Move settings persistence off interaction callbacks

- **Severity/type/method/personas:** S2 UX/performance risk; code inspection, not executed; P7.
- **Evidence:** `rune_ui.rs:194`–`:200` directly calls `Preferences::save` from UI handlers, including capture segments at `:1233`–`:1235` and favorites at `:1090`–`:1093`; `preferences.rs:211` defines the synchronous save entry point.
- **Repro:** Use isolated slow storage for preferences; repeatedly change settings while recording or browsing; collect input-to-paint traces on Windows.
- **Expected/actual:** UI remains responsive during persistence. Source puts persistence on the callback path; no measured stall is claimed.
- **Impact:** Storage latency can delay input processing. The 500-card render cap is a separate catalog completeness issue, not proof of acceptable performance.
- **Fix sketch:** Reuse a single storage owner with acknowledged writes and visible pending/error state. Preserve ordering and shutdown drains.
- **Effort/risk/confidence:** M; MED, stale writes and shutdown ordering; HIGH for architecture, MED for user-visible cost.

Strengths: Library scanning runs behind a scanner at `rune_ui.rs:303`–`:304`; file jobs use workers at `:202`–`:215`; preview requests are separate from editor operations at `rune_editor.rs:191`–`:199`. Polling schedules 100 ms idle wakes and 16 ms active wakes at `rune_ui.rs:448`–`:454`; battery/CPU costs are unmeasured. No GPU, frame pacing, RAM, startup or long-session performance claim is justified here.

## A8. Privacy and trust

### [A8-01] Reveal the effective capture sources before recording

- **Severity/type/method/personas:** S1 UX/privacy-control gap; code inspection, not executed; P2, P5. No wrong-source capture or disclosure was demonstrated; do not promote this to confirmed S0.
- **Evidence:** `rune_capture.rs:92`–`:108` honors saved microphone/output endpoint IDs and display identity. `rune_ui.rs:1349`–`:1380` exposes audio booleans without device selection. `docs/RUNE_MIGRATION.md:39`–`:41` acknowledges missing display/application/device pickers.
- **Repro:** Load an isolated migrated preference fixture with a non-default display and endpoint; inspect Capture and Audio settings before starting a harmless synthetic session.
- **Expected/actual:** The effective display and endpoint identities are visible and changeable. Saved identities influence the backend, but the current chooser interfaces are absent.
- **Impact:** Users cannot easily verify or correct capture scope through Rune, especially after migration or hardware changes.
- **Fix sketch:** Show resolved source labels and unavailable-device state; add selectors before restoring broader capture claims. Require explicit decisions rather than silent fallback.
- **Effort/risk/confidence:** M/L; HIGH, source identity and active-session boundaries; HIGH for missing controls.

No network traffic, upload destinations, retained logs, diagnostic redaction or credential storage was dynamically assessed. Missing configured upload UI is not evidence of accidental upload. Close-to-tray copy explicitly says shortcuts and replay continue at `rune_ui.rs:1481`–`:1486`, a trust strength.

## A9. Notifications and feedback

### [A9-01] Replace prose-based failure classification with typed notices

- **Severity/type/method/personas:** S2 UX; code inspection, not executed; all personas.
- **Evidence:** `rune_ui.rs:796`–`:800` identifies failures with six English substrings. `:258`–`:272` expires notices not matching them. `:350`–`:370` lets independent worker results replace the same notice. The save-error overwrite in A6-01 is a concrete example.
- **Repro:** Trigger two independent completions close together; inject a recoverable failure whose text lacks those six words; inspect icon, duration and retained recovery details.
- **Expected/actual:** Severity, lifetime and actions follow the operation result. Current behavior derives failure from text and stores only one global notice.
- **Impact:** Copy edits or localization can change error lifetime and icon; later messages can displace important context.
- **Fix sketch:** Use typed severity, operation identity and explicit actions; retain actionable failures until acknowledged and offer a small outcome history where necessary.
- **Effort/risk/confidence:** M; LOW/MED, coordinate existing notice callers; HIGH for mechanism, MED for unexecuted interleavings.

Strength: toasts wrap text, expose status semantics and label dismissal at `rune_ui.rs:803`–`:842`. Native announcement behavior remains untested.

## A10. Recovery and error handling

### [A10-01] Keep trash recovery available after the toast expires

- **Severity/type/method/personas:** S1 UX; code inspection, not executed; P4, P7.
- **Evidence:** `rune_ui.rs:268` expires trash notices after ten seconds; `:240`–`:242` clears the in-memory restore list. A later trash result replaces that list at `:351`–`:353`. `recordings.rs:440`–`:452` persists a recovery journal before moving files, so original path information survives. `docs/RUNE_MIGRATION.md:38` confirms persistent recovery UI is missing.
- **Repro:** Trash a disposable capture; wait more than ten seconds, dismiss the toast, restart, or trash another capture; attempt recovery through Rune.
- **Expected/actual:** A persistent Trash route can restore earlier removals. Rune exposes only the current toast Undo; files and recovery.json remain on disk for manual recovery.
- **Impact:** Ordinary delay or another action removes the app's recovery route. This is a major recovery gap, not demonstrated permanent deletion.
- **Fix sketch:** Read existing journals into a persistent recovery view with original destination and collision-safe restore. Keep fast Undo as a shortcut.
- **Effort/risk/confidence:** M; MED, restoring to existing paths must never overwrite; HIGH.

Strength: video edits block switching while unsaved at `rune_editor.rs:109`–`:112`, and close blocks unsaved edits/export at `:551` onward. Image unsaved-close protection is not equivalent: `rune_editor.rs:124` clears annotations during open, while the guard tests video editor state. Investigate image dirty tracking with native acceptance before assigning a data-loss severity.

## A11. Accessibility

### [A11-01] Give timeline trim controls a keyboard equivalent

- **Severity/type/method/personas:** S1 UX/accessibility gap; code inspection, not executed; P6.
- **Evidence:** `rune_editor.rs:593`–`:671` labels trim grips as sliders but implements only drag input. Global editor keys at `:375`–`:386` seek frames, split, delete and undo; they do not adjust either trim boundary.
- **Repro:** Open a disposable clip; use only keyboard input to focus a trim edge and adjust its exact boundary without rebuilding the clip through split/delete.
- **Expected/actual:** Both boundaries can be adjusted through keyboard or numeric controls. Inspected trim controls have no direct keyboard adjustment path.
- **Impact:** The direct drag-to-trim operation lacks an equivalent for users unable to drag precisely.
- **Fix sketch:** Add focusable boundary controls with current/min/max values and arrow increments or numeric frame fields. Reuse the existing trim transaction so undo remains one step.
- **Effort/risk/confidence:** M; MED, trim bounds and ripple semantics; HIGH for missing handler, MED for native assistive-technology outcome.

Accessibility support in the dependency and role/label calls is useful groundwork, not proof of UI Automation correctness. Screen-reader reading order, focus restoration, modal trapping, high contrast, text scaling, reduced motion, IME, clipboard behavior and minimum-size layouts are untested. `rune_ui.rs:466` blocks editor shortcuts whenever any control is focused, which deserves a separate native keyboard check before being called a defect.

## A12. Help and troubleshooting

### [A12-01] Put setup diagnostics and accurate capability help in Rune

- **Severity/type/method/personas:** S2 UX; code/document inspection, not executed; P1, P2.
- **Evidence:** `main.rs:106`–`:125` exposes diagnostics through CLI; `docs/RUNE_MIGRATION.md:50`–`:52` acknowledges missing diagnostics UI. `README.md:14`–`:16` documents NVIDIA AV1 and FFmpeg prerequisites. The active page/action list at `rune_palette.rs:55`–`:115` offers no diagnostics/help destination. README points to a missing welcome route at `:25` and calls Updates a standalone settings route at `:29`, whereas updates render inside App at `rune_ui.rs:1476` onward.
- **Repro:** On a machine lacking FFmpeg or supported recording hardware, seek help from the app before attempting the affected task; follow README's recovery/navigation instructions.
- **Expected/actual:** Discoverable capability checks explain what works, what is unavailable, and how to recover. Diagnostics require CLI knowledge, and part of the user documentation describes obsolete navigation.
- **Impact:** Users must distinguish dependency failures from migration limitations themselves.
- **Fix sketch:** Add a help/diagnostics entry using the existing backend, show capability-specific requirements beside unavailable actions, and update user docs to Rune routes. Do not treat old retired UI validation as current acceptance.
- **Effort/risk/confidence:** M; LOW/MED, diagnostics must redact sensitive paths/details before sharing; HIGH.

## Provisional ten-heuristic scorecard

Scale 0 absent, 1 weak, 2 partial, 3 sound in inspected paths, 4 strong. These ordinal judgments are not measured usability scores; no aggregate average is warranted.

| Heuristic | Score / 4 | Evidence and rationale |
| --- | --- | --- |
| Visibility of system status | 2 | Status toast semantics at `rune_ui.rs:808`; shared replaceable prose notices, A9-01 |
| Match to user expectations | 2 | Named Library/Capture/Tools; tray labels disagree with destination, A2-01 |
| User control and freedom | 1 | Video undo exists; redo wrong and persistent trash missing, A4-01/A10-01 |
| Consistency and standards | 2 | Shared tokens and controls, `rune_theme.rs:9`; annotation/video redo differs |
| Error prevention | 2 | Hotkey duplicate/reserved validation, `hotkeys.rs:89`–`:110`; persistence lacks transaction, A6-01 |
| Recognition over recall | 2 | Unified palette, `rune_palette.rs:47`; effective recording sources hidden, A8-01 |
| Flexibility and efficiency | 2 | Global shortcuts and palette; missing per-row availability, A4-02 |
| Aesthetic/minimal design | 2 | Shared type/color hierarchy, `rune_theme.rs:41`–`:54`; visual quality unverified |
| Error recognition and recovery | 1 | Save retry and trash journal; no persistent restore UI, A10-01 |
| Help and documentation | 1 | CLI diagnostics exist; promised onboarding unavailable, A1-01/A12-01 |

## Provisional coherence scorecard

Same 0–4 scale; distinct dimensions, not a single invented product score.

| Dimension | Score / 4 | Basis |
| --- | --- | --- |
| Visual vocabulary | 3 | Shared theme tokens and heading helpers, `rune_theme.rs:9`–`:78`; no render validation |
| Navigation | 2 | Section-aware navigation, `rune_ui.rs:218`; tray route mismatch, A2-01 |
| Input conventions | 2 | Shared buttons and palette; redo inconsistency and trim accessibility, A4/A11 |
| State and persistence | 1 | Video save guards; UI/runtime/disk settings diverge on failure, A6-01 |
| Recovery | 1 | Safe backend journal; ephemeral frontend access, A10-01 |
| Product promises versus shipped behavior | 1 | README welcome promise versus migration gap, A1-01; old retired UI UX docs remain historical |
| Capture-to-library-to-edit | 2 | Completion refresh/open path, `rune_ui.rs:361`–`:367`; native end-to-end run pending |
| Trust and source transparency | 2 | Explicit tray continuation copy; hidden saved recording source identity, A8-01 |

## Priorities for synthesis and follow-up

Highest-leverage small fixes are tray routing, modifier-aware redo, persistent shortcut registration status, and preventing settings success from overwriting save failure. Persistent trash recovery, truthful migration onboarding, capture-source controls and keyboard trimming are release-readiness work. Keep performance changes dependent on persistence ownership tests, rather than introducing background writers independently.

Proposed native acceptance order: isolated first-run/migration and unavailable-data-root cases; tray restore/navigation; shortcut registration and editor key dispatch; failed settings writes/restart; trash expiry/restart/collision restore; 501-file browsing; audio/display source identity; keyboard/screen reader; capture-to-library-to-edit/export; installer/update/uninstall. Use generated media and disposable profiles only. Never exercise destructive scenarios against personal captures.

Read-only review gates completed: active module/dependency tracing; direct reads of cited handlers; obsolete UI separation. No automated tests were run. Future commands documented by the repo are `cargo build --locked -p desktop -j1`, `cargo test --locked -p desktop -j1`, `./scripts/test-package-windows.ps1`, and `./scripts/verify-rune.ps1 -SkipBuild -GpuAdapter NVIDIA` at `docs/RUNE_MIGRATION.md:70`–`:74`. Builds/compilation belong only to the authorized remote boat VM agent; this report does not authorize local compilation. Native Windows hardware/UI coverage is still necessary even if headless scenes pass.

Considered and rejected: claiming permanent trash deletion, because durable journals exist; treating omitted retired UI controls as secretly shipped Rune capabilities; applying historical hardware timings to this revision; declaring absent upload UI an upload privacy incident; equating the 500-result cap with a full-catalog search cap; claiming dependency accessibility support proves screen-reader acceptance.

Not audited: Parts B–K; full backend security/dependency advisories; installer binary contents/signature; real OS permissions and display/audio transitions; update download/restart/crash recovery; actual upload/network behavior; all tool-specific options; image dirty-state loss reproduction; resource/battery budgets; localization; competitor operation. These are explicit synthesis gaps, not passed checks.
