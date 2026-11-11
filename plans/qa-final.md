# Kielbox UX and QA report

> Historical record. Source paths and commands describe the audited revision.
> Retired frontend sources and proof scripts were removed after this report;
> see [Rune status](../docs/RUNE_MIGRATION.md) for current behavior. Exact original reports remain in git at `a52bff7`.

Audit date: 11 November 2026. Source version 0.1.0, commit `d96346b2ee44cf44b9ffeb5881c6a44d2cbcb6c7`. No installed binary/build hash was tested. Six GPT-6.1 Sol researchers contributed; the synthesis owner re-read the principal finding paths. Source code was not changed.

## Evidence and product context

This is a source-supported release-readiness audit with limited remote execution, **not completed hands-on Windows acceptance**. All UI reproduction steps below are proposed acceptance scenarios, not actions observed on a running app. “Actual” describes inspected code. No participant hesitations, task times, ease ratings, FPS, accessibility compliance or competitor timing results were invented.

Kielbox is Windows-only preview software. macOS/Linux are planned. Positioning in README is capture, clips and disk tools in one app. Primary inferred users are creators, support staff, gamers with supported NVIDIA hardware and developers. Intended audience priority remains a product-owner question. The desktop uses Rune; obsolete retired UI screens do not count as shipped functionality. An unsigned per-user installer is documented; source/portable builds also exist. Tray, global shortcuts and active recording/replay workers can continue after window close. Actual installed process/service/driver inventory was not measured. No mandatory account gate was identified in current Rune paths; external-service requirements are unverified. Game recording requires NVIDIA AV1; editing/export/screencast require FFmpeg/FFprobe. Migration gaps are explicit in `docs/RUNE_MIGRATION.md`.

Methods used: source-based heuristic evaluation, cognitive walkthrough of controls and state transitions, cross-tool call tracing, official-documentation competitive comparison, and remote Linux fixture tests. Task-based Windows walkthroughs, persona studies, native stress tests and matched competitor runs could not be completed. Personas below use the supplied Gabe, Chloe, Sam, Ada, Pete, Nina and Alex definitions.

### Module inventory

Y means reachable in current Rune or the separately identified CLI, not complete parity. N means absent from the current product path inspected. Partial capabilities are stated explicitly.

| Module | Included? | Benchmark competitors | Current boundary |
| --- | --- | --- | --- |
| Screenshot capture and annotation | Y | ShareX, Snagit, Windows Snipping Tool | Region, desktop/window, annotation; styles and workflow parity incomplete |
| Screen recording / GIF | Y / GIF inaccessible | OBS, ShareX, ScreenToGif | Desktop screencast; GIF backend lacks selectable UI mode |
| Game clipping / instant replay | Y | NVIDIA App/ShadowPlay, Medal, Xbox Game Bar | NVIDIA AV1 manual replay; no demonstrated game-event detection |
| Clip library, trim/edit, sharing | Y / sharing absent | Medal, ShadowPlay Gallery | Local library, trim/split/mute/export; configured upload not wired |
| Disk space analyzer | Y | WizTree, WinDirStat, TreeSize | Read-only scan and navigation; logical bytes |
| File upload / sharing | N in Rune | ShareX, Dropbox | Backend/legacy presence does not establish shipped upload UI |
| Color picker / ruler / magnifier | Y typed color / N ruler and magnifier | PowerToys, ShareX | Typed conversion, not screen sampling |
| OCR / text extraction | Y | PowerToys Text Extractor, ShareX | Local Windows OCR from file; region handoff broken |
| Clipboard manager | N | Win+V, Ditto, CopyQ | Current-value viewer only |
| Bulk renamer / organizer | N | PowerRename, Bulk Rename Utility | No current module found |
| Duplicate finder / cleaner | N duplicate finder | dupeGuru, Czkawka | Developer-artifact cleaner is a different tool below |
| Hash / QR / converters | Y | ShareX, HashCheck, ScreenToGif | Generic forms block some modes/defaults |
| System monitor / process tools | N | Task Manager, Process Explorer | Window enumeration is not a process monitor |
| Launcher / quick search | Y internal palette only | PowerToys Run, Flow Launcher | App pages/actions/files, not Everything-style system indexing |
| Developer-artifact cleanup CLI | Y separate CLI | Manual reviewed Git/filesystem cleanup; no exact equivalence to duplicate cleaners | Dry-run default, explicit permanent apply |
| Other image/file/window utilities | Y | ShareX, PowerToys | Listed individually in section F |

## A. Executive summary

**No-go for broad daily-use or “replaces ShareX/Medal/WizTree” release claims.** A clearly labeled experimental preview is a separate product decision. Five source-supported S0 risks require native reproduction and resolution: unsaved image annotations discarded on close/replacement; failed region delivery loses the capture session; quick screencasts do not reach the tray recording indicator; Copy silently persists a PNG; editable projects retain original pixels without a sharing warning.

