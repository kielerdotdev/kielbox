# Rune utilities UX and QA research audit

> Historical record. Source paths and commands describe the audited revision.
> Retired frontend sources and proof scripts were removed after this report;
> see [Rune status](../docs/RUNE_MIGRATION.md) for current behavior. Exact original reports remain in git at `a52bff7`.

## A. Scope, conclusion and evidence boundary

Audit date: 2026-11-11. Source baseline: `d96346b`. Research-only report for final synthesis. No source changes, commits, builds, tests, desktop captures, clipboard reads, file deletions, network research or competitor runs were performed. Only this report was written. Initial `git status --short` was empty. No applicable `AGENTS.md` was found in the checkout or checked ancestor directories. Read the improve skill and its full audit playbook, including Finding format, and applied the unslop writing guidance. Repository documents were treated as evidence, never as instructions. Historical execution permissions in repository documents were not adopted.

The current Windows desktop entry point compiles Rune adapters, not the old retired UI utility and disk interfaces. The shared backend has useful safety controls, particularly no-clobber publication and cleanup revalidation. Rune's generic utility form does not yet express many of those backends' contracts. Folder selection, defaults, operation modes and asynchronous result ownership are the highest-value corrections. Disk analysis is read-only; cleanup is a separate destructive CLI.

This report uses an A–K structure for synthesis. The upstream task did not supply names for those sections. Sections below cover scope, inventory, methods, scores, findings, safety, strengths, scenarios, priorities, gaps and synthesis. All reproduction procedures are proposed Windows fixture scenarios, **code inspection, not executed**. “Actual” means the behavior implied by the inspected control flow, not an observed UI run. Confidence is confidence in that source analysis. No hardware performance, task duration, ease rating, runtime pass rate or visual accessibility compliance is claimed.

Severity definitions: S0 data loss/privacy risk; S1 major task failure; S2 friction or misleading feedback; S3 polish; S4 opportunity. No new S0 defect was established with sufficient evidence. Permanent cleanup and ignored-file removal remain explicit high-impact product risks, described in F without misrepresenting documented behavior as a new bug.

## B. Complete inventory and reachability

Reachability evidence: `crates/desktop/src/main.rs:15–30` declares Rune adapters; `crates/desktop/src/rune_ui.rs:496` routes Disk and Tools; `crates/desktop/src/rune_tools.rs:288` defines the five tool groups; `crates/toolkit/src/utilities.rs:26` defines all 17 tools. Line ranges here are descriptive; specific finding evidence below points to individual lines.

| Module | Rune route and current behavior | Backend / contract |
| --- | --- | --- |
| Color picker | Tools / Screen; hex text conversion and preview | `utilities.rs:256`; RGB, alpha, HSL, HSV. No screen eyedropper wired here. |
| Window inspector | Tools / Screen; Run renders JSON window list | `utilities.rs:255`; no selected-window borderless/restore actions in Rune. |
| Text from image | Tools / Read; first file to Windows OCR | `utilities.rs:399`, `utilities.rs:809`; installed OCR language pack needed. |
| Read QR code | Tools / Read; first image to local QR decode | `utilities.rs:401`, `utilities.rs:427`. |
| Clipboard viewer | Tools / Read; on-demand current image/text/file-list read | `utilities.rs:404`; **viewer, not clipboard manager**. No capture history, persistence, search, pinning, automatic monitoring, clearing or multi-entry restore. |
| Image viewer | Tools / Images; first image preview and Save image | `utilities.rs:262`. Advertised copy and pin are absent from Rune. |
| Beautify | Tools / Images; first image, padding and radius | `utilities.rs:263`; generic labels incorrectly say Width/Height. |
| Effects | Tools / Images; grayscale only through current UI | `utilities.rs:296`; backend has 15 branches; mode remains zero. |
| Combine | Tools / Images; ordered file list; vertical combine | `utilities.rs:342`; horizontal backend exists, no mode control. Width field means gap, capped at 1024. |
| Split | Tools / Images; first image; a/b mean columns/rows | `utilities.rs:349`; output must be existing directory; unique child folder. Rune supplies save-file picker. |
| Convert | Tools / Images; preview, then Save image | `utilities.rs:262`, `utilities.rs:697`; extension selects encoding, Rune advertises PNG filter only. |
| Thumbnails | Tools / Images; first image; bounded fit | `utilities.rs:393`; multiple inputs are accepted by UI but only first is used. |
| Convert video | Tools / Video; first file, chosen output, MP4 only | `utilities.rs:398`, `utilities.rs:796`; WebM/GIF backend modes inaccessible. FFmpeg required. |
| Video thumbnails | Tools / Video; first file; a = integer seconds, b = max width | `utilities.rs:779`; Width/Height labels hide the contract, default timestamp 1024 seconds. |
| Make QR code | Tools / Files; text to generated QR image | `utilities.rs:400`. |
| File hash | Tools / Files; stream every chosen file; optional expected digest | `utilities.rs:402`, `utilities.rs:503`; six hashes. Initial text is invalid expected hash. |
| Folder index | Tools / Files; backend requires first file-list path be directory | `utilities.rs:403`, `utilities.rs:589`; text/HTML/JSON/XML backend, text mode only in Rune, no folder picker. |
| Disk usage | Tools / Files; folder picker, background scan, one-level proportional blocks, largest list, descend/up | `rune_tools.rs:728`; `disk::disk_usage::scan`, NTFS attempt and directory fallback. No file deletion actions. |
| clean CLI | Separate `clean.exe`; not a Rune utility page | `crates/clean/src/main.rs:8`; preview default, `--apply`, age/kind/root filters and JSON. Uses `disk::cleanup`. |

