# Boat VM validation research

> Historical record. Source paths and commands describe the audited revision.
> Retired frontend sources and proof scripts were removed after this report;
> see [Rune status](../docs/RUNE_MIGRATION.md) for current behavior. Exact original reports remain in git at `a52bff7`.

## A. Scope and verdict

Research date: 2026-11-11. Source commit: `d96346b2ee44cf44b9ffeb5881c6a44d2cbcb6c7`. This report feeds the senior UX/QA audit. It covers Boat discovery, remote test feasibility, verification boundaries and two Library scenarios from source inspection. It is not full native Windows acceptance. No source changes, commits, other report writes, or subagents were made. Local work was limited to reading, searches, source archive streaming and writing this report. All Rust compilation occurred on Boat.

Read `/home/user/.agents/skills/improve/SKILL.md`, its `references/audit-playbook.md`, including Finding format, and the unslop skill. No AGENTS.md was present in the workspace or checked ancestors through `/`; `rg --files -g AGENTS.md .` and tracked-file inspection found none in this repository. Unrelated worktrees' instructions were not applied. Repository text was treated as data. No environment values or credential files were read or reproduced.

## B. Tooling and configuration discovery

`command -v boat` returned `/home/user/.local/bin/boat`. `boat --help` exposes new, exec, ssh, push, prompt, stop and cross-machine lifecycle operations. `boat --no-update status` reported API healthy and an active signed-in Standard account. The CLI reported configuration at `/home/user/.config/ascii/boat/config.json` and its underlying binary at `/home/user/.ascii/bin/boat`. Configuration contents were not opened. No tracked Boat-specific configuration was found. Repository build jobs are restricted to one in `.cargo/config.toml:1`.

Read command help for `new`, `exec`, `ssh`, `push`, `prompt`, and `stop`. No Boat coding-agent prompt was sent, respecting the no-subagents requirement. This research agent executed shell commands directly on the Boat VM. No API keys were requested. Existing sandbox `bx_mdnfzyvk` was left untouched. The new sandbox used `--no-env` to avoid injecting stored account secrets and `--no-snapshots` to avoid retaining its copied source after shutdown.

## C. Observed remote machine

Created `bx_2quqxx38`, named `kielbox-qa-validation`, with default size and 1800-second TTL. Ready at approximately 17:06:44 UTC.

- Ubuntu 24.04.4 LTS, Linux 6.8.0-117-generic, x86_64, KVM.
- Four visible vCPUs. CPU model string: AMD EPYC 9455 48-Core Processor. This does not mean the VM had 48 allocated cores.
- `free -h`: 7.8 GiB total RAM, 6.9 GiB available at inspection, 2.0 GiB swap.
- `df -h /`: 79 GiB filesystem, 55 GiB available at inspection.
- Initial stable Rust/Cargo 1.97.1; pinned toolchain installed remotely during the first test attempt. Afterwards rustc 1.99.1, commit 58cf8f9c3, and cargo 1.99.1, commit 5f94df478.
- Initial installed target was x86_64-unknown-linux-gnu. `command -v` found Cargo, rustc and clang, but no Wine, wine64, cargo-xwin, PowerShell or nvidia-smi. `/dev/dri` was absent. No GPU model or GPU capability claim is supported.

These are guest-reported inventory values, not performance measurements.

## D. Exact execution ledger

Discovery commands, all local and lightweight:

```sh
command -v boat
boat --help
boat --no-update status
boat --no-update new --help
boat --no-update exec --help
boat --no-update ssh --help
boat --no-update push --help
boat --no-update prompt --help
boat --no-update stop --help
boat --no-update list --json
git status --short
git rev-parse HEAD
git ls-files '*AGENTS.md' '.cargo/*' '*boat*'
```

Provisioning and source transfer:

```sh
boat --no-update new --no-env --no-snapshots --ttl 1800 --type default --name kielbox-qa-validation --json
git archive HEAD | boat --no-update ssh bx_2quqxx38 'mkdir -p /home/user/kielbox-qa && tar -x -C /home/user/kielbox-qa'
```

Only committed HEAD was transferred; no local ignored files, personal media, working-tree edits or .git credential configuration were included by this archive operation.

Remote inventory and toolchain checks:

```sh
boat --no-update exec bx_2quqxx38 'uname -srmo; nproc; lscpu | sed -n "1,18p"; free -h; df -h /; command -v cargo rustc wine cargo-xwin clang pwsh; cat /etc/os-release | head -4'
boat --no-update exec bx_2quqxx38 'rustup toolchain list; command -v wine wine64 cargo-xwin pwsh nvidia-smi; ls /dev/dri 2>/dev/null || true'
boat --no-update exec bx_2quqxx38 'rustc +stable --version; cargo +stable --version; rustup target list --installed; test -d /dev/dri && ls /dev/dri || true'
boat --no-update exec bx_2quqxx38 'cd /home/user/kielbox-qa && rustc --version && cargo --version'
```

