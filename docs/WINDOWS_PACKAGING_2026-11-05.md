# Safe Windows staging — 2026-11-05

> Historical record. Source paths and commands describe the audited revision.
> Retired frontend sources and proof scripts were removed after this report;
> see [Rune status](RUNE_MIGRATION.md) for current behavior. Exact original reports remain in git at `a52bff7`.

**All local review gates passed for the exact staged release.** Four actual packaged 6/60-second hardware cases passed complete media/preview/retirement gates (Stop→UI idle 93–252 ms). Clean-working-folder diagnosis/render/native checks, final six viewer runs/80 unchanged budgets and post-runtime verification of all 22 payloads passed. The [hash-bound acceptance JSON](../.downloads/plan-continuation/release-acceptance.json) closes this wave. This remains the same PC with installed FFmpeg, not clean-machine or public-distribution acceptance.

`scripts/package-windows.ps1` retains `-SkipBuild` and `-DevelopmentExecutable`. Its first-use defaults remain `dist/windows` and `dist/windows-dev`. An existing destination now fails before building or copying anything. Choose a fresh version or explicit output instead:

```powershell
# Parent runs the release build once all product code is frozen.
powershell -NoProfile -File scripts/package-windows.ps1 -SkipBuild -Version 0.1.0-test1
powershell -NoProfile -File scripts/package-windows.ps1 -SkipBuild -OutputDirectory dist/rcr-review-20261105
# Explicit development staging, not a portable release:
powershell -NoProfile -File scripts/package-windows.ps1 -DevelopmentExecutable target/debug/desktop.exe -OutputDirectory dist/rcr-dev-review
```

Version labels permit only a leading alphanumeric followed by alphanumeric, dot, underscore or hyphen (80 characters maximum). Relative output paths resolve against the repository root. Output and input paths must not traverse reparse points. The packager never merges into an existing folder, replaces a running executable, cleans Cargo output, or enumerates/copies user recordings, projects or preferences. It copies validated inputs into a unique private sibling folder, verifies every hash, then publishes via same-volume `Directory.Move`; a competing destination causes failure. Failed private staging is preserved for inspection rather than deleting unknown files.

### Release shader-compiler discovery

The first local release attempt exposed retired UI's hardcoded SDK fallback: it searched for `10.0.26100.0`, while this host has `10.0.22621.0/x64/fxc.exe`. Before starting Cargo, normal build-and-package mode now resolves the shader compiler through `scripts/packaging/Find-fxc.ps1`: a valid explicit the retired compiler-path override takes priority, followed by `fxc.exe` in PATH, then the numerically newest installed Windows SDK version containing the x64 compiler under Program Files' `Windows Kits/10/bin`. A newer SDK folder without a compiler is skipped. A broken explicit override is an actionable error rather than permission to choose another compiler silently.

If no compiler is found, packaging explains how to install the Windows SDK or set the retired compiler-path override and fails before invoking Cargo. The chosen absolute path is passed only in the packaging process's environment during the build; the previous value, including absence, is restored in `finally` after success or failure. No user/machine environment or SDK files are changed. `-SkipBuild` and development staging do not perform compiler discovery. Local release compilation and normal build-and-package discovery subsequently passed as recorded below; clean-machine acceptance remains separate.

The package contains `rcr.exe`, release-only `clean.exe` and `CLEANUP.md`, the main README, explicit Windows instructions, working-folder launcher, read-only diagnostic launcher, integrity verifier, GPL/Apache texts, Cargo.lock and the existing local retired UI/NVENC/DPDFNet/font/FFmpeg-header notices. It does not bundle FFmpeg executables/shared libraries, drivers, user data, or a source checkout. `-SourceArchive` can include an explicitly prepared zip/tar/tar.gz/tgz corresponding-source archive; the packager hashes and inventories it but cannot certify that its contents match the build.

`package-manifest.json` inventories every payload file by relative path, SHA-256 and byte count, and records source revision, dirty status, build kind, optional source inclusion and a version label. `SHA256SUMS.txt` covers every payload plus the manifest. `Verify-package.ps1` checks both inventories and manifest integrity, refuses duplicate/traversing/reparse paths and reports missing or modified files. This detects damage; unsigned local hashes do not authenticate a publisher.

