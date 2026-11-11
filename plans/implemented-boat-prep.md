# Boat preparation for implemented audit fixes

Date: 2026-11-11. Test sub-agent report. No source edits, child agents, local compilation or local heavy checks. Applied the unslop skill to this report.

## VM and shutdown ownership

Disposable VM `bx_hd7j6w3r`, named `kielbox-implemented-audit-prep`, created with `--no-env --no-snapshots --ttl 14400 --type default`. Boat reported ready and `archiveAfter=2026-11-11T21:21:44.548Z`. Leave it running for the parent agent's integration work. The parent owns explicit shutdown after follow-up:

```sh
boat --no-update stop bx_hd7j6w3r --json
```

The finite four-hour TTL is a cleanup backstop, not a promise of immediate shutdown at that instant. No pre-existing VMs were changed. Initial `boat --no-update list --json` returned an empty list.

Guest inventory: Linux 6.8.0-117-generic x86_64, 4 visible CPUs, 7.8 GiB RAM, 2 GiB swap, 79 GiB filesystem with 55 GiB free at transfer time.

## Source snapshot and remote paths

Source root `/home/user/kielbox-integration`. Base HEAD `d96346b2ee44cf44b9ffeb5881c6a44d2cbcb6c7`. Transferred committed source using `git archive HEAD`, then a binary diff capturing current tracked changes under crates, scripts, Cargo files, toolchain/config, README and docs. No untracked Rust modules were present when checked. The added desktop stress-test module was already tracked and included in the diff. No plans, ignored files, local target output, .git directory, credential configuration or personal media were copied. Tracked-name inspection found no environment files or media files with mp4/mov/mkv/mp3/wav suffixes. Files named secrets.rs are Rust source modules, not credential dumps.

Patch `/home/user/audit-working-tree.patch`, SHA-256 `9582db08228e70ab38648347e099714905b7a50eb415a268f076a6c87b4e28e1`. Applied successfully, with one trailing-whitespace warning. This is a captured intermediate snapshot: desktop agents were still editing. Parent must refresh the final integrated source before desktop compilation or tests.

```sh
git archive HEAD | boat --no-update ssh bx_hd7j6w3r 'mkdir -p /home/user/kielbox-integration && tar -x -C /home/user/kielbox-integration'
git diff --binary HEAD -- crates scripts Cargo.toml Cargo.lock rust-toolchain.toml .cargo README.md docs | boat --no-update ssh bx_hd7j6w3r 'cat > /home/user/audit-working-tree.patch && cd /home/user/kielbox-integration && git apply /home/user/audit-working-tree.patch'
```

## Commands and results

Read `plans/qa-vm_validation.md` and `scripts/ci/build-windows-cross.sh` before provisioning. Read Boat new/exec help. Provisioning command:

```sh
boat --no-update new --no-env --no-snapshots --ttl 14400 --type default --name kielbox-implemented-audit-prep --json
```

Linux scoped command, remote log `/home/user/linux-disk-clean.log`:

```sh
boat --no-update exec --timeout 240 bx_hd7j6w3r 'cd /home/user/kielbox-integration && cargo test --locked -p disk -p clean -j1 > /home/user/linux-disk-clean.log 2>&1; result=$?; tail -65 /home/user/linux-disk-clean.log; exit "$result"'
```

Windows prerequisite setup only, remote log `/home/user/windows-setup.log`:

```sh
boat --no-update exec --timeout 600 bx_hd7j6w3r 'cd /home/user/kielbox-integration && scripts/ci/build-windows-cross.sh setup > /home/user/windows-setup.log 2>&1; result=$?; tail -50 /home/user/windows-setup.log; exit "$result"'
```

No desktop build/test/smoke/render stage was requested. Wine or software Vulkan results cannot establish native Windows hardware capture, NVIDIA AV1, physical input latency or native installer acceptance.

Linux tests passed, exit 0, `success=true`, no timeout or OOM. Boat start `17:22:04.827 UTC`, finish `17:25:00.264 UTC`. Cargo reported a 2m20s test-profile build after initial toolchain/dependency setup. Passed 19 tests: 1 clean unit test, 2 clean CLI integration tests, and 16 disk unit tests. Disk doc-tests contained zero tests. No failures or ignored tests. New cancellation coverage passed for both cleanup ledger retention after the first removal and JSON CLI exit code three. Worktree ignored-data rechecks, preview read-only behavior, symlink and dirty/locked/unpushed protections also passed. This validates Linux fixture behavior, not Windows filesystem semantics.

