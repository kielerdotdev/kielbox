# QA implementation and acceptance tracker

> Historical record. Source paths and commands describe the audited revision.
> Retired frontend sources and proof scripts were removed after this report;
> see [Rune status](../docs/RUNE_MIGRATION.md) for current behavior. Exact original reports remain in git at `a52bff7`.

Updated 11 November 2026. Baseline `d96346b2ee44cf44b9ffeb5881c6a44d2cbcb6c7`.
The user authorized implementation after the read-only audit. This tracker is an
implementation ledger, not a new audit or release certificate.

Source evidence: [final audit](qa-final.md), [suite](qa-suite.md),
[capture](qa-capture.md), [utilities](qa-utilities.md),
[safety/accessibility](qa-safety_accessibility.md), [benchmarks](qa-benchmarks.md)
and [Boat ledger](qa-vm_validation.md). Preserve these reports as baseline
provenance; do not rewrite their historical findings as fresh execution results.

## Status and ownership rules

All F01–F24 have source implementations in the shared checkout. Two independent
reviews found five follow-up issues; these were corrected and included in the
integrated Boat tests. The table below retains the native acceptance criteria;
source implementation and remote passing tests do not close those native gates.

| Findings | Implementation evidence |
| --- | --- |
| F01/F05/F07, editor F17 | [Editor](implemented-editor.md): ordered recovery acknowledgements, output disclosure, redo and keyboard geometry |
| F02/F04/F09/F21, region F17 | [Screenshots](implemented-screenshots.md): retained failures, clipboard-only Copy, owned delivery, utility callback and quit/update guard |
| F03/F18, source transparency | [Capture](implemented-capture_state.md): lifecycle, tray integration, pause reset, cap disclosure, display/audio choices |
| F06/F08/F12/F13/F15/F19/F20/F22 | [Shell](implemented-shell_recovery.md): routing, persistence rollback, existing-data route, paging, Trash, labels/status, notices and thumbnails |
| F09/F10/F11/F13/F14/F23/F24 | [Utilities/disk](implemented-utilities_disk.md): forms, formats, capture tickets, result actions, scan details and paging |
| F16 and ignored-data protection | [Cleanup](implemented-cleanup.md): partial ledger/exit3 and explicit ignored-data opt-in |

Root integration corrections: cancelled utility workers remain owned and joined;
failed utility saves retain the generated result; image recovery sidecars are
hidden from normal Library scanning and trashed with the source after closing its
editor; stale/invalid journals produce per-batch warnings without hiding valid
entries; validated missing source parents are recreated on restore. Overlay
annotation keys route to the annotation actor. Native capture uses the tested
tail-draining retirement helper, and capture polling emits separate typed notices
so a later success cannot erase an earlier error. Regression tests cover these
state and recovery contracts; native interaction remains untested.

Remote evidence: [integrated validation](implemented-validation.md), 268
Windows/Wine passes, three ignored entries, 19 Linux passes, release build/smoke
and six renders. The final capture-copy wrapping adjustment passed a second Windows test,
release/smoke/render confirmation; see the validation report. No local Rust compilation or source
commits. Updated README/Utilities/migration instructions describe the implemented
controls rather than the old audit baseline.