Three strengths are worth preserving: no-clobber image/video publication; journaled, collision-safe library trash; and cleanup preview/revalidation protections, with 16 disk/clean tests passing on Boat Linux.

The most promising module is local video capture/editing, supported by autosave and explicit worker ownership. It is not yet a proven flagship. The weakest reachable tools are Split and Folder index because their dialogs contradict backend requirements. Generic utility forms also hide existing modes and reuse misleading defaults.

Bundling currently offers shared navigation and files more reliably than complete workflows. Capture-to-OCR loses its handoff; clip-to-share ends at local export; disk analysis cannot reveal a selected file. Restore those transitions before expanding the catalog.

No Windows UI, GPU, game, screen-reader or competitor run was performed. Scores are provisional expert judgments, not measured user outcomes. Boat executed only scoped Linux tests. Native release acceptance remains outstanding.

## Part A execution ledger, in requested order

“Inspected” is not “passed.” This ledger follows A1–A12; specialist module inspection ran in parallel, so the requested strictly sequential live-test protocol was not performed.

| Area | Evidence-based result | Still required |
| --- | --- | --- |
| A1 Install/first run | README promises “Use existing data”; Rune starts Library and migration UI is pending. F12 | Installer size/time, elevation, unsigned warnings, component selection, startup defaults, uninstall leftovers/data retention |
| A2 IA/discoverability | Six-page shell, grouped Tools and internal palette exist; tray routes fail; library truncates. F06/F13 | Ten-second first-use study, goal-language search, favorites/pinning/hiding discoverability |
| A3 Consistency | Shared theme and controls; image/video save protection differs; generic utility labels mismatch tasks | Rendered light/dark/system themes, spacing, overlays and secondary-window consistency |
| A4 Hotkeys | Central fields and validation; redo error and missing persistent registration status. F07/F19 | OS/game/elevated/RDP conflicts, press-to-set experience and cheat sheet |
| A5 Cross-tool flows | Capture→library→editor exists; OCR handoff and sharing stop short. F09; repeat below | Native clipboard/export/destination checks |
| A6 Settings | Save errors can be replaced with success; runtime and saved state diverge. F08 | Reset/import/export/search/restart semantics, portable configuration |
| A7 Resources | Worker isolation and bounded utility work exist; UI settings save is synchronous | All startup/idle/boot/battery/FPS/long-run measurements |
| A8 Trust | F03–F05 privacy risks; effective source selectors absent; no current upload route found | Network monitoring, actual credentials, consent, telemetry, anti-cheat and capture-scope verification |
| A9 Feedback | One replaceable notice; error severity inferred from English substrings; quick lifecycle mismatch. F18/F20 | DND, focus stealing, notification capture and audible feedback |
| A10 Recovery/updates | Video save guards and trash journals exist; image/capture recovery and persistent trash UI fail. F01/F02/F15/F21 | Sleep/wake, full disk, crash/update interruption and rollback |
| A11 Accessibility | Named common controls exist; spatial actions and shortcut names have gaps. F17/F19 | Narrator/NVDA, high contrast, scaling, RTL, IME, focus and motion |
| A12 Help | CLI diagnostics exist; linked guides describe dormant controls. F12 | Novice setup troubleshooting and accurate in-app help |

## B. Findings table

All rows use method **source inspection/cognitive walkthrough**, unless noted. Reproduction uses disposable synthetic data. High confidence means the code path was traced, not runtime reproduced. Paths below are relative to repository root. Supporting reports provide expanded evidence and acceptance criteria.