Remote toolchain after the run: rustc 1.99.1 `58cf8f9c3`, cargo 1.99.1 `5f94df478`.

Windows setup passed, exit 0, no timeout or OOM, from `17:22:10.240` to `17:26:23.468 UTC`. Installed apt prerequisites, Windows target for pinned Rust, cargo-xwin 0.23.1, FFmpeg 9.0.2 and MinGit 2.56.0.4. Both downloaded ZIP checksums matched the script's pinned values. Wine prefix `/home/user/.wine-kielbox` exists. Wine reports `wine-9.0 (Ubuntu 9.0~repack-4build3)`.

Setup observations and repair:

- `wine64` was absent. The existing script's fallback to `wine` succeeded. This diagnostic did not fail setup.
- Post-setup inspection found no `clang-cl`, despite the script's best-effort alias loop. Added the missing VM-only driver alias with `sudo ln -s /usr/bin/clang /usr/local/bin/clang-cl`. `clang-cl --version` then reported Ubuntu Clang 18.1.3 and target `x86_64-pc-windows-msvc`. No repository script was changed.
- An initial target inventory outside the source directory showed only the default toolchain's Linux target. Repeating inside the source directory confirmed both Linux and Windows targets for the pinned toolchain. A combined inspection command returned exit 2 because its `ls` glob found no clang-cl binary; the alias above resolves that prerequisite.

The script installs cargo-xwin but does not prefetch its SDK until needed. Prefetched it without any project compilation:

```sh
boat --no-update exec --timeout 300 bx_hd7j6w3r 'cd /home/user/kielbox-integration && XWIN_ACCEPT_LICENSE=1 XWIN_ARCH=x86_64 XWIN_CACHE_DIR=/home/user/.cache/cargo-xwin cargo xwin cache xwin > /home/user/xwin-cache.log 2>&1; result=$?; tail -20 /home/user/xwin-cache.log; exit "$result"'
```

Exit 0, `17:26:49.786` to `17:27:34.901 UTC`. Reported successful Microsoft CRT and Windows SDK cache setup at `/home/user/.cache/cargo-xwin/xwin`. Both `crt/include`, `crt/lib`, `sdk/include` and `sdk/lib` exist. Final `cargo xwin env --target x86_64-pc-windows-msvc` also exited 0, writing only generated compiler configuration to `/home/user/xwin-env.txt`. Tool discovery found clang-cl, lld-link, llvm-rc, llvm-lib, llvm-ar, wine and xvfb-run. Final free disk was 51 GiB.

Prerequisite runtime check, no Kielbox binary involved:

```sh
boat --no-update exec --timeout 60 bx_hd7j6w3r 'export WINEPREFIX=/home/user/.wine-kielbox WINEDEBUG=-all WINEDLLOVERRIDES=mscoree,mshtml= LANG=C.UTF-8 LC_ALL=C.UTF-8; xvfb-run -a bash -c '\''wine /home/user/.cache/kielbox-ffmpeg/9.0.2/bin/ffmpeg.exe -version | head -1; wine /home/user/.cache/kielbox-mingit/2.56.0.4/cmd/git.exe --version; wineserver -w'\'''
```

Exit 0, output identified FFmpeg 9.0.2 and Git 2.56.0.windows.4. This only establishes that those prerequisite executables start under Wine. The FFmpeg pipeline reports the first version line and is not a recording/encoding test.

## Parent follow-up

Refresh the source after all desktop edits finish, preferably by staging a fresh `git archive HEAD` plus a freshly captured binary diff and explicitly copying any relevant new Rust modules. Do not blindly reapply the new diff on top of the old patched files. Preserve the cache and toolchain outside the source root. Then run the existing script's desired stages on Boat, for example `scripts/ci/build-windows-cross.sh test build smoke`, with bounded command limits and logs. The setup phase is complete; no desktop compilation or desktop tests have run in this pass. Native hardware acceptance remains separate from cross-compilation and Wine.

The VM remains available for follow-up. Parent must stop `bx_hd7j6w3r` when done; snapshots are disabled, so stopping erases the disposable VM.
