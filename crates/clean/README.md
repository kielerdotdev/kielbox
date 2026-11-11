# clean

A single command for space consumed by agents and development builds. Uses the
same `disk` scanner as the desktop disk explorer, without starting the UI.

```powershell
# Preview everything eligible under these roots.
clean C:\src "$env:USERPROFILE\.codex\worktrees"

# Remove eligible targets, using the default minimum age of 30 days.
clean C:\src "$env:USERPROFILE\.codex\worktrees" --apply

# Only dependencies and Rust build output, at least seven days old.
clean C:\src --kind node-modules,rust-target --older-than 7 --apply

# Machine-readable preview for agents/scripts.
clean C:\src --json
```

With no roots, searches the current directory. `--dry-run` explicitly selects the
default preview behavior; it conflicts with `--apply`. There is no interactive
prompt. `--apply` permanently deletes the eligible directories shown in that run.
Stop agents, builds, and development servers before applying cleanup.

## Targets

| Kind | Recognized directories |
| --- | --- |
| `node-modules` | `node_modules` beside `package.json` |
| `rust-target` | `target` beside `Cargo.toml` |
| `cache` | `.next`, `.nuxt`, `.svelte-kit` beside `package.json`; `__pycache__`; `.pytest_cache`, `.mypy_cache`, `.ruff_cache` beside `pyproject.toml`, `setup.py`, or `requirements.txt` |
| `worktree` | Linked Git worktrees physically inside the selected roots |

All kinds are enabled by default. Custom Cargo target directories, arbitrary
`dist`/`build` folders, global package caches, and missing worktree registrations
are not cleanup targets. Main repositories and `.git` contents are excluded from
artifact discovery. Worktrees outside the roots are not discovered through Git.
Overlapping roots are deduplicated; removing a worktree includes its artifacts.

## Preservation checks

Every entry in a target must have a modification time older than the cutoff.
This includes nested files and directories; worktrees also check their Git admin
directory (HEAD, index and reflogs). Modification time does not prove inactivity:
reading or executing an old dependency does not refresh its modification time.

Worktrees must be unlocked, have no modified/untracked files, have no submodules,
and have HEAD reachable from existing local remote-tracking refs. This includes
detached worktrees. Repositories without those refs and worktrees with unpublished
commits are preserved. No fetch is performed. Worktrees containing any ignored files, including ignored
build output, are preserved by default. Review those files before using
`--allow-ignored-worktree-data` to permit their permanent removal with the worktree.
This flag can delete non-regenerable ignored settings, secrets and datasets, and
must be supplied for both the preview and apply runs to see the same policy.
The check runs again immediately before deletion. The opt-in does not bypass
age, dirty/untracked, lock, submodule or remote-tracking checks. Git branches are kept.
Removal uses `git worktree remove` without force, keeping Git registration correct.
When worktree cleanup is selected, a preserved worktree is not searched for
artifact cleanup. Selecting only artifact kinds allows inspecting its artifacts.

The command preserves any target containing its working directory. Scan roots and
target ancestors cannot be links or junctions. Internal package links are skipped
when measuring and unlinked on deletion; their external targets are preserved.
Offline/recall entries and targets that cannot be fully inspected are preserved.
Scope, age, target type and Git state are checked again just before deletion.
Concurrent filesystem changes remain possible; this does not lock out agents.

Sizes are logical bytes, counting hard links per entry. They do not guarantee the
same physical space reclaimed, especially for pnpm, sparse or compressed files.
Errors are reported per directory; a failed removal can leave a partial directory.
Ctrl+C cancels between traversal/deletion operations; an in-progress filesystem
delete or Git operation finishes before cancellation takes effect.

Exit codes: `0` success (including no eligible targets), `1` scan/deletion failure,
`2` invalid arguments, `3` cancellation. Preserved targets appear with reasons and
are not failures. `--json` reports planned targets, skipped targets, removed paths,
and scan/removal errors separately. Apply cancellation retains completed removals
and errors, reports `cancelled: true` and `success: false`, and exits with code 3.

## Build

The Windows release bundle includes `clean.exe`. To build or install it alone:

```powershell
cargo build --locked --release -p clean -j1
cargo install --locked --path crates/clean
```

No GPU, FFmpeg or desktop UI is required. Git must be on PATH for worktree cleanup.