| ID | Module / area | Severity | Persona(s) | Description and type | Steps to reproduce | Expected | Actual in inspected code | Evidence | Recommended fix |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| F01 | Image editor | S0 | Sam, Chloe | Bug: unsaved annotation loss | Open PNG; annotate; close tab or open another image; reopen | Save/discard/cancel or draft recovery | Video-only dirty guard then annotation cleared | `crates/desktop/src/rune_editor.rs:109,552`; `crates/toolkit/src/image_editor.rs:395` | Track image revisions and guard replacement/close/quit |
| F02 | Region delivery | S0 | Sam | Bug: failed output loses session | Annotate region; make owned output folder unwritable; Save; restore permissions | Retained pixels/edit state with Retry | Session closed before save; only error remains | `crates/desktop/src/rune_shot.rs:325,342,382` | Retain accepted frame/session until successful delivery |
| F03 | Screencast/tray | S0 | Chloe, Nina | Bug: hidden recording state | Stop native replay; start screencast; close to tray; inspect indicator | Accurate recording indicator and Stop label | Quick flag initialized false; Rune never calls setter | `crates/desktop/src/tray.rs:204,211`; `rune_ui.rs:445` | Feed quick lifecycle into tray |
| F04 | Screenshot Copy | S0 | Sam, Nina | UX/privacy: undisclosed local retention | Select synthetic region; Copy; inspect library folder | Copy-only or explicit pre-action retention disclosure | PNG saved before clipboard; disclosure only afterward | `crates/desktop/src/rune_shot.rs:342,352,692` | Independent Copy or explicit Copy and save policy |
| F05 | Editable annotation output | S0 | Sam | UX/privacy: source pixels retained | Cover private test text; Save editable; reopen/remove cover | Explain original retention before saving/sharing | Base PNG and separate objects serialized | `crates/image-editor/src/project.rs:125`; `rune_annotation.rs:267` | Label original-preserving project versus flattened sharing output |
| F06 | Tray navigation | S1 | Pete, Nina | Bug: wrong destination | Choose Library then Settings from tray | Restore named page | Both fall through to Tools | `crates/desktop/src/tray.rs:150`; `rune_ui.rs:418` | Explicit routes plus restore window |
| F07 | Video redo | S2 | Chloe, Alex | Bug: redo invokes undo | Split; Ctrl+Z; Ctrl+Shift+Z | Restore split | Command-z always calls undo(false) | `crates/desktop/src/rune_editor.rs:375` | Modifier-aware redo, consistent across editors |
| F08 | Settings persistence | S1 | Pete, Gabe | Bug: false save success | Make preferences unwritable; Apply shortcuts | Persistent failure/rollback or unsaved state | Save error replaced by “Shortcuts applied.” | `crates/desktop/src/rune_ui.rs:194,1430` | Return save result and coordinate runtime/persisted changes |
| F09 | Capture→OCR/QR | S1 | Sam, Nina | Bug: no input handoff | Text from image; Capture region; Save; Run | Capture supplies OCR input | Capture saved to library; tools.files unchanged | `crates/desktop/src/rune_tools.rs:458`; `rune_shot.rs:370` | Carry origin/request identity and attach captured pixels |
| F10 | Split / Folder index | S1 | Ada, Pete | Bug: incompatible dialogs | Split 3×2 image to chosen file path; index folder through Add file | Directory pickers and valid grid | Split requires directory; shared dialogs select files | `crates/desktop/src/rune_tools.rs:443,570`; `crates/toolkit/src/utilities.rs:361,403` | Per-task folder input/output and validation |
| F11 | Utility forms | S1 | Chloe, Pete, Nina | UX: unreachable modes and wrong defaults | Select effect/GIF; fresh File hash Run; thumbnail short video | Relevant controls/defaults | mode=0; hash text starts color value; thumbnail timestamp=1024 | `crates/desktop/src/rune_tools.rs:32,534`; `crates/toolkit/src/utilities.rs:503,779,796` | Explicit forms, named units, per-tool initial state |
| F12 | Onboarding/help | S1 | Nina, Pete | UX/docs: unavailable instructions | Follow README Use existing data and Utilities guide | Current routes and migration chooser | Welcome and several documented controls absent | `README.md:25`; `docs/RUNE_MIGRATION.md:50`; `docs/UTILITIES.md:12` | Correct docs immediately; restore data-root onboarding |
| F13 | Library/disk scale | S2 | Chloe, Ada | UX: silent 500-row limits | Browse 501 matching captures or 501 child directories | Every match reachable, explicit count | Loops take/break at 500 without continuation | `crates/desktop/src/rune_ui.rs:925`; `rune_tools.rs:880` | Bounded paging/search with shown/total counts |
| F14 | Utility result ownership | S1 | Nina, Pete | Bug: wrong/stale output identity | Run slow tool A; switch B; also success then failed rerun | Result belongs to exact request; failure clears current success | Late result assigned to B; old result keeps Done after error | `crates/desktop/src/rune_tools.rs:53,99,321,585` | Request IDs, explicit result state and bound save actions |
| F15 | Trash recovery | S1 | Nina, Alex | UX: only recovery control expires | Trash fixture; wait >10s or restart; seek restore | Persistent recovery route | Undo list cleared; durable files/journal remain | `crates/desktop/src/rune_ui.rs:240,268`; `recordings.rs:440` | Journal-backed Trash view; retain collision-safe restore |
| F16 | Clean JSON cancellation | S1 | Ada, Pete | Bug: deletion ledger discarded | Apply JSON to disposable eligible directories; cancel after first removal before next candidate | Partial removed list with cancelled state | Interrupted returns before outcome JSON; generic error only | `crates/disk/src/cleanup.rs:396,398`; `crates/clean/src/main.rs:108,179` | Return structured partial outcome; retain exit 3 |
| F17 | Spatial accessibility | S1 | Alex | Accessibility: pointer-only region/annotation geometry | Select small region and place annotation using keyboard alone | Coordinates/nudge/placement equivalent | Drag handlers supply geometry; keys lack equivalent | `crates/desktop/src/rune_shot.rs:386,735`; `rune_annotation.rs:347` | Keyboard geometry and object manipulation |
| F18 | Screencast state | S1 | Chloe, Nina | Bug/UX: pause mismatch and hidden cap | Pause idle/old session; start new; compare state; record toward cap | Truthful lifecycle and disclosed limit | Pause flag retained; progress discarded; 600-second cap | `crates/desktop/src/rune_capture.rs:188,289,649`; `quick_recording.rs:160` | Single lifecycle state; reset pause and disclose cap |
| F19 | Shortcut accessibility/status | S2 | Alex, Pete | UX: identical names and no row status | Navigate shortcut fields with AT; configure occupied chord | Distinct action names, persistent availability | All explicit labels “Not set”; registration status not beside field | `crates/desktop/src/rune_ui.rs:1397`; `rune_theme.rs:514`; `hotkeys.rs:182` | Separate label/placeholder and show registration result |
| F20 | Notices | S2 | All | UX: fragile severity and overwritten outcomes | Complete jobs together; failure without recognized substring | Typed severity and retained recovery | Single string slot; English substring classification | `crates/desktop/src/rune_ui.rs:258,350,796` | Typed operation notices and actionable error retention |
| F21 | Screenshot shutdown | S0 provisional | Sam | Lifecycle risk: save task not owned | Save large synthetic region to slow storage; immediately Quit | Drain save or retain recoverable capture | Normal delivery thread detached; app job guards cannot see it | `crates/desktop/src/rune_shot.rs:325`; `rune_ui.rs:419,1532` | App-owned task and shutdown barrier; reproduce interrupted-save consequence |
| F22 | Older thumbnails | S2 | Chloe | Bug: filtered old match has no thumbnail/duration | Search unique video beyond first 500 files | Visible match queued | Thumbnail admission uses unfiltered first 500 | `crates/desktop/src/rune_thumbs.rs:94`; `rune_ui.rs:925,1044` | Queue visible/filter-matched paths within budget |
| F23 | Disk results/action | S2 | Ada, Nina | UX: incomplete explanations and no file handoff | Scan denied fixture; select large file; seek error details/Reveal | Explain omissions/backend and reveal exact file | Aggregate errors; row acts only on directories | `crates/desktop/src/rune_tools.rs:203,779,897` | Persistent scan summary, sampled errors, Reveal/Copy path |
| F24 | Utility output actions | S2 | Sam, Pete | UX: advertised actions absent | Run image/text utility; seek Copy/Pin/Save text; use Convert Output | Relevant destination and result actions | Only conditional Save image; Convert ignores generic Output | `crates/desktop/src/rune_tools.rs:643,664`; `crates/toolkit/src/utilities.rs:91,262` | Accurate copy, result-bound save/reveal actions and formats |

