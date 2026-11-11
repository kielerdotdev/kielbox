# Implemented safety review

Reviewed the integrated working-tree diff for F01/F02/F04/F05/F15/F16/F21 on 11 November 2026. Read-only source review, except for this requested report. No source edits, commits, builds, tests, remote execution, or subagents. Reproductions below are derived from traced code, not observed in a running application.

## Findings

### P1: Moving an image to Trash leaves its original-preserving recovery in the active Library

Location: `crates/desktop/src/rune_editor.rs:592-596`; caller integration at `crates/desktop/src/rune_ui.rs:1299-1304`.

The new recovery is a sibling named `<image>.rune-recovery.ssxe`. The Library trash action still passes only the selected media path to `recordings::trash`. Consequently, its editable recovery is neither moved nor included in the recovery journal. `recordings::scan_in` includes non-hidden `.ssxe` files at `recordings.rs:136-154`, so this original-preserving copy remains a normal active Library item after the image is trashed.

Reproduction: Open a synthetic PNG, cover private test text, leave the editor by opening another image, and retry after the recovery acknowledgement. Refresh Library, then trash the original PNG. Its recovery project remains outside `.trash`, still opens, and retains the original pixels and annotations. This differs from the intentionally reversible retention inside Trash. Reusing the original filename also makes `rune_editor.rs:129-132` automatically load the old recovery for the replacement image.

Fix: Treat the image and its associated recovery as one journaled trash operation. Apply this at the shared ownership boundary, retain collision-safe restore for both files, and avoid exposing internal recovery sidecars as unrelated captures. Account for an image still open in the editor so a later checkpoint cannot recreate an orphan after trashing.

Confidence: High. Recovery naming, scan admission, the single-path trash caller, and automatic recovery loading are explicit in the code.

### P1: One stale journal disables persistent recovery for unrelated trash batches

Location: `crates/desktop/src/recordings.rs:516-528`, also `493-507`; caller at `crates/desktop/src/rune_ui.rs:233-237`.

`recoverable_trash` aborts the whole enumeration when any recorded source parent no longer exists. It validates that parent before checking whether the trashed destination still exists. Completed restores retain their batch journal, so even a journal with no remaining recoverable files can trigger this error. The UI assigns its recovery list only on complete success; after restart the list stays empty and unrelated intact batches have no Restore controls.

Reproduction: Trash a file from `Library/nested`, restore it, then move the restored file elsewhere and remove the now-empty `nested` directory. Trash another capture from the Library root. Restart and open Trash. The old journal fails parent canonicalization, and the root capture is omitted even though its destination and journal are valid. An interrupted journal creation leaving a batch without `recovery.json` has the same global effect through line 493.

Fix: Inspect destination existence before requiring a source parent for an entry. Isolate errors per batch/entry, return valid recoverable items alongside diagnostics, and remove or retire completed journals. For a genuinely recoverable file whose original parent disappeared, provide a safe restore destination or recreate a validated parent rather than blocking other batches.

Confidence: High. The validation order, early returns, retained journals, and UI assignment behavior establish the failure without relying on timing or hostile filesystem changes.

## Other reviewed integration

- F01/F05: Traced ordered actor checkpoints and acknowledgements, revision tracking, replacement/close/quit gates, editable versus flattened save behavior, and original-pixel disclosures. The trash/recovery association above remains unresolved.
- F02/F04/F21: Traced region retry, clipboard-only delivery, Save elsewhere, overlay ownership, worker join, capture shutdown counter, normal close, tray Quit, and update/restart gates. Window/desktop screenshot siblings use app-owned file jobs. No further source-confirmed finding in these paths.
- F15: Traced immediate Undo and persistent per-file restoration, including no-clobber moves. Persistent enumeration has the failure above.
- F16: Traced CLI JSON output and cancellation exit status through the partial apply ledger. Ignored-worktree opt-in is carried through planning and apply-time revalidation; default worktree checks enumerate ignored files. No further source-confirmed finding in these changes.

This is not native Windows acceptance or a claim that asynchronous delivery, clipboard behavior, or crash durability passed execution tests. Tests run: none, as requested. Existing test source was inspected only.