| ID | Severity / kind | Integration owner | Acceptance check required before closure |
| --- | --- | --- | --- |
| F01 | S0 defect | Image editor | Track image revisions and successful saves. Dirty close, replacement and quit offer recovery or Save/Discard/Cancel. Cancel and failed save retain edits; editable and flattened saves remain distinct. |
| F02 | S0 defect | Screenshot delivery | Unwritable output retains accepted pixels/annotations and supports retry or another destination. Failed attempt cannot destroy the session. |
| F03 | S0 defect | Capture lifecycle + tray | Existing tray setter receives quick lifecycle. Native minimized start/pause/finish/stop shows accurate icon, tooltip and Stop action independently of replay. |
| F04 | S0 privacy disclosure | Screenshot delivery + product policy | Establish clipboard-only or explicitly named retention policy before activation. Test writable/unwritable folders and independent clipboard/storage failures; inspect retained artifacts. |
| F05 | S0 disclosure defect | Image editor | Explain original retention before editable save. Reopen SSXE and remove cover to verify semantics; review flattened sharing output. Do not promise secure blur/pixelation. |
| F06 | S1 defect | Shell/tray | Library and Settings routes restore the window on their named page. Unknown action IDs cannot silently navigate to Tools. |
| F07 | S2 defect | Video editor | Split, Ctrl+Z, Ctrl+Shift+Z restores the split. Test chosen Ctrl+Y policy and consistency with annotation; text fields keep editing behavior. |
| F08 | S1 defect | Settings persistence | Inject failed write; no success replaces error, live and persisted settings follow a documented rollback/pending policy. Retry/restart folder, audio and hotkey changes. |
| F09 | S1 handoff defect | Utility ownership + screenshot delivery | OCR/QR region capture attaches once to originating session. Switch, cancel, close origin and delayed completion cannot attach to another tool or save implicitly under an undisclosed policy. |
| F10 | S1 form defect | Utility forms | Folder index chooses a folder; Split chooses an existing parent directory and valid Columns/Rows. Verify 7×5 image split 3×2 covers every pixel with unique output and cancellation cleanup. |
| F11 | S1 form defect | Utility forms | Expose backend effect/orientation/index/video modes with correct units and per-tool defaults. Blank optional hash digest, valid split grid and sensible video timestamp. Remote request-mapping tests plus native mode/dialog checks. |
| F12 | S1 docs/onboarding | Documentation + shell/install integration | Documentation portion implemented in README, Utilities and migration guide. Verify every current route against final integrated source. Native first-run/existing root/unavailable root preserve data; active root distinct from Capture folder; chooser/diagnostics remain pending unless evidenced. |
| F13 | S2 scale UX gap | Library + disk navigation | With 501+ matches/children, every result is reachable by bounded continuation/search and shown/total counts are accurate. Whole-library search remains available; maintain bounded rendering. |
| F14 | S1 ownership defect | Utility state | Request/result/tool identity survives navigation. Delayed A completion cannot become B output; failed rerun cannot show stale Done; deferred save dialogs act only on their bound result. |
| F15 | S1 recovery gap | Library/trash | Journal-backed recovery remains available after toast dismissal/timeout, multiple batches and restart. Partial restore and destination collision preserve replacement bytes and remaining journal entries. |
| F16 | S1 accounting defect | Disk cleanup + clean CLI | Cancel apply after at least one removal on disposable fixtures; final JSON retains removed/error ledger and cancellation state, exit 3. Preview/revalidation and existing safety exclusions remain intact. |
| F17 | S1 accessibility gap | Region/image interaction + video editor | Keyboard selects/moves/resizes a small region and places/selects/manipulates annotation geometry. Check monitor bounds/DPI and Narrator dimensions. Verify timeline piece/slider operation and retain frame/Start here/End here alternatives. |
| F18 | S1 lifecycle defect | Capture lifecycle | One truthful quick lifecycle drives UI/tray. Pause unavailable when idle; new sessions reset pause; Starting/Paused/Finishing/Failed and progress are truthful. Disclose and test 600-second cap. |
| F19 | S2 accessibility/status gap | Settings/hotkeys | Each shortcut field has distinct action name and persistent per-binding availability. Occupied chord shows failure beside its row; inspect native accessibility tree/Narrator. |
| F20 | S2 feedback defect | Shell/notices | Typed severity and operation identity replace English substring inference. Concurrent outcomes retain actionable errors until acknowledgement; verify recovery actions and announcements. |
| F21 | S0 provisional lifecycle risk | Screenshot delivery + shutdown/update | App owns delivery and update busy state. Quit/update during controlled slow save/copy drains safely or retains explicit recoverable state. Actual interrupted-save loss was not reproduced by audit. |
| F22 | S2 defect | Library thumbnails | Search sole video match beyond index 499; its visible card gets thumbnail/duration under existing queue/decode bounds. Page/filter changes avoid unbounded work or stale assignment. |
| F23 | S2 result/action gap | Disk navigation | Persist logical-size/accounting/exclusions, actual backend/fallback and sampled failures after scan. Reveal/Copy path targets exact file; denied/cancelled scan reports incomplete totals. Keep disk view read-only. |
| F24 | S2 action gap | Utility result actions | Exact result-bound copy/save/reveal and format controls match advertised behavior. Convert Output is meaningful or absent; full text survives bounded preview; no-clobber sentinel unchanged. Pin remains unavailable unless separately implemented and verified. |