F21 is deliberately raised from the specialist's S1 to provisional S0 because the user requested potential data-loss paths be treated as blockers until proven safe. The detached ownership is code-confirmed; actual interrupted output remains a hypothesis. F05 is a disclosure defect, not a serializer bug. None of F03–F05 establishes network exfiltration. Blur/pixelation irreversibility in flattened exports is untested.

Cleanup permanently removes eligible artifacts/worktrees under explicit --apply. Ignored local data and concurrent mutation remain S0-class policy risks requiring a disposable Windows safety matrix; documentation does not prove them harmless. This audit did not establish a new wrong-target deletion defect. Do not conflate cleaner policy with a duplicate finder or the read-only disk UI.

## C. Task performance summary

NT = not tested/measured. Source-only outcomes below are not observed user success. No ease scores can responsibly be assigned without performing the tasks.

| Scenario | Module | Persona | Time | Competitor time | Success | Ease 1–5 | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Region→annotate→copy | Screenshot | Sam | NT | NT | NT | NT | F01/F02/F04; clipboard compatibility untested |
| Record tutorial with pause/audio | Screencast | Chloe | NT | NT | NT | NT | F03/F18; no 30-minute audio run; cap is 10 minutes |
| Save last play→Discord <30s | Replay/library | Gabe | NT | NT | NT | NT | Replay local path exists; configured sharing/size preset absent |
| Browse and trim older clip | Library/editor | Chloe | NT | NT | NT | NT | F07/F13/F22 |
| Find large folder/file | Disk | Ada/Nina | NT | NT | NT | NT | F13/F23 |
| Capture text→clipboard | OCR | Sam | NT | NT | NT | NT | F09/F24; no accuracy corpus |
| Pick screen color | Color | Pete | NT | NT | Missing screen sampler | NT | Current typed conversion only |
| Read current clipboard | Viewer | Nina | NT | NT | NT | NT | No history module |
| Hash against expected | File hash | Pete | NT | NT | NT | NT | Clear default color text workaround |
| Generate/read QR | QR | Nina | NT | NT | NT | NT | Backend paths present; native input/output untested |
| Split image/index folder | Utilities | Pete | NT | NT | Predicted blocked normal path | NT | F10 |
| GIF conversion | Video converter | Chloe | NT | NT | Missing selectable mode | NT | F11 |
| Keyboard region/annotation | Capture/editor | Alex | NT | NT | Predicted blocked geometry | NT | F17 |
| Cleanup fixture suite | disk/clean | Ada/Pete | Not user timing | NT | Y: 16 automated tests | N/A | Boat Linux only |
| clean --help | CLI | Pete | Not measured | N/A | Y | N/A | Executed, exit 0 |