Dormant or missing from shipped Rune, not runtime-tested modules: old retired UI `crates/desktop/src/utilities.rs`, `screen_tools.rs`, `disk_usage.rs`, `disk_explore.rs`, `disk_treemap.rs`, `disk_sunburst.rs` and related proof files. `main.rs` does not declare these modules. These contain or document screen color sampling, ruler, independent pin windows, monitor test, dedicated OCR/QR region handoff, borderless/restore UI, copy/save-text/reveal actions, rich disk views, file-type filtering and native icons. Existing backend or old code is not evidence that Rune exposes an operation. `docs/RUNE_MIGRATION.md` explicitly calls out missing utility-specific forms and screen tools. Clipboard history is also explicitly pending in `docs/UTILITIES.md`; it must not be called a shipped manager.

The generic “Capture region” button is reachable on all utility pages, but launches the screenshot workflow. It does not populate utility inputs. See UTIL-05. The separate image annotation editor and capture/recording workflows are adjacent integrations, not fully audited here.

## C. Method, personas and heuristics

Method: static trace from entry point to Rune routes, state, dialogs, request construction, utility dispatch, output publication, disk scanning and cleanup. Read existing targeted tests to identify intended safeguards and gaps; did not execute them. Read README, workspace and desktop manifests, utility and cleanup documentation, Rune migration status, product improvement ledger and verification script. Checked source references directly. No public pages were needed for repository findings.

Personas used as analytical lenses, not recruited participants:

- P1 cautious Windows user converting or splitting personal images, expects originals preserved and output locations explicit.
- P2 developer cleaning old agent worktrees, needs protection for work and a trustworthy machine-readable removal ledger.
- P3 support/power user finding large files, needs scan completeness, full paths and an actionable handoff to Explorer.
- P4 occasional utility user reading QR/OCR, generating a QR or verifying a download hash, expects task-specific defaults.
- P5 keyboard or assistive-technology user, needs truthful labels, reachable actions and understandable result status. Native behavior remains untested.

Heuristics assessed: task/label match, prevention of errors, progress visibility, user control and recovery, recognition over recall, and consistency between advertised and reachable behavior. No persona timing or subjective ease scores were collected.

## D. Per-module heuristic scores

These are **source-based heuristic assessments, not usability-test results**. No module was run. Ordinal rubric: 0 = primary contract blocked through intended form; 1 = substantial mismatch/inaccessible operation; 2 = viable core with material friction; 3 = coherent basic path with minor or unverified gaps; 4 = fully evidenced task fit and recovery. Three dimensions: T task fit, F feedback/visibility, R prevention/recovery. Do not average these as a measured product score. Shared state defect UTIL-04 affects every utility, so R is conservative. Windows native dialogs and assistive technology could change the assessment after execution.

