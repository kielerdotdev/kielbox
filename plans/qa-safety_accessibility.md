# Rune safety and accessibility audit

> Historical record. Source paths and commands describe the audited revision.
> Retired frontend sources and proof scripts were removed after this report;
> see [Rune status](../docs/RUNE_MIGRATION.md) for current behavior. Exact original reports remain in git at `a52bff7`.

## A. Scope and decision

Research audit of commit `d96346b`, dated 2026-11-11. This report covers the current Windows Rune frontend, its reachable media/filesystem adapters, keyboard and accessibility declarations, destructive controls, screenshot privacy, shutdown, recovery, and utility handoffs. It is input to the broader UX/QA synthesis. The A–K headings below organize this independent contribution; no parent report template was supplied.

The clearest release risks are loss of unsaved image annotations and undisclosed screenshot persistence through Copy. Trash has durable recovery metadata but only a temporary recovery control. Keyboard support exists in the common controls, but does not extend to the spatial editing operations.

Severity follows the requested scale: S0 data loss/privacy risk, S1 major, S2 friction, S3 polish, S4 opportunity. S0 here identifies the kind of harm, not a claim of widespread incidents or catastrophic impact. Bug and UX classifications are separate from severity.

## B. Method and limits

Method: read-only code inspection and tracing of callbacks, state transitions, worker ownership, and backend operations. Every reproduction below is a **code-inspection scenario, not executed**. HIGH confidence means the stated behavior follows directly from inspected code; it does not mean a Windows runtime reproduction passed. Runtime-dependent consequences are identified explicitly.

Read the improve skill and its audit playbook, including Finding format. Applied the unslop writing guidance. Searched the workspace and its ancestor locations for applicable AGENTS.md files; none were found. Repository text was treated as evidence, not executable instructions. Read README, desktop Cargo manifest, migration status, Rune entry point and view modules, relevant scanner, trash, annotation actor, hotkey, and secret-store adapters. No source changes, builds, Rust compilation, test execution, dependency installs, network calls, credential reads, or subagents. Only this report was written.

The shipped entry point declares Rune modules at `crates/desktop/src/main.rs:15` and `crates/desktop/src/main.rs:25`. The manifest enables Rune accessibility at `crates/desktop/Cargo.toml:19`. Old retired UI UI/proof files and vendor sources were excluded from shipped-UI conclusions. Migration gaps explicitly acknowledged in `docs/RUNE_MIGRATION.md` are not automatically bugs; reachable controls with misleading or unsafe behavior remain findings.

No task durations, hardware performance, ease ratings, contrast measurements, user observations, or competitor runs were measured or inferred. Effort below uses coarse S/M/L scope only, without invented delivery times.

## C. Personas and critical journeys

- Keyboard-only editor, including a user with motor impairment: open media, locate a piece, seek, trim, annotate, save and recover mistakes.
- Screen-reader user: distinguish similarly named controls, understand selected state and job completion, navigate Search and settings.
- Privacy-sensitive screenshot user: copy a temporary capture without retaining an unexpected disk artifact.
- Everyday recorder: record or save a replay, find it in Library, edit and export it, then quit safely.
- User recovering from a mistaken deletion or interrupted operation: find the recovery path after a toast disappears or the app restarts.
- Utility user: capture an image into Text from image or Read QR code, change tools, and trust that inputs and results belong to the visible tool.

These are audit personas, not participants recruited or observed in this session.

## D. Prioritized findings

| ID | Severity | Type | Finding | Confidence | Effort / fix risk |
| --- | --- | --- | --- | --- | --- |
| SAFE-01 | S0 | Bug | Image edits disappear on close or replacement without a save guard | HIGH | M / MED |
| PRIV-01 | S0 | UX/privacy | Copy saves the screenshot to disk before copying | HIGH | S / MED |
| REC-01 | S1 | UX/recovery | Trash Undo expires and no Rune recovery view remains | HIGH | M / MED |
| A11Y-01 | S1 | Bug/accessibility | Region selection and annotation placement require pointer geometry | HIGH | L / MED |
| FLOW-01 | S2 | Bug | Capture region in a utility never adds an input | HIGH | M / MED |
| FLOW-02 | S2 | Bug | A running utility can publish its result under a different tool | HIGH | M / MED |
| A11Y-02 | S2 | UX/accessibility | Timeline pieces and slider grips lack explicit keyboard operation | HIGH for adapter omission | M / MED |
| SAFE-02 | S1 | Bug/lifecycle | Screenshot delivery is detached from shutdown ownership | HIGH for lifecycle gap; MED for lost-file outcome | M / MED |
| A11Y-03 | S2 | UX/accessibility | Shortcut inputs all use the accessible name Not set | HIGH for declaration; MED for spoken result | S / LOW |