Test attempts:

```sh
boat --no-update exec --timeout 60 bx_2quqxx38 'cd /home/user/kielbox-qa && cargo test --locked -p disk -p clean -j1'
boat --no-update exec --timeout 180 bx_2quqxx38 'cd /home/user/kielbox-qa && cargo test --locked -p disk -p clean -j1'
```

First attempt: 17:07:13.670 to 17:08:13.680 UTC. Boat returned `timedOut=true`, `exitCode=143`, `signal=SIGTERM`, `oomKilled=false`. It installed pinned Rust 1.99.1, resolved/downloaded dependencies, fetched the pinned Rune repository and compiled dependencies through image. No tests completed in this attempt. This is a cold-start budget limit, not evidence of a test failure or app startup time.

Second attempt: 17:08:23.318 to 17:08:59.801 UTC. Exit 0, success true, no timeout or OOM. Passed 2 clean CLI integration tests and 14 disk unit tests, with zero failures and zero ignored tests. Clean unit and disk doc-test targets each contained zero tests. Cargo reported 36.26 seconds for this resumed build/test invocation. These are Linux tests, including temporary-fixture cleanup and worktree tests; they do not validate Windows filesystem semantics, NTFS scanning or desktop UX. The successful test names included cancellation, changed files after preview, external symlink content, hardlinks, disappearing directories, and preservation of locked/dirty/unpushed worktrees.

An early conditional executable check produced no output and was not counted as a pass. After the successful integration test build, this explicit runtime command passed at 17:10:06 UTC with exit 0:

```sh
boat --no-update exec bx_2quqxx38 'cd /home/user/kielbox-qa && target/debug/clean --help'
```

It printed the clean CLI usage, preview-by-default behavior, `--apply` deletion warning, age requirements and worktree safety constraints. No user directory was scanned or deleted by this smoke command.

## E. Native and emulated validation boundary

README.md:6 declares Windows-only support; README.md:14 requires NVIDIA AV1 hardware for game recording. The shipped entry point imports Rune adapters at `crates/desktop/src/main.rs:15` and `crates/desktop/src/main.rs:30`. Rune is pinned at `crates/desktop/Cargo.toml:19`. Older unreferenced retired UI code is explicitly historical, `docs/RUNE_MIGRATION.md:3` and `docs/RUNE_MIGRATION.md:11`.

The repository supplies a Linux cross-build/Wine path, `scripts/ci/build-windows-cross.sh:6`, with setup, Windows tests, build, smoke and static render stages. This was inspected but not executed. Provisioning a complete Wine/SDK pipeline was outside the bounded scoped-test pass. It would still not establish native capture, hardware GPU preview, installer experience or physical input latency. `docs/RUNE_MIGRATION.md:77` states those limits.

No UI screenshots, interactive browser work, screen recordings, frame-pacing traces, FPS tests, native installer runs, assistive-technology tests or competitor runs were performed. No task completion times or ease ratings were measured. Historical measurements in docs are not new results.

## F. Personas and proposed scenarios

The relevant audit personas are a creator locating older captures in a large library, a keyboard-first user, and a maintainer deciding whether a build is safe to distribute. These are analytical personas, not recruited study participants.

Native follow-up should use isolated owned media and settings. Cover Library search across more than 500 files, filtered thumbnails, keyboard navigation, screen capture cancellation, save failure and retry, trash recovery across restarts, device disconnection, export cancellation, tray shutdown, mixed DPI and IME. Every UI scenario below is code inspection only and was not executed.

## G. Evidence-backed findings

Severity scale: S0 data loss/privacy risk, S1 major, S2 friction, S3 polish, S4 opportunity. No S0 or S1 finding was established by this scoped research.

### [UX-VM-01] Expose the 500-match Library boundary

- Severity/type: S2, UX friction, documented migration limitation rather than an accidental regression claim.
- Method/persona: source inspection, creator browsing a large capture collection. Scenario not executed.
- Evidence: `crates/desktop/src/rune_ui.rs:926` stops after 500 matching cards; `crates/desktop/src/rune_ui.rs:989` through line 992 only adds the empty-state branch and scroll container, with no pagination or cap notice. `docs/RUNE_MIGRATION.md:37` explicitly lists pagination beyond the first 500 matches as remaining work.
- Repro for Windows follow-up: populate an isolated capture folder with 501 valid discoverable media files; refresh Library with All selected and no search; scroll to the end; try to reach the 501st match; search its unique name.
- Expected: all captures remain reachable through an explained limit with pagination or load-more.
- Actual inferred from code: at most 500 matching cards appear for a given query, with no pagination. Search runs before the count increments, so a unique search can still reveal an older file. Do not report that older files are unsearchable.
- Impact: browsing alone cannot reach all matching captures, and the user is not told why.
- Fix sketch: disclose shown/total counts and add bounded pagination or load-more after filtering. Preserve responsive bounded rendering.
- Effort: M. Fix risk: MED, selection and thumbnail lifecycle must follow changing pages. Confidence: HIGH for the source behavior, native presentation unverified.