| Module inspected | T / F / R | Reason and evidence |
| --- | --- | --- |
| Color picker | 2 / 2 / 2 | Valid initial hex conversion; screen picking not present; generic irrelevant file/dimension fields. `rune_tools.rs:35`, `utilities.rs:256`. |
| Window inspector | 2 / 1 / 2 | Enumeration JSON only, no structured selection or border actions. `utilities.rs:255`. |
| OCR | 2 / 2 / 2 | File path viable, dependency errors surfaced; capture handoff missing. `utilities.rs:399`, `rune_ui.rs:361`. |
| QR decode | 2 / 2 / 2 | Local file path and no-code error; capture handoff missing. `utilities.rs:401`, `utilities.rs:443`. |
| Clipboard viewer | 2 / 2 / 2 | Explicit on-demand read; no history claim justified; current result persists across edits. `utilities.rs:404`. |
| Image viewer | 2 / 2 / 2 | Preview/save path, copy/pin missing despite description. `utilities.rs:91`, `rune_tools.rs:658`. |
| Beautify | 1 / 2 / 2 | Dimensions mean padding/radius; defaults clamp to 512/256. `utilities.rs:265`. |
| Effects | 1 / 2 / 2 | Only grayscale reachable; modes absent. `utilities.rs:298`, `rune_tools.rs:39`. |
| Combine | 1 / 2 / 2 | Vertical only, gap mislabeled; no reorder controls beyond remove/re-add. `utilities.rs:342`, `rune_tools.rs:487`. |
| Split | 0 / 2 / 2 | File dialog cannot select required directory; grid defaults usually invalid. `utilities.rs:353`, `rune_tools.rs:570`. |
| Thumbnails | 2 / 2 / 2 | Correct bounded aspect fit backend; first input only, no UI explanation. `utilities.rs:393`. |
| Convert image | 2 / 2 / 3 | Safe extension-driven save; only PNG filter advertised, separate Output chooser unused. `utilities.rs:697`, `rune_tools.rs:664`. |
| Convert video | 1 / 2 / 3 | MP4 only reachable; no-clobber final output. `utilities.rs:796`, `utilities.rs:750`. |
| Video thumbnails | 1 / 2 / 2 | Default seeks to 1024 seconds, fields mislabeled. `rune_tools.rs:36`, `utilities.rs:780`. |
| QR encode | 2 / 2 / 2 | Text path viable; generic form adds unrelated controls. `utilities.rs:400`. |
| File hash | 1 / 2 / 2 | Default color string fails expected-digest validation; streaming backend sensible. `utilities.rs:507`. |
| Folder index | 0 / 2 / 2 | Text folder ignored; no proper directory input; formats inaccessible. `utilities.rs:403`. |
| Disk analyzer | 2 / 1 / 3 | Read-only, cancel and drill-down; silently caps list and hides error details/backend. `rune_tools.rs:880`. |
| clean CLI | 3 / 2 / 2 | Good dry-run/revalidation; cancellation loses accumulated JSON outcome. `cleanup.rs:398`, `main.rs:179`. |

## E. Vetted findings

Every finding below uses method **code inspection, scenario not executed**. Effort S/M/L estimates describe implementation scope including focused verification, not measured task durations. Risk is risk of the proposed fix. No finding is asserted introduced by this branch; this is an audit of the current baseline.

### [UTIL-01] S1 UX: supply a real folder input for Folder index

- Evidence: `crates/toolkit/src/utilities.rs:403` calls `index(first()?, ...)`, ignoring text. `crates/desktop/src/rune_tools.rs:445` uses `open_file`; `rune_tools.rs:520` labels text “Text, URL, timestamp or folder”. `utilities.rs:590` requires a directory.
- Personas/impact: P3/P4 cannot complete the named task through the intended inputs; pasted folder path does nothing.
- Repro: 1. Start with no files. 2. Open Tools / Folder index. 3. Paste a fixture folder path in Text. 4. Run. 5. Add an ordinary fixture file and Run again.
- Expected: select/paste folder, choose depth/format and generate index. Actual by inspection: first run “Choose an input file”; second run “Choose a directory”. Native file picker acceptance of typed directory paths was not tested and is not a designed folder workflow.
- Fix sketch: per-tool directory picker and validated folder text; build request from that input. Label depth and allow text/HTML/JSON/XML. Hide irrelevant image fields.
- Effort M; risk LOW, scoped request/UI contract; confidence HIGH.

### [UTIL-02] S1 UX: choose a directory for Split and initialize valid grid values

- Evidence: `crates/toolkit/src/utilities.rs:361` requires an existing output directory; `crates/desktop/src/rune_tools.rs:570` calls `save_file` for every tool. `rune_tools.rs:36` defaults a/b to 1024/768; `utilities.rs:353` caps grid at 1000 cells.
- Personas/impact: P1 cannot follow the displayed output workflow; defaults alone request 786432 cells. Destination validation is safe, but the main task is blocked.
- Repro: 1. Add a synthetic 7×5 PNG. 2. Open Split. 3. Set Width 3 and Height 2. 4. Choose output and provide a new filename in a writable fixture directory. 5. Run.
- Expected: choose existing parent directory, get six PNG tiles in a unique subfolder. Actual by inspection: “Output directory does not exist”, since chosen path is a file destination. With untouched dimensions, grid validation fails first.
- Fix sketch: use `open_folder` for split output, call fields Columns/Rows, set conservative valid defaults, describe unique child folder. Preserve temporary-folder cleanup and edge coverage.
- Effort S; risk LOW; confidence HIGH.

### [UTIL-03] S1 UX: expose utility modes and initialize each tool's fields

