# Seek-proxy disk cache ownership audit — 2026-11-05

Scope: `crates/editor-core/src/scrub/proxy.rs` and the focused `proxy_cache_storage.rs` helper. The original recording, project, decoder implementation, export graph, and playback implementation are outside this change.

## Evidence and decision

The seek-proxy cache was already bounded. Before this pass it had a **3 GiB** cache budget, a **2 GiB** per-copy acceptance limit, a **2 GiB + 16 MiB** build reservation, an additional **512 MiB** free-space margin, LRU eviction, a global index lock, a single encoder lease, and shared leases protecting active readers. Replacing this with another generic cleaner would not address the actual defect.

The ownership test was weaker than the limits: cleanup trusted an `rcr-partial-` filename prefix or a 32-hex-character basename. `RCR_SEEK_CACHE_DIR` could point at a populated folder. A matching name alone could therefore authorize removal of an unrelated file. Publication could likewise try to replace an existing hash-named file without validated provenance.

The chosen fix retains the existing budgets and adds an explicitly managed namespace and ownership checks. It does not scan or clean existing user caches during implementation or validation.

## Changes

| Boundary | Behavior |
| --- | --- |
| Managed root | The configured cache base receives a `seek-v3-managed` child. The default is `%LOCALAPPDATA%/rcr/cache/seek-v3-managed`, with the existing temporary-directory fallback when LocalAppData is unavailable. |
| Root ownership | RCR must exclusively create the child and write/sync a schema ownership sentinel before using it. An existing child without a valid supported sentinel is refused; it is never silently adopted. |
| Path handling | Relative bases resolve against the startup working directory before ancestor checks. Symlink/reparse ancestors, roots, sentinels, and candidate files are rejected or skipped. Directory cleanup is nonrecursive. |
| Completed output | A disposable entry must have a supported ownership schema, an identity-derived basename, and the expected output length. Reuse still checks the complete source identity, including source path, size, timestamp, stream, and recipe. |
| Active readers | Existing shared file leases remain held until the native decoder closes. Eviction requires a nonblocking exclusive lease and cannot force-delete an active reader. |
| Builds | Temporary output and marker files receive their own synced, uniquely created ownership sidecars. Cleanup of abandoned partials requires those claims rather than a filename prefix. |
| Maintenance admission | Idle cleanup acquires both index and encoder locks nonblockingly. An active encoder prevents maintenance; starting a new build retains the existing reservation and encoder ownership. |
| Ambiguous files | Unknown files, invalid/missing claims, modified-size outputs, and legacy files are preserved. Recognizable but unowned occupancy counts against admission instead of being deleted to satisfy the budget. |
| Failure | Unsupported ownership, unavailable locks, full reservations, or storage errors preserve originals and fall back to the source preview path through the existing proxy failure behavior. |

The heavy filesystem work remains on the existing scrub/decoder worker. No new UI-thread cleanup, joins, timer, independent maintenance worker, or recursive filesystem traversal was introduced.

## Validation

All fixtures use temporary directories, synthetic bytes, and file locks. They do not capture devices, start FFmpeg, delete a user cache, or modify user media. The parent coordinates Cargo and native parity evaluations with `RCR_SEEK_CACHE_DIR` set to a new workspace fixture base.

New unconditional regressions cover:

- Creating and reusing a managed namespace while leaving legacy and foreign base files unchanged.
- Refusing preexisting unmarked, malformed, or unsupported-schema namespaces.
- Preserving unrelated hash/prefix-named files even when cache admission cannot fit its budget.
- Preserving active builds and partial-file readers until their leases end, then reclaiming a claimed abandoned partial.
- Preserving a changed output whose completion marker no longer matches its length.

The existing LRU, active-reader, reservation, source-identity, and incomplete-output tests remain applicable. The orphan test now distinguishes an independently claimed abandoned output from an ambiguous unpublished file that must remain untouched.

A separate reparse fixture creates directory/file symlinks when the Windows account permits it. This account returned `ERROR_PRIVILEGE_NOT_HELD` (1314), which Rust classified as an uncategorized I/O error. The fixture now explicitly treats that setup error as unavailable coverage; assertions after successful symlink creation remain unchanged. Actual reparse-fixture coverage is unavailable on this account, and a passing suite must not be presented as proof of that branch. The implementation uses Windows file attributes to reject reparse points, including junction-like entries that are not ordinary file symlinks.

Validation status: the central full-core rerun **passed: 120 tests, 8 ignored**. Namespace ownership, foreign-file preservation, changed-output provenance, active-reader and build-lease regressions all passed unconditionally. The reparse fixture remained unavailable because of Windows privilege 1314; its conditional skip does not add link-branch coverage. Five native AAC/two-PCM/audio-reference checks also passed with the cache isolated. `git diff --check` passed. The initial [core test log](../.downloads/plan-continuation/core-tests.log) preserves the earlier fixture-setup classification failure. The parent execution ledger tracks the final rerun and native evidence. Suggested focused command:

```powershell
$env:RCR_SEEK_CACHE_DIR = '<fresh isolated fixture base>'
cargo test -p rcr-editor-core scrub::proxy -j1
```

## Remaining limits and operational implications

- Legacy `seek-v2` and files directly in a configured old cache base are not migrated or removed. The new bounded namespace can coexist with the old roughly 3 GiB cache. Any manual cleanup must distinguish disposable cache artifacts from original media; this change performs none.
- The ownership protocol coordinates cooperating RCR processes. It is not an adversarial filesystem sandbox or a guarantee against a privileged process replacing paths during a filesystem operation.
- Ambiguous or damaged entries may consume the admission budget until reviewed manually. Preserving unknown files takes precedence over reclaiming space; source previews remain available.
- The byte cap does not bound arbitrary files another application places in the directory, every directory-entry scan cost, or aggregate disk usage across multiple cache versions. Existing per-copy and free-space checks remain important.
- Locking or filesystem behavior unsupported by a drive can disable proxy creation. This is a cache optimization failure, not permission to delete files or substitute lower-quality export media.
- Cache initialization and readers still use blocking filesystem operations on their existing worker. No new claim is made about latency on offline/network drives or slow antivirus filters.
- A crash between output publication and completion-marker publication can leave ambiguous output. It is counted but not automatically deleted without sufficient ownership evidence.

The integrated roadmap and current evidence gates remain in [the product improvement plan](PRODUCT_IMPROVEMENT_PLAN_2026-11-05.md).