## Working-folder semantics

`Start-rcr.ps1 -WorkingDirectory "C:\existing RCR data"` requires an existing folder and starts the chosen executable with that working directory. `-ValidateOnly` returns its resolved paths without launching or writing. It never moves/reanchors data or invents an upgrade migration. Keep using the same data working folder when launching a new version. Direct executable launch retains the existing Windows/shell working-directory semantics; a shortcut can set **Start in** explicitly. Close the previous app with **Quit rcr** in its tray before launching another version.

`Diagnose-rcr.ps1` invokes the existing `--diagnose` command from the chosen working folder. It does not capture, activate a microphone, play media, install tools or change devices. It is a separate opt-in action, not a packaging side effect.

## Release portability and remaining distribution gates

The retired UI source uses `#[cfg(not(debug_assertions))]` to compile Windows shader bytes in `build.rs` and includes `OUT_DIR/shaders_bytes.rs` in its release renderer. Debug builds load source-checkout HLSL at runtime. Development staging is therefore explicitly marked checkout-dependent. Actual packaged diagnosis and rendered/native checks now passed from a separate writable working folder, without launching from the checkout. This establishes that local working-folder boundary; it does not remove installed dependencies from the same PC or establish clean-machine GPU/driver/runtime availability.

The packager now retains the known local notices that the old standalone script omitted, including embedded Liberation Sans licensing and generated FFmpeg-header notices. The top-level `THIRD_PARTY.md` has been reconciled with the current exact FFmpeg 7.0.2 implementation and is copied into the package. An exhaustive license review for the exact compiled transitive Rust dependency set and exact corresponding source (including the current dirty-tree changes) remain external-distribution gates. Neither a git commit plus `dirty=true` nor an unreviewed archive establishes that source obligation. No public publishing or compliance certification is performed by staging.

## Regression checks

```powershell
powershell -NoProfile -File scripts/test-package-windows.ps1
```

These CPU/file-only tests create tiny fake executables and owned fixtures below a fresh `.downloads/package-tests-*` directory; they never run an application or touch actual `dist` packages. They check complete notice/hash inventory, tampered payload/manifest detection, old/empty/locked destination refusal, preservation of user recording/project sentinels, explicit working-folder resolution, no implicit folder creation, failed-copy nonpublication, invalid version rejection, explicit source-archive inclusion, and reparse-ancestor refusal. Junction creation requires running outside the app sandbox; it does not require changing the OS audio/display/input state.

Windows PowerShell 5.1 completed all **34 assertions** successfully. The latest preserved fixture root is `.downloads/package-tests-3709eba1fa0c4016bd7ccc05de71647f/`. This includes the original 21 staging assertions plus fake-SDK numeric ordering, missing-newest-compiler handling, PATH/explicit precedence, actionable missing/invalid compiler errors, and process-environment restoration after successful/failed fake build callbacks. No Cargo or fixture executable is run by these checks. The actual release build/staging below is a separate receipt; tiny fixtures do not establish clean-folder diagnosis or portable UI launch.

Actual source-build doctor checks also passed for JSON output, human-readable output and an explicitly missing tool override. The override check reports its failure without silently substituting another tool. Packaged-release diagnosis, path validation and rendered/native checks subsequently passed as recorded below; clean-machine portability and manual foreground fullscreen experience are not claimed.

## Actual local release build and staging

The parent completed the optimized release build; its resumed build log is `.downloads/plan-continuation/release-build-resumed.log` (approximately 8 minutes 16 seconds). Normal build-and-package mode then selected `C:\Program Files (x86)\Windows Kits\10\bin\10.0.22621.0\x64\fxc.exe`, completed its release build check in 16.19 seconds and atomically staged the fresh folder `dist/windows-reviewed-2026-11-05/`. The preserved packager log is `.downloads/plan-continuation/release-package.log`.

The staged `rcr.exe` is 56,478,720 bytes with SHA-256:

```text
1EEA531A1D2AB8D8151BE1E84C8781A6A04F0E025D413450DC2B01A97698E63D
```