- Evidence: `crates/desktop/src/rune_tools.rs:39` initializes mode zero; `rune_tools.rs:73` forwards it; no other mode assignment exists in that adapter. `crates/toolkit/src/utilities.rs:298` selects effects by mode, `utilities.rs:344` combine orientation, `utilities.rs:630` index format and `utilities.rs:796` video format. `rune_tools.rs:331` changes tool without changing inputs/defaults; `utilities.rs:507` validates expected hashes.
- Personas/impact: P1/P4 cannot choose sepia/blur/etc., horizontal combine, WebM/GIF or structured index formats. First File hash run fails on the initial `#3ecf8e` text. Video thumbnail field labeled Width seeks 1024 seconds; Height actually controls output width. Beautify Width/Height mean padding/radius and silently clamp defaults. These are one shared form-contract defect, not separate backend bugs.
- Repro: 1. Open Effects with a fixture image and try choosing Invert. 2. Open Convert video and choose a `.webm` destination. 3. Fresh state: open File hash, add a fixture file and Run. 4. Fresh state: open Video thumbnails with a short synthetic clip and inspect fields before Run.
- Expected: appropriate options, units and defaults for each task. Actual by inspection: no effect or output-format selector, `.webm` rejected as requiring `.mp4`, hash validation fails until text is cleared, timestamp defaults far beyond a short clip.
- Fix sketch: small explicit forms driven by tool contracts, per-tool state/defaults and relevant validation. Migration status already acknowledges missing forms; this finding describes current task impact, not an undisclosed migration failure.
- Effort M; risk MED, several request mappings; confidence HIGH.

### [UTIL-04] S1 bug: bind asynchronous results and success state to the initiating request

- Evidence: `crates/desktop/src/rune_tools.rs:331` switches tools and clears result without cancelling/binding a live job; `rune_tools.rs:123` unconditionally assigns any returned output to current shared state. `rune_tools.rs:138` changes message on error without clearing prior result; `rune_tools.rs:588` displays Done whenever a result exists. `rune_tools.rs:658` offers Save image from that retained result.
- Personas/impact: P1/P4 can mistake another tool's late result or an earlier successful image for current success, then save the wrong image. No overwrite is implied, but output identity is unreliable.
- Repro A: 1. Start a deliberately slow synthetic video conversion or image job. 2. Return to Tools. 3. Open another utility before completion. 4. Wait for old completion. Repro B: 1. Run a successful image operation. 2. Change its input to an invalid fixture. 3. Run again and wait for failure.
- Expected: result identified by originating tool/request; failure does not present prior output as current success. Actual by inspection: A completion appears in the newly selected tool; B error text coexists with previous preview, Done and Save image.
- Fix sketch: carry tool/request generation in worker completion; keep per-tool results or reject stale completion. Represent busy/success/failure explicitly and invalidate or clearly mark prior output on edits/start/failure. Guard deferred save dialogs by result identity.
- Effort M; risk MED, state and dialog lifecycle; confidence HIGH.

### [UTIL-05] S1 bug: route utility region captures back to OCR/QR inputs

- Evidence: `crates/desktop/src/rune_tools.rs:462` shows Capture region for the shared form; `rune_tools.rs:470` invokes generic screenshot. `rune_tools.rs:980` starts Rune screenshot overlay; fallback at `rune_tools.rs:994` returns `JobResult::Capture`. `crates/desktop/src/rune_ui.rs:361` refreshes and opens capture; only file picker callback at `rune_tools.rs:449` adds utility files.
- Personas/impact: P4 choosing OCR or QR capture enters screenshot/annotation behavior without transferring the selected image into the tool. Potentially private captured content can be saved through screenshot controls, but this audit does not establish unauthorized persistence merely by clicking Capture.
- Repro: 1. Open Text from image with no files. 2. Capture region containing synthetic text. 3. Complete overlay using its save/open action. 4. Return to OCR and Run.
- Expected: selected region becomes OCR input or returns extracted text. Actual by inspection: generic screenshot operation, no utility request handoff; OCR still has no chosen input unless user manually adds the saved screenshot. Native overlay action paths not executed.
- Fix sketch: typed capture purpose and ephemeral frame handoff for utility jobs; keep normal screenshot actions explicit and separate. Until wired, omit this affordance from utility forms rather than imply direct extraction.
- Effort M; risk MED, screenshot integration and privacy expectations; confidence HIGH on missing handoff, MED on exact native navigation.

### [CLEAN-01] S1 bug: retain the deletion ledger when cancellation interrupts apply