## Integration contracts and reconciliation

- Screenshot owner and utility owner must agree on typed capture purpose/origin,
  request generation, cancellation and frame/path ownership before F09 closes.
  F02/F21 share delivery retention, tracked worker and shutdown/update ownership.
- F03/F18 share one quick lifecycle and the existing tray API. Avoid separate
  booleans that disagree about pause, completion or finalization.
- F10/F11/F24 consume the existing toolkit request/backend contracts. F14 must
  bind outputs and deferred dialogs before new actions are accepted. Reuse
  current APIs and dependencies; retain no-clobber publication and resource bounds.
- F13/F22 share the same filtered/paged visible paths; thumbnail admission must
  not return to the unfiltered first-500 prefix. Disk F13/F23 is a separate view.
- F08/F19/F20 must distinguish saved preferences, live registration and visible
  failure status. A successful runtime action is not proof of durable save.
- Retain collision-safe trash journals, explicit cleanup apply, revalidation,
  link/root/worktree exclusions and source preservation. No broad safety-policy
  relaxation belongs in a UI correction.
- The documentation owner changed only README.md, docs/UTILITIES.md,
  docs/RUNE_MIGRATION.md, this tracker and the expressly requested completion note.
  No source edits, commits, local compiler execution or delegated work occurred.

Before release, integration owner reviews final source and agent notes, fills
actual owner/file/test references for every row, records remote results against
one identified integrated snapshot, then reconciles current docs. In particular,
replace dated baseline limitations only when new controls and behavior have
source evidence; keep unexecuted native checks open. Historical retired UI guide and
migration proof counts remain labelled historical. Do not use a clean `git diff`
or generated scene as evidence of native acceptance.

## Remote and native verification gaps

V01 passed in Boat for the integrated source, including desktop/toolkit and
platform-specific disk/clean suites. V02's six Wine software-rendered scenes
passed and were inspected at 1200×800; native GPU proof remains open. V03–V10
remain native/product acceptance work. No new hardware claims are implied.