### Cross-tool flow repeat after module inspection

This second pass repeats the source trace, not a live task run. No measured step count or smoothness score is assigned.

| Flow | Trace and outcome | Context loss / acceptance requirement |
| --- | --- | --- |
| Capture→annotate→upload→copied link | Region→actor→local output exists; upload route missing | Destination review and link return cannot be completed inside current Rune |
| Replay→trim→Discord-size export→share | Replay→library→editor→MP4 exists; preset/share absent | Manual external compression/sharing required; <30s goal unvalidated |
| Disk→large folder→Explorer/delete/move | Scan/drill-down exists; file Reveal/move/delete absent | Read-only policy is acceptable; add safe Reveal before destructive integration |
| OCR/color→clipboard/history | OCR file input and typed color output exist; direct capture handoff broken | Text selection workaround; no persistent history; result actions incomplete |
| Any output→send to | Shared utility state exists without robust origin IDs | F14 must be fixed before extending common actions |

## D. Performance measurements and test machine

Boat guest: Ubuntu 24.04.4, Linux 6.8.0-117-generic x86_64/KVM, 4 visible vCPUs, AMD EPYC 9455 host model string, 7.8 GiB RAM, 2 GiB swap, 79 GiB root filesystem. No /dev/dri, NVIDIA tooling, Wine or PowerShell. Rust/Cargo 1.99.1 installed remotely. Host CPU string does not mean 48 allocated cores.

| Measurement | Result | Method / limitation |
| --- | --- | --- |
| Scoped tests | 16 passed: 14 disk + 2 clean integration; 0 failed | `cargo test --locked -p disk -p clean -j1` on Boat committed HEAD |
| First cold test attempt | Timed out at 60s, SIGTERM, no OOM | Included toolchain/dependency installation; not test failure or app startup |
| Resumed build/test invocation | Cargo reported 36.26s | Build plus tests, not scan speed or end-user performance |
| clean --help | Exit 0 | CLI smoke only |
| App cold start / tool open latency | Not measured | No Windows binary execution |
| Capture hotkey latency | Not measured | No native capture environment |
| Idle CPU/RAM/GPU/disk and boot impact | Not measured | VM inventory is not app footprint |
| FPS average/1% lows/frame time/input latency | Not measured | No GPU or games; no anti-cheat claims |
| 1TB/1M and 5M-file disk scan | Not measured | No representative drive corpus |
| 1/4/8-hour leaks, battery and thermals | Not measured | No long-duration laptop/game run |
| 30-minute audio sync/output quality | Not measured | No capture run |

All compilation ran on Boat; no local Rust compilation. Source transfer used git archive HEAD, no personal media or environment secrets. Disposable no-snapshot VM shutdown was requested and it disappeared from listing. Full command ledger and limitations are in [VM validation](qa-vm_validation.md).

## E. Heuristic scorecards

Provisional source-based judgments only: 1 severe gap, 2 substantial weakness, 3 adequate basic implementation, 4 strong inspected coverage, 5 exceptional demonstrated coverage. No 5 is justified here. N/T means not assessable. These replace specialist 0–4 scales; do not combine those scores with this table. Visual judgments assess structure only, not rendered quality. No aggregate average.

Columns: H1 status, H2 real-world match, H3 control/freedom, H4 consistency, H5 error prevention, H6 recognition, H7 efficiency, H8 minimalist structure, H9 error recovery, H10 help.