- Evidence: `crates/disk/src/cleanup.rs:396` keeps outcome locally; `cleanup.rs:398` returns Interrupted at next candidate after prior deletions; `cleanup.rs:432` accumulates removed candidates. `crates/clean/src/main.rs:112` propagates apply error before JSON emission; `main.rs:179` emits only success/error on failure.
- Personas/impact: P2 scripts cannot know which directories were permanently removed when interrupted. Non-JSON mode prints per-removal lines, but JSON mode deliberately suppresses them. Exit 3 is useful yet does not preserve accounting.
- Repro: 1. In an isolated fixture only, prepare two independently eligible aged artifact directories outside the command's current directory. 2. Invoke `clean <fixture-root> --apply --json`. 3. Inject cancellation after the first removal callback and before the second candidate's initial check. 4. Inspect final stdout and disk state. Use a deterministic harness for the timing boundary.
- Expected: cancelled report with first removed path, unattempted paths and exit 3. Actual by inspection: first directory is gone but final JSON has only success=false and error, discarding outcome. No destructive scenario was executed.
- Fix sketch: return cancellation as structured outcome retaining completed removals/errors, or include partial outcome in the error type. Emit one consistent final JSON schema with cancelled/applied flags and remaining candidates; retain exit 3.
- Effort M; risk MED, public outcome/schema behavior; confidence HIGH.

### [DISK-01] S2 UX: disclose the 500-item limit and provide a path to hidden children

- Evidence: `crates/desktop/src/rune_tools.rs:880` takes only 500 sorted children for Largest. `rune_tools.rs:815` shows eight map entries; `rune_tools.rs:859` groups rest into an inert block. `rune_tools.rs:897` only descends directory rows, and no child search/page control is rendered.
- Personas/impact: P3 cannot reach the 501st child directory from this scan even though it exists in the backend tree. Users may infer a complete directory listing. This is not backend truncation.
- Repro: 1. Prepare a fixture with 501 child directories and descending nonzero contents. 2. Scan parent. 3. Try locating/opening the smallest child through Largest and map.
- Expected: all children reachable via paging/search/virtualization, or explicit partial-list label and alternate navigation. Actual by inspection: child omitted from list and absorbed into noninteractive rest block.
- Fix sketch: label shown/total, add bounded paging or virtualization and folder search. Preserve proportional grouped area.
- Effort M; risk LOW; confidence HIGH.

### [DISK-02] S2 UX: expose scan errors, backend and size meaning alongside results

- Evidence: `crates/disk/src/disk_usage.rs:70` retains method, fallback_reason and error samples; `disk_usage.rs:97` records failed paths. `crates/desktop/src/rune_tools.rs:203` reports only file/byte/error totals. `rune_tools.rs:779` explains logical sizes only in the empty-state branch; populated page starts at `rune_tools.rs:783`.
- Personas/impact: P3 sees “errors” without affected paths or recovery guidance. They cannot distinguish permission omission, skipped offline data, directory fallback or allocated-space differences when comparing results to drive properties.
- Repro: 1. Scan an owned fixture with an intentionally inaccessible child under an appropriately scoped Windows test account. 2. Inspect completion and try revealing the failure reason. 3. Compare a sparse/hard-linked fixture's result to allocated size.
- Expected: persistent logical-size label, partial-result notice, copyable sampled failures and actual backend/fallback explanation. Actual by inspection: aggregate count only; explanation disappears when results exist. Exact Windows permission behavior unexecuted.
- Fix sketch: small scan-summary/details area backed by existing error samples and metadata; state that errors/skips make totals incomplete and hard links count by path. Do not suggest elevation without explaining its purpose.
- Effort S; risk LOW; confidence HIGH.

### [UTIL-06] S2 UX: make result actions match the advertised operation

- Evidence: `crates/toolkit/src/utilities.rs:91` advertises copy and pin; `utilities.rs:105` advertises four image save formats. `crates/desktop/src/rune_tools.rs:643` builds only Run, Cancel and optional Save image; `rune_tools.rs:664` advertises PNG filter only. Text is selectable at `rune_tools.rs:630`, but no text save/copy button or generated-file reveal uses `Output.files`.
- Personas/impact: P1/P4 can manually select text but lack advertised direct copying, pinning and saving text/index reports. “Choose output” appears on image tools yet `Tool::Convert` never uses that path, inviting a false assumption that Run saves there. Generated split/video outputs need manual navigation.
- Repro: 1. Run Color or Image viewer and seek advertised Copy/Pin. 2. Choose Output for Convert and Run, then inspect destination. 3. Run an OCR/hash operation and seek Save text. 4. Try selecting an advertised JPEG/WebP/BMP save format in Save image.
- Expected: visible supported actions and only meaningful destination controls. Actual by inspection: image preview plus PNG-filtered separate Save image; chosen Output ignored for image conversion; no direct text-save/copy/pin/reveal. Manual extension entry may still save non-PNG formats; native dialog behavior was not tested, so those formats are not called impossible.
- Fix sketch: align descriptions with shipped capabilities immediately; provide explicit save/copy/open-location actions and format choices where warranted. Keep clipboard writes user-triggered and maintain no-clobber semantics.
- Effort M; risk LOW to MED, clipboard and save-dialog behavior; confidence HIGH.

