# Cleanup implementation

Implemented F16 in `crates/disk/src/cleanup.rs` and `crates/clean/src/main.rs`.
Apply cancellation returns the completed deletion/error ledger with
`Outcome::cancelled = true`; CLI JSON retains `removed` and `remove_errors`, reports
`cancelled: true` and `success: false`, and returns exit 3. Cancellation during
planning validation propagates as interruption rather than a preservation notice.

Worktrees with ignored files are now preserved by default. Explicit
`--allow-ignored-worktree-data` permits permanent deletion of those files, including
non-regenerable local settings/secrets/datasets. The same check runs in preview
and pre-delete validation. Existing no-force Git removal, dirty/untracked, age,
lock, submodule, remote-tracking, scope and current-directory guards remain.
Help and `crates/clean/README.md` describe the policy and cancellation report.

Integration API: `cleanup::Options` gains required boolean
`allow_ignored_worktree_data`, normally false; `Plan` retains that policy privately.
`cleanup::apply` keeps `io::Result<Outcome>` but cancellation now returns `Ok` with
`Outcome::cancelled` instead of discarding the ledger in an interrupted error.
Callers must inspect that flag. All current callers were updated in owned files.

Added deterministic disk and CLI unit regressions that cancel from the successful
removal callback, assert one removed path and one surviving target, and check
JSON/exit 3. Added ignored-data preview/revalidation/opt-in and dirty-worktree
coverage, and adjusted the pre-cancelled apply regression to the new API.

Validation: rustfmt completed and scoped `git diff --check` passed. No local Rust
compilation, Cargo build or tests were run. Boat must run
`cargo test --locked -p disk -p clean -j1` after integration; Windows filesystem
and Ctrl+C acceptance remain unverified. No commits or subagents were created.
