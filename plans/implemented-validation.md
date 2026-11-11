# Integrated Boat validation

Date: 2026-11-11. All Rust compilation and test execution in this pass ran on Boat VM `bx_hd7j6w3r`, source root `/home/user/kielbox-integration`. No local Rust compilation or heavy tests, commits, subagents, or VM shutdown. Applied ponytail for minimal fixes and unslop for this report.

## Snapshot

Base HEAD `d96346b2ee44cf44b9ffeb5881c6a44d2cbcb6c7`. Refreshed source from a new `git archive HEAD`, after deleting files from the remote source directories, then applied a freshly captured binary diff. Preserved `target`, Cargo/xwin caches, Wine prefix and prerequisite downloads. No untracked source files existed under the source/config/documentation paths. Untracked plans were not transferred.

Initial archive SHA-256 `ecc6e9d1053f811b10bc69b49ac6d8cffce52384ab70e3bf948eeac7be321fba`. Initial patch SHA-256 `8347cb8b7bedd448f166d4e570ad75fe517e8f22b2cc3aedac44a17b72cc39b9`.

After the parent's safety fixes, refreshed `recordings.rs`, `rune_ui.rs`, and `rune_editor.rs`. After the parent's UI review fixes, refreshed `rune_capture.rs`, `rune_shot.rs`, `rune_ui.rs`, and `quick_recording.rs`. Reran the Windows suite after these transfers. Final integrated patch SHA-256 `11581102b529fa2de037c8b94651930e3ae38d0f5209c892b5687b00a74810b6`.

The sorted SHA-256 content manifest for 1,143 tracked source/config/documentation/vendor files has SHA-256 `a90fc82ec3314279660814b34ee261cc46dfee38c7182da35a2bc996822eee09`. Remote `sha256sum -c` passed for every entry before the final build. Local manifest and patch are in `/tmp/kielbox-validation/`; remote manifest verification is `/home/user/source-verification.log`.

## Commands and results

All commands below execute within the remote source root, through `boat --no-update exec --timeout ... bx_hd7j6w3r`. Existing setup and caches from `implemented-boat-prep.md` were reused.

| Invocation | Result |
| --- | --- |
| `CARGO_BUILD_JOBS=2 scripts/ci/build-windows-cross.sh test`, 1200-second bound | Exit 0. Initial integrated Windows/Wine run: clean 1 unit + 2 integration passed; desktop 168 passed, 3 ignored; disk 39 passed; disk doc-tests 0. Superseded for desktop by the final rerun below. |
| `cargo test --locked -p disk -p clean -j1`, 240-second bound | Exit 143, SIGTERM, timed out waiting for Cargo build lock. No tests ran. Not a test failure or OOM. |
| Same Linux command, 1200-second bound | Exit 0. clean 1 unit + 2 CLI integration and disk 16 passed, 19 total, 0 failed or ignored. Disk doc-tests 0. Cargo reported 55.81s including lock wait. |
| `cargo xwin test --locked --no-fail-fast --target x86_64-pc-windows-msvc -p toolkit -j2`, 1800-second bound | Exit 0. Windows/Wine toolkit 51 unit, 1 local-upload, 3 renderer-viewport and 2 screenshot-pixel tests passed, 57 total, 0 failed or ignored. Toolkit doc-tests 0. Cargo reported 3m30s including lock wait. |
| `CARGO_BUILD_JOBS=2 scripts/ci/build-windows-cross.sh test build smoke render`, 1800-second bound | Final Windows test stage passed: clean 1 unit + 2 integration; desktop 169 passed, 3 ignored; disk 39 passed; disk doc-tests 0. Overall exit 0. Release desktop and clean cross-build passed in 6m10s. Wine smoke passed. Render produced all six nonempty PNGs. |
| Local `git diff --check` | Exit 0. No local Rust execution. |

Toolkit command used the same static CRT setting, xwin cache, Wine prefix, UTF-8 locale, DLL overrides and Windows FFmpeg/MinGit paths as the existing script, with `wine` as Cargo's target runner. This was a separate toolkit invocation because the script's test stage selects disk, clean and desktop only.