| Gate | Owner | Required evidence |
| --- | --- | --- |
| V01 integrated regression suite | Central Boat validation | Exact source snapshot including working edits, target/toolchain, commands, exit codes and test names. Scoped disk/clean, toolkit utilities and desktop tests; packaging checks where supported. Never archive only committed HEAD and omit the fixes. |
| V02 Rune scenes / GPU ownership | Central validation + Windows GPU owner | Six page renders/element trees and generated checkerboard proof on compatible adapter. Software-rendered Wine scenes do not prove native GPU/capture behavior. |
| V03 installer/data/update | Native Windows owner | Binary hash, clean per-user install, unsigned prompts/elevation, startup defaults, existing/unavailable root, uninstall retention; update interruption/authenticity/rollback and busy-save behavior. |
| V04 image capture/privacy/recovery | Native Windows owner | Synthetic captures; clipboard interoperability, failure/retry, dirty exits, no-clobber collision, retained artifact review, flattened pixels versus editable originals, slow save quit/update. |
| V05 recording/replay/audio | Native Windows/NVIDIA owner | Actual display/audio identities, repeated replay saves/readiness, minimized indicators, pause/new session, ten-minute cap, source/device loss, sleep/wake/RDP and output playability/audio sync. A 30-minute tutorial test is incompatible with current cap unless policy changes. |
| V06 utilities/disk/cleanup | Native Windows owner | Native dialogs, OCR languages/corpus, QR modes, text/image actions; NTFS/directory accounting, hard links/junction/offline/denied/network/cloud fixtures, 501 children, cancellation/full disk; destructive cleanup only on disposable fixtures including ignored-data policy. |
| V07 accessibility/input/layout | Native accessibility owner | Keyboard journeys, Narrator/NVDA/UI Automation, focus/modal restoration, slider ranges, text entry/IME/RTL, high contrast and 100/150/200% scale, narrow window and target sizes. Dependency flags/roles alone are insufficient. |
| V08 capture environment | Native Windows/GPU owner | Mixed DPI/HDR, multiple/negative-coordinate monitors, DRM/elevated windows, fullscreen/protected games, controller/anti-cheat and GPU reset. No anti-cheat safety claim without evidence. |
| V09 performance/resource study | Performance owner | Cold start/tool/hotkey latency; idle/boot footprint, two-game FPS/1% lows/frame time, representative 1M/5M-file scans, 1/4/8-hour leaks/battery/thermals. Test build timing is not startup or scan performance. |
| V10 trust and user journeys | Product/QA owner | Observed network/update/telemetry/account policy; matched competitor runs and persona task times/success/ease. Replay-to-share under 30s and capture-to-output goals need actual tasks, not feature tables. |
| V11 Kielbox Cloud ([plan](cloud.md), [guide](../docs/CLOUD.md)) | Cloud owner + native Windows owner | Implemented in source and unit/integration tested on Linux only: `cloud/` server (73 Workers-runtime tests, typecheck, build), `crates/cloud` client (18 wiremock tests, local end-to-end upload against `npm run dev`), desktop worker, `cloud.json`, Cloud settings tab and card button (28 new desktop tests incl. a stalling-server shutdown). **Not validated:** no browser run of the web explorer (M4 checklist in the guide is unticked); no Windows run of the desktop UI, Credential Manager storage, clipboard or tray; the two Cloud fixture scenes were added to the Wine render list but not rendered; no real Cloudflare deployment, so Access, presigned R2 URLs, multipart against R2 and the 2 GiB smoke test (guide, "Deployment smoke test") have not run. No library list layout exists, so the list-view control is not implemented; Revoke is wired in the worker with no button. Close only with a recorded deployment smoke test and a native Windows pass. |

Prior execution is limited to the historical Boat Linux baseline: 14 disk + 2
clean integration tests, zero failures, and `clean --help` exit 0. The initial
cold attempt timed out; resumed build/test took 36.26 seconds. None of these
results validates the new working edits, Windows semantics or native UI.

## Missing modules and strategic choices

These are separate from the F01–F24 defect/action list. Implementing all audit
fixes does not establish competitor parity or require inventing absent modules.

| Category | Current scope / decision | Owner / acceptance if pursued |
| --- | --- | --- |
| Absent modules | Configured upload/share, clipboard history manager, duplicate finder, bulk renamer, system monitor, OS-wide search, screen sampler/ruler/magnifier are absent from the inspected Rune product path. | Product decides scope; implementation and native evidence needed before listing as current features. Clipboard viewer and internal palette are narrower existing tools. |
| Migration capabilities | Quick window/region targets, watch/share/fullscreen windows, richer annotation styles, last-region recall, rich disk views and full diagnostics UI remain scoped parity decisions. | Respective module owner; do not infer completion from retained retired UI files or backend branches. Installed Storage onboarding is implemented; full diagnostics UI is not. |
| Strategic opportunities | Task-first onboarding; one dependable local capture workspace; merge QR/image workflows; advanced placement for diagnostic tiles; reviewed destination sharing and size presets. | Product prioritization after safety gates, with explicit acceptance criteria. No new framework or cloud service is needed to close the current defects. |
| Cleanup policy risk | Ignored worktree data is now protected by default with explicit opt-in. Concurrent mutation remains a documented risk. | Product/safety owner decides narrower opt-in/quarantine policy; native disposable-fixture safety matrix remains required regardless of F16 ledger fix. |
| Release positioning | Experimental preview versus daily-use replacement, lead persona, secure-redaction promise, Copy retention and duration/hardware support need explicit policy. | Product owner; broad replacement/readiness claims remain unsupported by current evidence. |