S0 findings should be addressed before relying on this build for private screenshot or unsaved annotation work. Lifecycle, recovery and keyboard fixes need Windows acceptance checks before release.

## E. Detailed findings and reproduction scenarios

### [SAFE-01] Guard unsaved image edits before closing or replacing the editor

- **Severity/type:** S0 data-loss risk, bug.
- **Method/status:** Code inspection; scenario not executed.
- **Personas:** Image annotator, keyboard editor, user switching between captures.
- **Controls:** Library's `Open {title}`, editor tab close control, `Save editable…`, `Save image…`, tray `Quit Kielbox`.
- **Repro:** Open a PNG from Library. Add a rectangle or text annotation. Do not use either save control. Close its editor tab, or open another Library image. Reopen the original image. A second scenario is quitting with those unsaved annotations.
- **Expected:** Preserve a recoverable draft or offer Save/Discard/Cancel before destroying the modified image session.
- **Actual demonstrated by code:** `State::close` guards pending video revisions only when `self.editor.is_some()`, then clears `self.annotation`. Opening replacement media has the same video-only guard and resets annotation state. The image actor's Drop closes its queue and wakes its worker, without a project save. Project persistence is an explicit command from `Save editable…`. Image annotation changes have no corresponding draft-save worker in the inspected Rune adapter. The source image remains intact; the lost data is the user's new annotation work.
- **Evidence:** `crates/desktop/src/rune_editor.rs:109` open guard and `:124` annotation reset; `crates/desktop/src/rune_editor.rs:552` close and `:568` annotation reset; `crates/desktop/src/rune_annotation.rs:267` explicit project save; `crates/toolkit/src/image_editor.rs:395` actor Drop; `crates/toolkit/src/image_editor.rs:836` explicit atomic project save.
- **Impact:** A normal navigation action discards work without an explicit destructive decision. The polished video autosave behavior makes the image behavior especially easy to misjudge.
- **Effort:** M. **Risk:** MED, dirty-state tracking must distinguish saved editable projects from flattened exports and respect pending actor commands.
- **Confidence:** HIGH for close/replacement loss; quitting follows the same absence of image persistence, but was not run.
- **Fix sketch:** Track image document revisions and save completion. Offer Save/Discard/Cancel on replacement, tab close and quit, or implement a recoverable image draft. Keep flattened image export distinct from saving an editable document.
- **Acceptance:** On Windows, edit an image, exercise all three exits, cancel each, and verify the edit remains. Save and reopen both an editable project and flattened export. Simulate a failed save and verify no automatic discard.

### [PRIV-01] Disclose or remove disk persistence from Copy

- **Severity/type:** S0 privacy risk, UX contract defect.
- **Method/status:** Code inspection; scenario not executed.
- **Personas:** Privacy-sensitive screenshot user, user copying transient screen content.
- **Controls:** Region overlay `Copy`, Ctrl+C; adjacent `Save`.
- **Repro:** Capture a region of a disposable test image. Select `Copy`, or press Ctrl+C. Inspect the configured capture folder and Library.
- **Expected:** Copy places the selected result on the clipboard. If the product also retains it, that must be clear before activation or controlled by an explicit preference.
- **Actual demonstrated by code:** All finish modes enter `deliver`, which creates a screenshot PNG and calls `save_image` before branching on `Finish::Copy`. Only after persistence does it write the clipboard. The first explicit retention disclosure is the completion notice `Screenshot copied and saved to the library.` Saving fails before clipboard copy if the folder is unwritable.
- **Evidence:** `crates/desktop/src/rune_shot.rs:685` separate Save and `:692` Copy controls; `crates/desktop/src/rune_shot.rs:405` key mappings; `crates/desktop/src/rune_shot.rs:342` delivery and `:352` unconditional save; `crates/desktop/src/rune_shot.rs:358` after-action disclosure.
- **Impact:** Private content remains on disk when the selected action only says Copy. Clipboard use also unnecessarily depends on writable capture storage. This is local retention, not evidence of uploading or credential exposure.
- **Effort:** S. **Risk:** MED, intentional capture-history expectations may change.
- **Confidence:** HIGH.
- **Fix sketch:** Make Copy clipboard-only, with an explicit save-history preference, or label the action `Copy and save` before use. Do not make clipboard success depend on disk-save success when the user requests clipboard-only behavior.
- **Acceptance:** Exercise Copy and Save against writable and unwritable folders using non-sensitive generated content. Verify the chosen retention policy and independent error messages.