### [DOC-01] S2 UX: distinguish legacy utility instructions from Rune instructions

- Evidence: `docs/UTILITIES.md:12` promises Tree/File views, Reveal/Copy path and detailed navigation; `docs/UTILITIES.md:24` references retired UI verification. `crates/desktop/src/rune_tools.rs:728` renders current smaller disk view; `rune_tools.rs:643` lacks promised utility result actions. `docs/RUNE_MIGRATION.md:5` explicitly says parity is incomplete.
- Personas/impact: P1/P3/P4 follow linked README documentation toward controls absent from the installed frontend. Migration disclaimer does not make individual task instructions executable.
- Repro: 1. Read README's Utilities link. 2. Follow disk instructions to choose File View and Reveal. 3. Follow utility copy/pin/save-text instructions in Rune.
- Expected: current frontend instructions, clearly marked legacy capabilities and pending work. Actual by inspection: guide describes dormant retired UI interfaces as current behavior.
- Fix sketch: make Rune instructions primary, label archived retired UI sections and link migration limits beside affected tasks. Correct capability statements without implying all deferred functionality must be implemented immediately.
- Effort S; risk LOW; confidence HIGH.

## F. Deletion, overwrite and privacy review

### Confirmed safeguards

- Rune disk view is read-only. Navigation and scan callbacks contain no cleanup/delete operations. `rune_tools.rs:728`, `rune_tools.rs:897`.
- `clean` preview is the default, `--apply` is explicit and conflicts with `--dry-run`. Help states permanent deletion, modification-time age and stale local remote-ref semantics. `crates/clean/src/main.rs:11`, `main.rs:25`.
- Cleanup classifies narrowly named artifacts and linked worktrees. Main repositories, current-command directory ancestors, links/junctions/offline entries, dirty/locked/submodule/unpublished worktrees and unreadable targets have preservation checks. Revalidation precedes every removal. `crates/disk/src/cleanup.rs:158`, `cleanup.rs:174`, `cleanup.rs:209`, `cleanup.rs:273`, `cleanup.rs:400`.
- File publication uses temporary siblings and `persist_noclobber`; video is encoded in a temporary directory, FFmpeg uses `-n`, and final publication is also no-clobber. `crates/toolkit/src/utilities.rs:687`, `utilities.rs:710`, `utilities.rs:750`, `utilities.rs:771`. Reusing an input path should fail without replacing it. Existing unit coverage explicitly asserts image non-overwrite at `utilities.rs:904`, but was not run.
- Split uses a unique temporary subdirectory and retains it only after finishing. Scope drop cleans failed/cancelled runs before keep. Edge division covers odd-sized inputs. `utilities.rs:363`, `utilities.rs:370`, `utilities.rs:388`.
- Clipboard is read only after explicit Run; no manager/history database was identified in this workflow. Local OCR/QR/hash/index processing does not upload output. `utilities.rs:399–421`. Window enumeration can display sensitive window titles; do not collect real title/clipboard contents during QA.

### High-impact documented risks, not new defects

`clean` permanently deletes eligible linked worktrees including ignored files. Git cleanliness excludes ignored entries; age checks do not prove files are disposable. An old ignored local settings file or local dataset can therefore be removed with an otherwise eligible worktree. This behavior is explicitly documented at `crates/clean/README.md:50`, and no-force Git removal is intentional at `cleanup.rs:416`. Do not report it as accidental removal of tracked changes. A future S4 safety-policy option is explicit worktree opt-in and a preflight ignored-content summary, with conservative exclusion or quarantine for non-regenerable ignored material. This requires a product decision, not silently changing current documented semantics. Before deployment to non-developers, assess whether the help-level “permanent” warning sufficiently conveys this S0-class potential consequence.

Artifact classification trusts conventional names and adjacent manifests; it does not prove every file under `target` or `node_modules` can be recreated. README warns modification age is not activity and tells users to stop agents/builds. Validate/read/delete has residual concurrent mutation windows; no directory lock or handle-based immutable snapshot was established. This limitation is already documented at `crates/clean/README.md:61`, so no speculative race exploit is presented as a proven S0 bug.

Cancellation is cooperative. An ongoing filesystem/Git removal may finish before cancellation is seen. A failed directory removal may be partial. README explains both at lines 65–67. Distinguish this limitation from the fixable lost-ledger bug CLEAN-01.