| Module | H1 | H2 | H3 | H4 | H5 | H6 | H7 | H8 | H9 | H10 | Short evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Overall | 2 | 2 | 2 | 3 | 2 | 2 | 2 | 3 | 1 | 2 | Shared shell; F01–F24 state/handoff gaps |
| Screenshot/annotation | 2 | 2 | 1 | 2 | 1 | 3 | 2 | 3 | 1 | 2 | F01/F02/F04/F05/F17 |
| Screencast/GIF | 1 | 2 | 2 | 2 | 2 | 2 | 2 | 3 | 2 | 2 | F03/F11/F18 |
| Replay | 3 | 2 | 3 | 3 | 2 | 2 | 3 | 3 | 2 | 2 | Owned workers, source selectors absent |
| Library/video editor | 2 | 3 | 2 | 2 | 3 | 3 | 2 | 3 | 2 | 2 | Autosave; F07/F13/F15/F22 |
| Disk analyzer | 2 | 2 | 3 | 3 | 4 | 2 | 2 | 3 | 2 | 2 | Read-only/cancel; F13/F23 |
| Typed color | 2 | 2 | 2 | 2 | 3 | 2 | 2 | 2 | 2 | 2 | Valid color default; irrelevant generic fields |
| OCR | 2 | 2 | 2 | 2 | 3 | 2 | 2 | 2 | 2 | 2 | Local file backend; F09/F14/F24 |
| QR read | 2 | 2 | 2 | 2 | 3 | 2 | 2 | 2 | 2 | 2 | Same handoff/result gaps |
| QR create | 2 | 3 | 2 | 2 | 3 | 2 | 2 | 2 | 2 | 2 | Text→image; generic form/result ownership |
| Clipboard viewer | 2 | 3 | 2 | 2 | 3 | 2 | 2 | 2 | 2 | 2 | Explicit on-demand read; no history |
| Image viewer | 2 | 3 | 2 | 2 | 3 | 2 | 2 | 2 | 2 | 2 | Preview/save; advertised actions missing |
| Beautify | 2 | 1 | 2 | 2 | 3 | 1 | 2 | 2 | 2 | 2 | Padding/radius called Width/Height |
| Effects | 2 | 1 | 1 | 2 | 3 | 1 | 1 | 2 | 2 | 2 | Only grayscale reachable |
| Combine | 2 | 2 | 2 | 2 | 3 | 2 | 2 | 2 | 2 | 2 | Vertical only; gap mislabeled |
| Split | 2 | 1 | 1 | 2 | 3 | 1 | 1 | 2 | 2 | 2 | F10; safe temporary output |
| Image thumbnails | 2 | 3 | 2 | 2 | 3 | 2 | 2 | 2 | 2 | 2 | Bounded aspect fit; shared state gaps |
| Image convert | 2 | 2 | 2 | 2 | 4 | 2 | 2 | 2 | 2 | 2 | No-clobber; misleading Output control |
| Video convert | 2 | 2 | 2 | 2 | 4 | 1 | 2 | 2 | 2 | 2 | No-clobber; MP4-only UI |
| Video thumbnails | 2 | 1 | 2 | 2 | 3 | 1 | 1 | 2 | 2 | 2 | Timestamp default/labels wrong |
| File hash | 2 | 2 | 2 | 2 | 3 | 1 | 2 | 2 | 2 | 2 | Streaming/match output; invalid default |
| Folder index | 2 | 1 | 1 | 2 | 3 | 1 | 1 | 2 | 2 | 2 | Folder input contract blocked |
| Window inspector | 2 | 2 | 2 | 2 | 3 | 2 | 2 | 2 | 2 | 2 | JSON enumeration, generic form |
| Internal palette | 3 | 3 | 3 | 3 | 3 | 3 | 3 | 3 | 2 | 2 | Named pages/actions/files; native focus untested |
| clean CLI | 3 | 3 | 2 | 3 | 3 | 3 | 3 | 3 | 2 | 3 | Preview/revalidation; F16 and permanent deletion policy |

## F. Competitive verdict and portfolio

These are **provisional reachable-feature comparisons**, not timed or hands-on competitive verdicts. Speed, accuracy/output quality and stability are **NT for every row**; ease is not scored. “Worse” means a documented benchmark task is less complete in current Rune. No module earns Better or Equal from this evidence. Portfolio labels assess current interface readiness, not backend quality or market demand.