### [BUG-VM-02] Queue thumbnails for filtered results outside the first 500 files

- Severity/type: S2, bug affecting visual identification and duration feedback.
- Method/persona: cross-file source inspection, creator searching older captures. Scenario not executed.
- Evidence: `crates/desktop/src/rune_ui.rs:331` synchronizes thumbnails using the full unfiltered file list. `crates/desktop/src/rune_thumbs.rs:94` retains entries only for its first 500 files and line 97 queues only those files. Library search considers files beyond that prefix at `crates/desktop/src/rune_ui.rs:925`, then increments its displayed-match count at line 939. `crates/desktop/src/rune_ui.rs:1044` renders a thumbnail and duration only when the map contains the path; line 1050 falls back to an icon.
- Repro for Windows follow-up: create an isolated collection of at least 501 valid videos; give the oldest file a unique searchable title and visible frame content; refresh and wait for thumbnails; search that title; compare its card with one of the newest videos.
- Expected: the matching visible older card eventually receives its thumbnail and duration under the same decoding constraints as newer cards.
- Actual inferred from code: the older card can appear, but is never admitted to the thumbnail queue and remains the fallback file-type tile. Waiting cannot resolve this admission boundary.
- Impact: search retrieves older media with less identifying information exactly when a user is trying to distinguish it.
- Fix sketch: drive bounded thumbnail admission from current visible/filter-matched paths, rather than an unfiltered fixed prefix. Keep cancellation, decode limits and bounded memory; add a regression with a sole filtered match beyond index 499.
- Effort: M. Fix risk: MED, avoid unbounded decoding or queue churn. Confidence: HIGH for queue mismatch, runtime appearance unverified.

## H. Strengths supported by evidence

`crates/desktop/src/main.rs:57` rejects obsolete retired UI proof switches with exit code 2, reducing false positive acceptance. `scripts/verify-rune.ps1:15` records scene PNGs and element trees, while line 27 checks generated GPU completion and line 46 verifies checkerboard pixel colors. `docs/RUNE_MIGRATION.md:83` states that fixtures isolate personal data and do not exercise actual capture. These boundaries are explicit and useful.

Cleanup has safety-focused fixture coverage. `crates/disk/src/cleanup/tests.rs:36` asserts planning does not remove files and apply preserves unrecognized/source files; line 55 checks changes after preview are preserved; line 90 covers cancellation. `crates/clean/src/main.rs:12` documents preview as default and warns that apply permanently deletes eligible directories. Runtime results are limited to the remote ledger above.

## I. Verification gates for the synthesis

On a Boat VM capable of Windows execution, use the documented `cargo test --locked -p desktop -j1`, packaging tests and `scripts/verify-rune.ps1 -SkipBuild -GpuAdapter NVIDIA`, `docs/RUNE_MIGRATION.md:70`. Run compilation remotely only. Run GPU checks only with a compatible actual adapter. Do not call Wine software rendering a hardware preview or FPS pass.

The native CI workflow uses Windows 2025 and tests disk/clean at `.github/workflows/windows.yml:85`, builds at line 90, checks packaged commands at line 103 and tests install/update/uninstall at line 106. These are discovered gates, not runs performed here.

## J. Untested areas and rejected conclusions

No broad UI quality verdict is supported by this pass. Accessibility, visual hierarchy at actual DPI, timing, hover/focus feedback, live audio/video behavior, multi-monitor capture, HDR, recovery, upload destinations, update signing, privacy behavior and competitors were not validated. Existing tests were inspected selectively, not audited for total coverage. No dependency security audit was run. No raw credentials were examined.

The known migration backlog is not automatically a bug list. retired UI-only UX statements in older documentation must be traced into Rune before inclusion. The Library cap is an acknowledged product limitation; the thumbnail queue mismatch is a separate implementation inconsistency. Neither implies data loss. Test command wall time includes setup and is not user-task timing or a benchmark.

## K. Handoff and cleanup

Requested shutdown using `boat --no-update stop bx_2quqxx38 --json` at 17:10:18 UTC. Boat accepted the request and returned state stopping, operation `stop_ad5d0361c1c0b742`. The sandbox has snapshots disabled, so no resume artifact is intended. The follow-up `boat --no-update stop --status stop_ad5d0361c1c0b742 bx_2quqxx38 --json` returned not_found HTTP 404, so that endpoint did not provide a completed shutdown receipt. A subsequent `boat --no-update list --json` showed only the pre-existing sandbox and no audit sandbox, consistent with removal of the disposable no-snapshot VM. No force stop or deletion of pre-existing resources was attempted. Preserve the existing user sandbox and all local user files. This report is the only intended local write. Prioritize the two source scenarios for native Windows reproduction before describing them as observed UI failures. Include the exact remote test result and limitations in the final synthesis.