Saving an image to an existing filename is deliberately refused even if the native save dialog offers a replace confirmation. Preserve this safety policy and explain “choose a new filename”; do not “fix” it by allowing silent overwrite. Exact dialog replacement prompts were not observed.

## G. Strengths with source evidence

1. Shared disk backend separates reusable scan/cleanup logic from UI, and supplies counts, errors and partial cancellation results. `crates/disk/src/disk_usage.rs:125`, `crates/disk/src/cleanup.rs:389`.
2. Utility work runs off the UI thread with a bounded two-thread raster pool and cancellable process runner. `crates/desktop/src/rune_tools.rs:82`, `crates/toolkit/src/utilities.rs:17`, `crates/toolkit-services/src/command.rs:173`. This supports responsiveness but is not measured responsiveness evidence.
3. Input/output bounds are explicit: image dimensions and file bytes, combined decoded bytes, split cell cap, index entry/depth caps. `utilities.rs:181`, `utilities.rs:193`, `utilities.rs:470`, `utilities.rs:353`, `utilities.rs:609`.
4. Hashing streams chunks, supports common digests and provides explicit MATCH/MISMATCH. `utilities.rs:503`. HTML/XML index paths are escaped at `utilities.rs:680`. Native process argument vectors avoid shell command composition at `utilities.rs:737`.
5. Rune tools are grouped by task, search is case-insensitive, list links have role/label/focus attributes, Cancel is available during work, and text results are selectable. `rune_tools.rs:288`, `rune_tools.rs:354`, `rune_tools.rs:377`, `rune_tools.rs:630`, `rune_tools.rs:653`. Native keyboard/screen-reader effectiveness is still unverified.
6. Targeted existing tests address hash vectors/input preservation, QR round-trip, split edge coverage/no-clobber, escaped index/depth/cancel, FFmpeg track mapping and bounded threads. `utilities.rs:858`, `utilities.rs:876`, `utilities.rs:894`, `utilities.rs:920`, `utilities.rs:933`. Cleanup tests cover revalidation, roots, age, dirty/locked/unpublished worktrees and package-link external-target preservation. `crates/disk/src/cleanup/tests.rs:37`, `tests.rs:56`, `tests.rs:145`, `tests.rs:204`. These are coverage observations, not new test results.

## H. Scenario inventory and verification handoff

All listed checks are unexecuted. Use generated, owned fixture files and synthetic text only; never use real clipboard secrets, window titles, account settings or personal worktrees. No local compilation is authorized. Only the boat VM agent may perform remote Rust compilation. These commands are a handoff, not a request to execute in this research pass.

| Area | Owned-fixture acceptance scenarios |
| --- | --- |
| All 17 tools | Launch from Tools group/search; cancel; error then success; success then error; change tool during worker; stale save dialog; keyboard traversal; irrelevant inputs absent. |
| Color | Six/eight-digit hex, invalid ASCII/non-ASCII, transparent colors; verify numeric conversions with reference vectors. |
| Images | PNG/JPEG/WebP/BMP decode/encode, transparent edges, 32MP boundaries, unsupported files, missing file, odd dimensions; verify source bytes unchanged. |
| Beautify/effects/combine | Correct labels and parameter units, each effect/mode, one/many inputs, order, gap, horizontal/vertical geometry, bound failures. |
| Split | 7×5 / 3×2 exact coverage, folder selection, unique folder, cancellation cleanup, unwritable/disk-full output and valid defaults. |
| Video | Synthetic short clips, MP4/WebM/GIF choices, two audio tracks, no audio, odd dimensions, timestamp before/after duration, missing FFmpeg, cancellation and output-created-during-encode collision. |
| OCR/QR | Generated text/QR, no recognition, missing language pack, transparent/high-resolution input, multiple QR codes, ephemeral region handoff and cancellation. |
| Hash | Known vectors and optional expected digest blank/match/mismatch, multiple files, unreadable input and cancellation; explicitly clear inappropriate default text. |
| Index | Folder input, depths 0/1/64, hidden and junction entries, long/Unicode names, escaped markup, 100k/depth cap, formats and save complete output. |
| Clipboard viewer | Isolated synthetic text/image/file-list/empty contents; read only on demand; no history assertions; do not overwrite a user's clipboard to run tests. |
| Window inspector | Owned synthetic windows only, list/title/geometry and elevated refusal where applicable; borderless is dormant in Rune, not a current acceptance target. |
| Disk | Empty folder, zero-byte entries, 501 children, permissions, cancelled partial scans, deep trees, links/junctions/offline/sparse/hard-link files, scan A then B, complete vs partial totals, show backend and failure details. |
| clean | Preview leaves bytes unchanged; explicit apply only on disposable fixtures; wrong roots/age/kinds, overlapping roots, worktree preservation, ignored-content policy, mutation between plan/apply, cancellation after one deletion, partial deletion failures, JSON schema/exit codes. |
| Output publication | Existing destination, same-as-input path, destination collision during job, denied writes and disk full. Assert old destination hash unchanged and no published partial final output. |