All **22 payload files** were independently verified twice. `package-manifest.json` and `SHA256SUMS.txt` record the payload and manifest hashes. The manifest identifies a release package (`debug_shaders_require_checkout=false`), version label `2026.11.05-reviewed`, source commit `aeddd2589491c635d2ebe1a0b2b4ac25a6274808` and `source_tree_dirty=true`. No corresponding-source archive is included. These provenance fields preserve the distribution-review boundary above; the git commit alone does not contain the local changes.

This receipt establishes local optimized compilation, real installed-SDK discovery, fresh non-overwriting staging and hash integrity. The exact packaged executable also passed all four actual 6/60-second no-audio/system-loopback Desktop cases in `.downloads/plan-continuation/release-hardware/desktop-evaluation.json` (`success=true`). Stop→UI idle was 93.22–251.86 ms and Library ready 124.84–312.93 ms; all outputs fully decoded with active content, correct timing, actual GPU preview and clean requested audio. The slower six-second system row includes 203.0252 ms durable writer finalization; the cause of that variation is not established. See [the full release hardware table](HARDWARE_STOP_EVALUATION_2026-11-05.md).

Release hardware success is established by its own report, not inferred from earlier debug/source results. These ordinary-desktop captures do not prove loaded gameplay, microphones, hour-long sessions, physical device changes or clean-machine portability.

## Actual packaged clean-working-folder acceptance

The parent ran the exact staged release from `.downloads/plan-continuation/release clean work` (including spaces). Direct `--diagnose` reported the release profile and exact executable/working-directory paths and created zero files during that diagnostic phase. The packaged `Diagnose-rcr.ps1` also returned JSON with exit zero; `Start-rcr.ps1 -ValidateOnly` returned the exact chosen executable/data paths without launching or reanchoring data. Evidence is `release-clean-doctor.json` and `release-clean-doctor-launcher.json` in `.downloads/plan-continuation/`.

The packaged 900-pixel share view passed native rendering and bounds checks; `release-clean-share-900.png` was visually inspected. Actual packaged `--smoke-editor-native-fullscreen` also passed in **11,263 ms for the entire keyboard fixture**, not for one transition. F11 entry took 29 ms, Escape exit/restoration 25 ms, second entry 27 ms, F11 exit/restoration 32 ms, sidebar-hide restoration 56 ms, and immediate hide during pending entry 306 ms (including a deliberate 300 ms stability wait). Strict physical RECT/styles/logical bounds/monitor coverage and project/history/volume checks passed with `activation_observed=false` and `os_input_sent=false`. Evidence is `release-clean-native-fullscreen.log`; manual foreground interaction and monitor scanout are outside this owned nonactivating proof.

These local release working-folder checks passed with existing dependencies, including installed FFmpeg, on the same PC. They do not establish clean-machine installation, exhaustive DPI/monitor configurations, loaded CS, microphone behavior or public-distribution license/source compliance.

## Final package acceptance and launch

The exact release SHA above passed the final viewer suite at `.downloads/plan-continuation/viewer-release/20261105-191613-94ff28a1/summary.json`: **six generated-media runs, 80 unchanged budget checks, zero failures**, with layouts, capture-menu and editor-keyboard proofs. Cold first-paint samples were 706.4409–814.4757 ms; 100 rapid switches ending on the correct paint took 61.8378–92.1208 ms. Post-runtime `Verify-package.ps1` again passed all 22 payload files, and source diff checks passed. The [final acceptance JSON](../.downloads/plan-continuation/release-acceptance.json) was validated against the artifact/report hashes and records `all_local_review_gates_passed=true`.

Launch only when you choose to, retaining your existing working/data folder. From the existing repository-root data folder used in this evaluation:

```powershell
powershell -NoProfile -File .\dist\windows-reviewed-2026-11-05\Start-rcr.ps1 -WorkingDirectory (Get-Location).Path
```

For another existing data folder, use the package launcher's absolute path and pass that folder explicitly. Keep recordings/projects and quit an older running app through its tray before changing builds. No user application was automatically launched by this final documentation step. The remaining roadmap and external license/corresponding-source review are still open; local acceptance is not a claim that every planned feature or hardware scenario is complete.