### [REC-01] Keep trash recovery available after the toast expires

- **Severity/type:** S1 major recovery gap, UX. This is not permanent file deletion.
- **Method/status:** Code inspection; scenario not executed.
- **Personas:** User correcting an accidental deletion, slower keyboard/screen-reader user, user returning after restart.
- **Controls:** Library `Move to trash`, toast `Undo`, `Dismiss`.
- **Repro:** Move a disposable Library capture to trash. Wait beyond the ten-second undo notice, dismiss it, or restart. Attempt recovery using the Rune UI. Also trash two captures sequentially and try to undo the first operation.
- **Expected:** A persistent Trash/Recently deleted view or equivalent recovery action. Short-lived Undo should be a convenience rather than the only in-app recovery path.
- **Actual demonstrated by code:** Expiration calls `dismiss_notice`, clearing the in-memory moved list. Each completed trash operation replaces that list rather than adding a recovery history. Undo appears only while it is nonempty. The backend moves files into a private `.trash/batch-*` folder and writes durable `recovery.json`; this retains a manual recovery path, but current Rune navigation does not expose it. Migration docs explicitly acknowledge the missing persistent recovery interface.
- **Evidence:** `crates/desktop/src/rune_ui.rs:240` dismissal and `:259` expiry; `crates/desktop/src/rune_ui.rs:352` trash result replacement; `crates/desktop/src/rune_ui.rs:829` conditional Undo; `crates/desktop/src/recordings.rs:424` batch creation and `:443` durable manifest; `docs/RUNE_MIGRATION.md` remaining Library parity work.
- **Impact:** Users lose the obvious recovery path after a short delay and may mistake app-private trash for Windows Recycle Bin. Sequential removals lose earlier toast recovery even before expiration.
- **Effort:** M. **Risk:** MED, recovery must reject collisions and handle partially moved batches.
- **Confidence:** HIGH.
- **Fix sketch:** Add a persistent Rune recovery list backed by the existing journals. Preserve no-clobber restore behavior. Until then, explain capture-trash location and recovery in the notice and avoid expiring the only recovery control.
- **Acceptance:** Recover after timeout, restart, multiple batches, original-path collision and a partially restored batch. Never overwrite the replacement file.

### [A11Y-01] Provide keyboard equivalents for region geometry and annotation placement

- **Severity/type:** S1 major accessibility limitation, bug.
- **Method/status:** Code inspection; scenario not executed.
- **Personas:** Keyboard-only and motor-impaired users; screen-reader users need separate spatial editing design.
- **Controls:** Overlay `Drag to capture an area`, Ctrl+A `whole screen`, annotation tool buttons, `Label text`.
- **Repro:** Invoke Screenshot region. Without a pointer, attempt to select a small rectangle, move it, or change its dimensions. In an opened image, choose a rectangle or text tool by keyboard and attempt to place it at a chosen location.
- **Expected:** Keyboard region creation/move/resize, or editable coordinates and dimensions. Annotation tools need a focusable canvas and an operable alternative for placing/selecting objects.
- **Actual demonstrated by code:** Overlay geometry is driven by root `on_drag(pointer)`. Its key handler can select the whole screen with Ctrl+A, accept, cancel, or choose a tool, but has no arrow/coordinate operation for region geometry. The standalone annotation canvas translates drag coordinates into actor Down/Move/Up; its key adapter forwards text, deletion and undo but no position or geometry commands. Choosing a tool alone does not place its shape. Whole-screen selection is not equivalent to privately selecting a small area.
- **Evidence:** `crates/desktop/src/rune_shot.rs:386` complete key handler; `crates/desktop/src/rune_shot.rs:735` pointer geometry; `crates/desktop/src/rune_shot.rs:755` drag instruction; `crates/desktop/src/rune_annotation.rs:347` canvas and `:365` drag-only placement; `crates/desktop/src/rune_annotation.rs:83` key adapter.
- **Impact:** The primary region and annotation workflows require pointer precision even though their toolbar actions can receive keyboard focus.
- **Effort:** L. **Risk:** MED, spatial commands must preserve monitor bounds, scaling and actor coordinate mapping.
- **Confidence:** HIGH for missing application-level alternatives. Native assistive pointer emulation was not assessed and does not establish a keyboard equivalent.
- **Fix sketch:** Add region coordinates and keyboard movement/resizing with announced dimensions. Add a focusable annotation canvas plus keyboard object selection and placement controls. Retain Escape and explicit save/copy actions.
- **Acceptance:** Complete a small-region screenshot and a placed text annotation without mouse emulation. Check bounds at mixed DPI and negative monitor coordinates, then verify Narrator feedback.