Suggested remote gates for the owning boat VM agent, after any eventual implementation: `cargo test --locked -p disk -p clean -j1`; `cargo test --locked -p toolkit utilities::tests -j1`; `cargo test --locked -p desktop -j1`; Windows `./scripts/verify-rune.ps1 -SkipBuild` only against a separately built identified executable. The script tests scene structure/rendering, not all workflows. Inspect Windows dialogs and action/result routing separately. Compilation and platform setup belong to the authorized remote owner; the researcher did not run them.

## I. Priorities and independent fix boundaries

| Order | Work | Why / dependency |
| --- | --- | --- |
| 1 | CLEAN-01 partial cancellation ledger | Destructive operations need trustworthy accounting; preserve current safety checks and explicit apply. |
| 2 | UTIL-04 request ownership | Establish truthful result identity before adding more utility controls/actions. Add deterministic delayed-completion scenarios. |
| 3 | UTIL-01/02/03 per-tool forms and defaults | Restore core tasks; changes should be explicit enum contracts, not a large speculative form framework. Backend capability exists. |
| 4 | UTIL-05 typed region handoff | Build on request identity and ephemeral inputs. Avoid implicit persistent copies as a workaround. |
| 5 | UTIL-06 result actions | Bind clipboard/save/reveal actions to exact result, preserve publication safety. |
| 6 | DISK-01/02 reachable complete navigation and transparent totals | Reuse backend metadata; do not introduce deletion into disk analysis as part of this fix. |
| 7 | DOC-01 current instructions | Small independent update, should ship alongside corrected capability descriptions. |

Do not rebuild the dormant retired UI frontend, implement clipboard history, add cloud integrations or rewrite scanning to resolve these findings. S4 options are a conservative worktree-cleanup policy review and restoring selected dormant screen tools only after core utility forms and result ownership are trustworthy. No benchmark-based choice between scanners is justified by this pass.

## J. Untested areas and considered/rejected findings

Untested: executable build/runtime, Windows dialogs and permissions, NTFS raw-volume records and fallback correctness, Wine/native differences, actual file deletion semantics under all Git versions, TOCTOU mutation races, read-only/network/cloud drives, disk-full publication failures, native OCR language/WinRT behavior, FFmpeg codecs, HDR/DPI/multiple displays, screen-reader announcements, focus order, text clipping/contrast, rendering at small window sizes and worker shutdown latency. No dependency advisory scan or repository-wide secret audit was performed. Files with possible sensitive content were not dumped into this report.

Considered and rejected or bounded:

- “Image/video saves overwrite originals”: rejected; explicit no-clobber helpers and temporary outputs are present. Fault/collision testing remains necessary.
- “Disk analyzer deletes files”: rejected; cleanup is separate CLI and disk callbacks only scan/navigate.
- “Cleanup deletes dirty/unpublished worktrees”: rejected as a blanket claim; explicit checks exist. Ignored data is a documented separate policy risk.
- “Age proves inactivity”: rejected; README already warns otherwise. No-force Git and narrow artifact names reduce risk but cannot prove disposability.
- “No confirmation prompt is a defect”: rejected; explicit `--apply` and noninteractive operation are documented design choices. Improve disclosure rather than adding arbitrary prompts to scripts.
- “All advertised retired UI features are live”: rejected; entry point and migration document show otherwise.
- “Clipboard manager is broken”: rejected; only a current-value viewer is shipped, persistent history is pending.
- “All long disk lists are slow”: not established; rendering is capped. The confirmed issue is inaccessible omitted children, not a measured frame-rate problem.
- “Native JPEG/WebP/BMP saving impossible”: not established; extension-driven backend exists and manual path entry may work. The confirmed gap is discoverability and picker format choices.
- “Need another treemap/parallel scanner rewrite”: not established by static analysis. Existing read-only scan and navigation are a credible base.

## K. Research conclusion for synthesis

The strongest evidenced issues are request/result ownership, unusable folder/output contracts, unreachable operation modes and lost cleanup accounting after cancellation. They need small, focused interface and state corrections while retaining existing no-clobber and deletion-preservation checks. Disk analysis should disclose incomplete scans and provide access to every scanned child before additional visual modes are prioritized. Clipboard is a viewer; dormant retired UI tools and backend capabilities must not inflate the current Rune inventory.

This is a source audit with reproducible proposed scenarios, not an executed Windows QA certificate. No S0 defect, measured speed improvement, task time, ease rating, competitor comparison or native accessibility pass is claimed.