The final Windows suite passed 211 tests across clean, desktop and disk. Together with toolkit, 268 Windows/Wine tests passed. Linux adds 19 tests on its platform; repeat invocations are not counted as additional coverage.

Desktop ignored entries remain ignored: `blocked_stdin_child` is an owned child entrypoint exercised through its watchdog parent; `generated_stop_latency_eval` is an opt-in generated-media evaluation; `five_minute_av1_replay_muxes_and_decodes_while_an_independent_screencast_runs` requires AV1 NVENC. No standalone invocation of these ignored entries occurred.

Local copies of all logs and six render PNGs are preserved under `/tmp/kielbox-validation/`, with a combined `/tmp/kielbox-validation/results.tar`. Remote logs: `/home/user/integrated-windows-test.log`, `/home/user/integrated-linux.log`, `/home/user/integrated-linux-retry.log`, `/home/user/integrated-toolkit.log`, `/home/user/integrated-final.log`.

## Release smoke and render

The combined final command completed successfully at approximately 17:41:57 UTC. The release outputs were `desktop.exe`, 50,722,304 bytes, and `clean.exe`, 1,120,768 bytes. Linker warnings reported missing Microsoft CRT PDB debug information, LNK4099; they did not prevent linking or execution.

The existing script verified that `desktop.exe --proof-seconds 1` returns exit 2 for the unsupported legacy flag and that `clean.exe --help` succeeds. It then rendered library, capture, editor, disk, tools and settings under Xvfb with Wine Vulkan and Mesa software rasterization. The script checked that all six output PNGs exist and are nonempty. PNG headers locally confirm 1200×800 for each. File sizes were library 31,318; capture 75,314; editor 22,741; disk 26,298; tools 41,730; settings 43,306 bytes. This checks render-path execution and artifact production, not a visual/accessibility review or native GPU acceptance.

## Fixes and limits

No compiler or test defect required a validation-agent source edit. Parent source edits were coordinated and refreshed as described above. Compiler warnings were not treated as failures, and no broad formatting changes were made.

Wine tests and Mesa software rendering do not establish native Windows GPU/capture, NVIDIA encoding, installer, physical clipboard/input, Narrator, mixed-DPI/HDR, game/anti-cheat, performance or long-duration acceptance. No native Windows session or hardware acceptance ran. The VM remains running; parent owns shutdown and its TTL is the backstop described in the preparation report.

## Final coordinator confirmation

After inspecting all six scenes, the coordinator fixed three capture descriptions
that overflowed their columns by enabling wrapping. Only rune_capture.rs changed
after the manifest above; documentation/tracker updates do not change the binary.
Final local and remote rune_capture.rs SHA-256 matched:
`ae37449ea8ee8c342f182ac3a9ae81f9d97bc486929e1451d360d778b284fd1d`.

Reran `CARGO_BUILD_JOBS=2 scripts/ci/build-windows-cross.sh test build smoke render`
on the same Boat VM. Exit 0: 211 Windows/Wine tests passed, 3 ignored, release
build completed, smoke passed, all six renders produced. This is a confirmation,
not 211 additional unique tests. Final capture render inspected at 1200×800;
explanatory text now wraps inside both columns. The toolkit's 57 passes and
Linux's 19 passes remain the applicable unchanged-component results.

Final release hashes:
- desktop.exe: `65ffa09a28fed689ab2b7ab19c30c3b346b18e0d17ea8848d29f5d84e8cf863c`
- clean.exe: `8456ba0c1e9d379fae8d6ee1acdd470350907d50961ad7056323c668a34a54a0`

Durable local logs, source manifest and final capture render are under
[validation-evidence](validation-evidence/). Initial full artifacts also remain
in `/tmp/kielbox-validation/`. Whitespace and documentation links passed. The
Impeccable mechanical detector returned no findings for the changed Rune views;
that detector is not native accessibility validation. No source commits made.

Boat shutdown requested at 17:47:03 UTC. A subsequent `boat --no-update list --json`
returned an empty sandbox list. The disposable no-snapshot VM is no longer listed.