### [FLOW-01] Return Capture region to the utility input list

- **Severity/type:** S2 friction, bug.
- **Method/status:** Code inspection; scenario not executed.
- **Personas:** Text from image and Read QR code users.
- **Controls:** Utility `Capture region`, overlay `Save`, `Copy`, `Edit`, utility `Add file…`, `Run`.
- **Repro:** Open `Text from image` with an empty file list. Choose `Capture region`, select a test region, then Save. Return to the utility and inspect its inputs before Run. Repeat with Edit and Copy.
- **Expected:** A capture initiated inside Input should become an input of that originating utility, or the UI should explicitly explain a different destination.
- **Actual demonstrated by code:** The utility launches the generic screenshot function. Overlay delivery saves to Library, refreshes it, optionally opens the image editor, or copies. No origin/tool context is carried, and no completion appends the path to `s.tools.files`. The only inspected file append in the utility is the native Add file dialog.
- **Evidence:** `crates/desktop/src/rune_tools.rs:449` Add file append; `crates/desktop/src/rune_tools.rs:458` Capture region callback; `crates/desktop/src/rune_tools.rs:949` generic capture; `crates/desktop/src/rune_shot.rs:370` delivery callback.
- **Impact:** The apparent direct capture-to-OCR/QR flow stops short. Users must find the saved screenshot and manually add it, or unintentionally Run with existing unrelated inputs.
- **Effort:** M. **Risk:** MED, asynchronous completion must target the originating session, not whichever tool is open later.
- **Confidence:** HIGH.
- **Fix sketch:** Pass an explicit capture destination/session identifier. Add the saved result to that utility only after successful delivery, and surface capture cancellation or origin disappearance clearly.
- **Acceptance:** Capture into an empty utility, switch tools during capture, cancel, and close the originating tool. Confirm the result attaches once to the right session.

### [FLOW-02] Associate utility jobs and retained options with their originating tool

- **Severity/type:** S2 friction and correctness, bug.
- **Method/status:** Code inspection; scenario not executed.
- **Personas:** User moving between utilities while a longer operation runs.
- **Controls:** Utility `Run`, `Tools` back button, another tool tile/Search result, `Choose output…`.
- **Repro:** Start a sufficiently long disposable operation in tool A. Use Tools to return to the catalog and open tool B before completion. Wait for A to finish. Observe B's Result. Separately choose an output in A and open B; inspect the retained output.
- **Expected:** Keep the running result attached to A, prevent ambiguous switches, or visibly identify the background job. Tool changes should make retained inputs/output explicit and relevant.
- **Actual demonstrated by code:** One shared tools State owns tool, inputs, output, receiver and result. `open_tool` changes the displayed tool and clears message/result only. It does not cancel or identify the pending receiver. `poll` later assigns that receiver's output to the current shared result. Back/close only toggle `open`. Files, text and chosen output also persist between different tools.
- **Evidence:** `crates/desktop/src/rune_tools.rs:11` shared state; `crates/desktop/src/rune_tools.rs:69` request snapshot; `crates/desktop/src/rune_tools.rs:99` completion assignment; `crates/desktop/src/rune_tools.rs:321` switch; `crates/desktop/src/rune_tools.rs:696` back action.
- **Impact:** A result can appear under the wrong heading and with Done status. Retained output paths can surprise a user starting a different operation. This report does not claim a specific overwrite without inspecting that operation's backend contract.
- **Effort:** M. **Risk:** MED, ownership and cancellation changes can drop results or target the wrong file.
- **Confidence:** HIGH for misassociation when a job spans the switch; whether a particular job runs long enough is runtime-dependent.
- **Fix sketch:** Store tool/session identity with each job and route completion accordingly, or block switching with clear background-job controls. Decide and communicate which fields persist per tool.
- **Acceptance:** Complete, cancel and fail A after opening B; B must never present A's output as its own. Verify output selection behavior across tools.