References: [ShareX](https://getsharex.com/), [ShareX editor](https://getsharex.com/docs/image-editor), [ShareX indexing](https://getsharex.com/docs/index-folder), [OBS recording](https://obsproject.com/kb/standard-recording-output-guide), [ScreenToGif](https://github.com/NickeManarin/ScreenToGif), [NVIDIA App](https://www.nvidia.com/en-us/software/nvidia-app/release-highlights/), [Medal platform](https://support.medal.tv/support/solutions/articles/48001271455-platform-support-on-medal-tv), [WizTree FAQ](https://diskanalyzer.com/faq), [PowerToys Text Extractor](https://learn.microsoft.com/windows/powertoys/text-extractor), [PowerToys Color Picker](https://learn.microsoft.com/en-us/windows/powertoys/color-picker). Researcher accessed official pages; detailed source mappings are in [benchmark report](qa-benchmarks.md).

| Module | Benchmark | Speed | Accuracy/quality | Ease | Feature depth | Stability | Overall | Would switch? | Portfolio/reason |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Screenshot/annotation | ShareX | NT | NT | NT | Worse: output/workflow controls | NT | Worse | N until safety/recovery fixed | Weak; promising integrated region editor |
| Screencast/GIF | OBS, ScreenToGif | NT | NT | NT | Worse: targets/formats/state | NT | Worse | N for tutorial/GIF workflow | Weak; indicator and lifecycle first |
| Replay | NVIDIA App, Medal | NT | NT | NT | Worse: selectors/sharing | NT | Worse | Maybe for local AV1 users after testing | Weak now; flagship candidate after native proof |
| Library/trim/share | Medal | NT | NT | NT | Worse: archive/sharing depth | NT | Worse | Maybe for local trim-only use | Weak; strongest suite foundation with autosave |
| Disk | WizTree | NT | NT | NT | Worse: results/actions | NT | Worse | N for full analyzer replacement | Weak; safe read-only base |
| Typed color | PowerToys Color Picker | NT | NT | NT | Missing screen sampling/history | NT | Worse | N for screen picker | Cut candidate as standalone tile; merge into image tools |
| OCR | PowerToys Text Extractor | NT | NT | NT | Worse region→clipboard | NT | Worse | N for screen extraction | Weak; fix capture handoff |
| QR create/read | ShareX | NT | NT | NT | Worse handoffs/actions | NT | Worse | Maybe for local file tasks | Weak; merge into one QR tool |
| Clipboard viewer | ShareX viewer | NT | NT | NT | Narrow current-value inspection | NT | Unverified | Maybe for debugging only | Cut candidate as prominent tile; keep advanced utility |
| Image viewer/convert | ShareX | NT | NT | NT | Worse output actions | NT | Worse | Maybe after forms corrected | Weak; merge with editor/open/export |
| Beautify | ShareX effects | NT | NT | NT | Worse exposed parameters | NT | Worse | N today | Weak; integrate image editor |
| Effects | ShareX | NT | NT | NT | Only grayscale selectable | NT | Worse | N | Weak; expose or hide unavailable promises |
| Combine | ShareX | NT | NT | NT | Vertical only | NT | Worse | Maybe for narrow task | Weak; orientation/ordering needed |
| Split | ShareX | NT | NT | NT | Normal destination path blocked | NT | Worse | N | Weak; small dialog fix |
| Image thumbnails | ShareX/PowerToys resize | NT | NT | NT | Basic single-image fit | NT | Unverified | Maybe; no observed usability evidence | Solid provisional backend task; repair shared form |
| Video convert | ShareX/ScreenToGif | NT | NT | NT | GIF/WebM inaccessible | NT | Worse | N for GIF | Weak; format selector |
| Video thumbnails | ShareX | NT | NT | NT | Default timestamp/units wrong | NT | Worse | N today | Weak; reset task defaults |
| File hash | ShareX/HashCheck | NT | NT | NT | Core hashes; poor first-run default | NT | Unverified | Maybe after default fix | Solid provisional backend; interface needs correction |
| Folder index | ShareX | NT | NT | NT | Directory input blocked | NT | Worse | N | Weak; folder picker/form |
| Window inspector | ShareX | NT | NT | NT | Enumeration only | NT | Unverified | Maybe for developer diagnostics | Cut candidate from primary catalog; advanced group |
| Internal palette | PowerToys Run/Flow | NT | NT | NT | Different scope: app-local | NT | Unverified | N as OS launcher replacement | Solid provisional suite navigation |
| clean CLI | Reviewed manual cleanup | NT | NT | NT | Different scope; no matched comparison | Linux fixtures pass only | Unverified | Maybe for informed developers | Solid provisional, separate expert tool; fix ledger |

Absent upload, clipboard manager, duplicate finder, bulk renamer, system monitor and OS-wide search receive Missing for those requested benchmark tasks, not invented per-module test scores. No current Flagship award is supported. “Cut candidate” means reduce primary navigation prominence; do not delete a backend merely because it lacks current UI polish.

## G. Toolbox coherence score

| Dimension | Score 1–5 | Evidence |
| --- | --- | --- |
| Unified design language | 3 provisional | Shared Rune theme/buttons/headings; rendered consistency untested |
| Shared settings/hotkeys/history | 2 | Central preferences and shortcuts; no common result/history contract; F08/F14 |
| Cross-tool workflows | 1 | OCR handoff broken; upload/share unavailable; disk reveal absent |
| Discoverability without overwhelm | 2 | Task groups/palette help; 17 generic forms and misleading names burden novices |
| Resource footprint versus value | NT | No desktop resource or game measurements |
| Trust and safety perception | 1 provisional | S0 retention/indicator/recovery risks; native perception not studied |
| Whole greater than sum of parts | 2 provisional | Shared files/navigation, but transitions not dependable |

## H. Ten quick wins

Ranked by expected impact relative to implementation scope, not measured effort. None was implemented.

1. Wire existing quick-recording tray setter and verify minimized recording indication. F03.
2. Make Copy retention explicit before activation; decide independent clipboard behavior. F04.
3. Explain original pixels in Save editable and distinguish flattened sharing output. F05.
4. Handle Library/Settings tray routes and restore window. F06.
5. Correct modifier-aware video redo. F07.
6. Preserve preference-save failure instead of replacing it with success. F08.
7. Use folder pickers for Split and Folder index, with valid grid defaults. F10.
8. Clear File hash's default expected digest and label video thumbnail timestamp/width correctly. F11.
9. Give shortcut fields unique accessible action names. F19.
10. Correct README/Utilities instructions and disclose shown/total limits. F12/F13.

Dirty-image protection, retained failed captures, request ownership and durable recovery are higher-priority release work even where they are not quick wins.

## I. Strategic recommendations

Prioritize one dependable local capture workspace. First resolve S0 paths and establish Windows acceptance using generated media. Then complete capture→annotation→reviewed output and replay→trim→export before adding destinations. Preserve no-clobber output, existing video autosave, owned recording workers and trash journals.

Replace the generic utility form with small explicit task forms, not a new extensibility framework. Establish request/result identity first, then fix dialogs, modes/defaults and result actions. Merge QR read/create; place viewer/converter/beautify/effects around the image workspace; move window/clipboard inspection to advanced tools. Keep clean separate from novice disk analysis and expose its permanent/ignored-data policy clearly.

Make onboarding task-first: Capture a screenshot, Record something, Find disk usage. Show capability checks for NVIDIA/FFmpeg and the actual capture display/audio endpoints before starting. Expose the current data root and migration route. Reveal missing capabilities honestly instead of presenting dormant backend functionality as available.

Disk analysis should gain complete navigation, error details, accounting explanations and safe Reveal/Copy path before richer charts or deletion. Do not claim WizTree speed parity until measured with matched roots, privileges, filesystem and accounting definitions.

A default-choice claim requires measured evidence: Support Sam's frequent capture throughput; Gabe's replay/share latency and two-game FPS/1% low matrix; Chloe's output/audio reliability; Ada's accounting and recovery fixtures; Alex's keyboard/Narrator completion. No numerical target beyond the user's requested scenarios is invented here.

## J. Open questions for the product owner

1. Is the next release an experimental Rune preview or a daily-use replacement? Which single persona leads acceptance?
2. Is Copy intentionally persistent, and should that behavior be opt-in or explicitly named?
3. Should editable projects ever be a recommended sharing artifact? What secure-redaction promise is intended?
4. Which recording sources/devices and GPU generations are officially supported? Is the ten-minute screencast cap intentional product policy?
5. Which missing migration workflows are committed for the next build versus intentionally dropped?
6. Is there an available native Windows/NVIDIA test machine with disposable data and protected games for actual acceptance?
7. What telemetry/update-network/account policies should be verified against observed traffic?
8. Is permanent removal of ignored worktree data acceptable for the target audience, or should cleanup require narrower opt-in/quarantine?

These questions do not block the source findings; they determine implementation policy and native release gates.

## K. Untested areas and limits

No Windows app/installer was operated. Installation time/size, signature warnings, admin prompts, component selection, startup entries, uninstall leftovers, update signing/download/rollback/interruption and installed services remain untested. No screen-reader session, rendered contrast check, high-contrast theme, 150–200% scaling, RTL/IME/localization or actual focus traversal was performed.

No native screenshot/recording, second monitor, mixed DPI/HDR/negative coordinates, DRM/elevated window, fullscreen game, anti-cheat, controller binding, GPU reset, sleep/wake/RDP, physical input latency, webcam, microphone unplug, separate-track audition, 30-minute sync, output playback or GIF quality test was performed. No statement of anti-cheat safety or FPS neutrality is supported.

No network monitor, actual upload, external account/token configuration, clipboard interoperability with Slack/Discord/Teams/Office, OCR language/accuracy corpus, opaque-redaction pixel test, NAS/network/cloud drive, NTFS MFT correctness run, 1M/5M-file fixture, full disk, permission fault or destructive Windows cleanup exercise was performed. No matched competitor execution occurred. Linux tests do not prove Windows semantics.

No 1/4/8-hour resource, battery, thermal, leak or boot benchmark occurred. Cold test-build timing is not startup latency. No end-user task time, hesitation, success rate or ease score was measured. Consequently the full requested live-testing methodology remains incomplete; this report is not release certification.

Corrections made during synthesis: library search can find files beyond the 500 rendered matches; trash files are not deleted when Undo expires; disk UI does not delete; clipboard viewer is not a manager; keyboard Start here/End here trimming alternatives exist, so only slider/piece interaction remains disputed; saved editable originals do not prove flattened-export reversibility; current library filters and monitor-under-pointer region capture exist despite stale migration wording. Generic absence of old retired UI parity is not automatically a bug.

### Supporting evidence files

- [Suite A1–A12](qa-suite.md)
- [Capture and editing](qa-capture.md)
- [Utilities and cleanup](qa-utilities.md)
- [Safety and accessibility](qa-safety_accessibility.md)
- [Official benchmark research](qa-benchmarks.md)
- [Boat execution ledger](qa-vm_validation.md)