### [A11Y-02] Make timeline pieces and sliders explicitly operable by keyboard

- **Severity/type:** S2 accessibility friction, UX.
- **Method/status:** Code inspection; scenario not executed.
- **Personas:** Keyboard-only video editor, screen-reader user.
- **Controls:** `Seek`, `Piece N, … s`, `Trim start`, `Trim end`, `Start here`, `End here`, `Previous frame`, `Next frame`.
- **Repro:** Open a clip, split it, then attempt to focus a specific later piece and adjust its bounds using Tab and arrow keys. Try the Seek and trim slider controls directly. Compare with the available frame-step and Start here/End here buttons.
- **Expected:** Pieces have a predictable focus/selection path; sliders expose values and accept keyboard adjustment. Keyboard commands continue while a non-text editor control is focused.
- **Actual demonstrated by code:** Seek has slider role/range but only a drag handler; pieces have button role/on_press but no explicit focusability in their construction. Trim grips have slider role and drag only, with no range values or keyboard handlers. The global editor key path returns early whenever `tx.focused().is_some()`, not only for text fields. Frame-step and Start here/End here buttons provide partial alternatives; this is not a claim that all video editing is impossible by keyboard.
- **Evidence:** `crates/desktop/src/rune_editor.rs:593` trim grips; `crates/desktop/src/rune_editor.rs:845` Seek; `crates/desktop/src/rune_editor.rs:873` pieces; `crates/desktop/src/rune_editor.rs:959` button alternatives; `crates/desktop/src/rune_ui.rs:466` focus guard.
- **Impact:** A user cannot rely on the declared slider/button roles to provide the app's spatial editing behavior. Global arrow and edit shortcuts also stop at focused controls.
- **Effort:** M. **Risk:** MED, shortcuts must not intercept text editing or duplicate native button activation.
- **Confidence:** HIGH for missing explicit adapter support. Actual tab reachability, Rune default role behavior and UI Automation adjustment require runtime confirmation with the pinned Rune version.
- **Fix sketch:** Add explicit focusability and keyboard operations to pieces/sliders, numeric accessible ranges, and a shortcut guard based on text-entry context. Keep the existing button alternatives.
- **Acceptance:** Select the second piece, seek to a boundary, trim and undo without a pointer. Test focus on buttons versus text fields and inspect Narrator/UI Automation values.

### [SAFE-02] Own screenshot delivery until save and clipboard completion

- **Severity/type:** S1 major lifecycle gap, bug. Potential screenshot loss is a runtime hypothesis.
- **Method/status:** Code inspection; scenario not executed.
- **Personas:** User saving a screenshot immediately before quitting or updating.
- **Controls:** Overlay `Save`, `Copy`, `Edit`; tray `Quit Kielbox`; settings `Restart to update`.
- **Repro:** On Windows with a disposable large capture and a deliberately slow test destination, choose Save, then immediately Quit Kielbox after the overlay closes. Inspect whether the PNG is complete. Separately attempt Restart to update during the same save window.
- **Expected:** The app owns the delivery task until completion, waits on shutdown, and reports failures before destroying the only capture session.
- **Actual demonstrated by code:** Overlay poll takes the accepted frame, closes the session and spawns `deliver` on a new thread. The JoinHandle is joined only in patient fixture mode; normal execution drops it. This thread is not in the application FileJob list, whose Drop joins workers. Tray quit invokes `tx.quit` directly; update guards check `s.jobs` but cannot see this delivery thread.
- **Evidence:** `crates/desktop/src/rune_shot.rs:325` accepted-frame handling and detached spawn; `crates/desktop/src/rune_ui.rs:83` tracked FileJob Drop; `crates/desktop/src/rune_ui.rs:419` tray quit; `crates/desktop/src/rune_ui.rs:1532` update activity check.
- **Impact:** Screenshot persistence is excluded from the normal shutdown ownership guarantee. Process termination may interrupt save/copy and the closed session cannot retry. An incomplete PNG or loss has not been demonstrated in this audit.
- **Effort:** M. **Risk:** MED, shutdown must drain without deadlocking app-update callbacks.
- **Confidence:** HIGH for detached ownership; MED for specific interrupted-output outcomes.
- **Fix sketch:** Route delivery through an application-owned task with a shutdown barrier and update busy state. Preserve the accepted frame for retry until successful delivery.
- **Acceptance:** Quit during slow save/copy, fail the destination write, and attempt update during delivery. Verify completion or explicit cancellation and intact existing files. Do not use personal screenshots.

### [A11Y-03] Give each shortcut input its action name

- **Severity/type:** S2 accessibility friction, UX.
- **Method/status:** Code inspection; scenario not executed.
- **Personas:** Screen-reader user reassigning capture shortcuts.
- **Controls:** Settings → `Shortcuts`, seven inputs including `Turn replay on or off`, `Save replay`, `Screenshot region`; `Apply shortcuts`.
- **Repro:** Navigate by text fields with Narrator in Shortcuts. Identify the action for each input without reading surrounding rows manually.
- **Expected:** Each input announces its action plus value, with Not set used only for an empty binding.
- **Actual demonstrated by code:** All seven fields pass `Not set` as the input name. The shared input builder assigns that string directly as `.label` and placeholder. Visible action titles are sibling text in the settings row, without a label reference in this call.
- **Evidence:** `crates/desktop/src/rune_ui.rs:1397` shortcut rows and `:1401` shared name; `crates/desktop/src/rune_theme.rs:514` input and `:516` explicit label; `crates/desktop/src/rune_theme.rs:541` settings row layout.
- **Impact:** Field navigation loses the distinction between actions, increasing the chance of editing the wrong binding.
- **Effort:** S. **Risk:** LOW, accessible-name-only change.
- **Confidence:** HIGH for identical explicit names; MED for exact Narrator output, which may include extra context.
- **Fix sketch:** Pass the SHORTCUTS action title as each field's accessible name; separate placeholder text from the name API if needed.
- **Acceptance:** Inspect the accessibility tree and have Narrator announce distinct action names and current bindings for all seven fields.

## F. Accessibility and keyboard coverage summary

Common controls are intentionally focusable and labeled. `Btn` declares button role/name/focus at `crates/desktop/src/rune_theme.rs:269`; switches include role/name/checked at `:367`. Library open controls include the capture title at `crates/desktop/src/rune_ui.rs:1040`. Search has a dialog and modal declaration at `crates/desktop/src/rune_palette.rs:320`, plus Escape and arrow handlers at `:221` and input-specific handlers at `:247`. These are meaningful foundations.

The gaps concentrate in spatial controls and contextual naming, not a blanket absence of accessibility. Actual focus-ring visibility, focus restoration, modal confinement, automatic scrolling of highlighted Search results, reading order, Narrator live announcements, high contrast, text scaling and native IME behavior remain untested. No WCAG conformance verdict is asserted from these declarations alone. Small 24px icon controls and fixed-width utility columns deserve Windows zoom/DPI review, but no target-size or clipping failure was measured here.

## G. Destructive actions and recovery

Library Move to trash is an immediate action, but recoverability makes lack of confirmation alone insufficient evidence of a bug. The concrete defect is the short-lived recovery route in REC-01. The backend validates canonical paths remain within Library at `crates/desktop/src/recordings.rs:411`, journals before moving at `:440`, and refuses overwrite on restore at `:471`. These protections should survive any UI recovery work.

Video Delete is an edit operation, not source-media deletion. Undo and autosave exist; do not conflate it with destructive disk removal. Image edits have the separate loss boundary in SAFE-01. Imported external files are rejected by the trash backend, so an external import's visible Move to trash can fail with `File is outside this Library`; that is a smaller affordance mismatch, not evidence that outside files are deleted.

Persistent capture-recovery presentation is a documented migration gap. It was not exercised through interrupted native recording in this audit.

## H. Upload, tokens and local privacy

No uploader or token configuration controls were found in `rune_*.rs`, and the inspected screenshot paths save locally or use the clipboard. `docs/RUNE_MIGRATION.md` explicitly lists configured screenshot/upload workflows as not wired. This audit therefore makes no claim that a current Rune Upload button leaks tokens, uploads unexpectedly, or stores credentials in plaintext. Old retired UI upload settings are not shipped UI evidence.

The shared secret backend uses OS keyring entries through `crates/toolkit-services/src/secrets.rs:64`, reads passwords at `:80`, and writes through the keyring at `:88`. Its documented environment override and memory fallback are not findings by themselves. This was a narrow architecture check, not a complete upload security audit. No credential store, environment secret, settings credential value or personal file was read or reproduced. Reachable Copy retention is the concrete privacy issue in PRIV-01.

## I. Shutdown, failure and cross-tool consistency

Video save workers drain on Drop at `crates/desktop/src/rune_editor.rs:100`, and pending video edits prevent close at `:557`. Export teardown cancels and joins at `:539`. Utilities cancel and join at `crates/desktop/src/rune_tools.rs:138`; recording teardown stops and joins at `crates/desktop/src/rune_capture.rs:305`. These are strengths, not proof of every native shutdown path. Screenshot delivery is an ownership exception in SAFE-02. No claim is made about shutdown duration or hardware finalization success.

Close-to-tray is explicit in Settings at `crates/desktop/src/rune_ui.rs:1481`, with fallback quit if no tray at `crates/desktop/src/main.rs:167`. A future runtime review should verify that hiding, quitting and restarting communicate which jobs continue, cancel or finish, especially image drafts and quick recording.

Cross-tool state ownership is the main consistency concern: generic capture completion cannot target the originating utility, while generic utility completion can target the wrong currently visible tool. Fixing both should establish an explicit origin/session contract rather than adding unrelated callback exceptions.

## J. Validation plan for the Windows owner

No commands in this section were executed. The project documents `cargo build --locked -p desktop -j1`, `cargo test --locked -p desktop -j1`, `./scripts/test-package-windows.ps1` and `./scripts/verify-rune.ps1 -SkipBuild -GpuAdapter NVIDIA` in `docs/RUNE_MIGRATION.md`. Local Rust compilation remains prohibited for this audit; only the authorized boat VM agent may compile remotely. Existing rendered fixtures and old proof results do not establish these scenarios.

Use generated, non-sensitive media in an isolated Library and capture folder. Prioritize:

1. Image close/replace/quit with dirty state and failed save.
2. Copy retention with writable/unwritable storage; distinguish clipboard and disk outcomes.
3. Trash recovery after timeout, restart, sequential removals and path collision.
4. Capture-to-utility destination and utility completion after navigation.
5. Keyboard-only geometry, timeline piece selection and slider values; Narrator input names.
6. Screenshot save during quit/update with controlled slow storage.

Capture actual expected/actual observations, build SHA, Windows version and assistive technology version. Record evidence only after execution. Add focused state/worker regression tests for confirmed fixes; use live Windows acceptance for native clipboard, accessibility and process teardown.

## K. Untested areas, rejected claims and handoff

Untested: live Windows/Narrator behavior, accessibility tree mapping of the pinned Rune version, rendering/contrast/scaling, multi-monitor capture, clipboard/IME, native dialogs, installation/update execution, recording/replay recovery, device selection, remote storage failures, utility backend-specific file replacement, all upload protocol security, external services and competitor workflows. No runtime screenshots or proof artifacts were produced.

Considered and rejected as unsupported or overstated:

- "Trash permanently deletes files after ten seconds." False. The UI loses Undo; media and recovery journals remain in private capture trash.
- "Rune exposes plaintext uploader tokens." No reachable token settings were found; old retired UI files are not shipped UI.
- "The whole frontend is inaccessible." False. Shared controls and Search have explicit accessibility/keyboard work. Spatial editing and some names have specific gaps.
- "All workers are detached on exit." False. Several important workers join; screenshot delivery is the concrete exception.
- "Copy proves network exfiltration." False. The demonstrated behavior is unexpected local persistence.
- "Missing migration parity automatically means a regression bug." Unsupported. Findings here focus on reachable actions and concrete expectation/safety failures.

Recommended order: image draft protection and Copy retention policy; screenshot worker ownership; persistent trash recovery; origin-aware utility jobs/capture; keyboard geometry and timeline support; shortcut names. Do not describe any of these scenarios as executed until the Windows owner records results. No implementation, commits or additional plan files were created by this research pass.
