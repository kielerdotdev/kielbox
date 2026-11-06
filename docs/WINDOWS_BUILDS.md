# Windows builds

The [Windows workflow](../.github/workflows/windows.yml) produces the
`kielbox-windows-x64` artifact on pushes to `main`, `v*` tags, pull requests, and
manual runs. It uses `windows-2025` x64 and the Rust toolchain pinned in
`rust-toolchain.toml`. No signing or publishing credentials are required.

## What runs

1. Discover the newest available x64 Windows SDK shader compiler using the same
   helper as the local packager. Explicit compiler overrides remain authoritative.
2. Configure Rust source-path remapping for the checkout and runner profile.
   Both Windows separator forms are covered. This is best-effort compiler path
   remapping, not a guarantee about linker-generated data or external tools.
3. Restore Cargo downloads and release output. Cache keys include the toolchain,
   dependency/configuration hashes, and commit; a matching dependency prefix lets
   the next commit reuse the previous successful build without freezing the cache
   at its first version.
4. Run the file-only packaging regressions and disk/cleanup tests. Tests use the
   release profile so their dependencies can be reused by the application build.
5. Build the desktop and cleanup CLI with the lockfile enforced and one Cargo job.
6. Archive the exact checked-out Git tree, then stage the release package with
   its source archive, notices, payload manifest, and SHA256 checksums. The build
   label contains the run number and commit; the app version still comes from Cargo.
7. Launch the packaged cleanup CLI's `--help` and desktop's read-only `--diagnose`
   from a separate fresh working folder. Each command has a 30-second budget.
   Validate the JSON schema and release profile, then recheck package hashes.
8. Upload only `dist/windows/` as `kielbox-windows-x64`, retained for 30 days,
   and put the download link in the job summary.

The setup report may return exit 1 because a hosted runner lacks NVIDIA hardware
or optional media tools. A valid release report is required in either case;
crashes, unexpected exit codes, malformed JSON, and timeouts fail the job.

This workflow checks buildability and packaged commands. Hosted runners do not
qualify capture performance, GPU preview, real audio devices, or the screenshot
picker. Those need the [hardware checks](HARDWARE_STOP_EVALUATION_2026-11-05.md).

## Download and launch

Select a successful run in [Actions](https://github.com/kielerdotdev/kielbox/actions/workflows/windows.yml)
and download its artifact. Downloads require a GitHub login and access to this
private repository. Extract the whole package and follow `README-WINDOWS.txt`.
Use `Start-rcr.ps1` with the same existing working folder after each update.

Artifacts are unsigned development snapshots. They expire and are not an
automatic-update feed. Tags currently run the same build; they do not publish
a GitHub Release, sign binaries, or make anything public. See the
[updater design](AUTO_UPDATE.md) for the separate release/promotion pipeline.

## Run the packaging checks locally

```powershell
powershell -NoProfile -File scripts/test-package-windows.ps1
powershell -NoProfile -File scripts/ci/check-windows-package.ps1 `
  -PackageDirectory dist/windows `
  -ReportDirectory .downloads/package-check
```

The second command requires a real release package including `rcr-clean.exe`.
Its local reports may contain machine paths; they remain outside the artifact.
The file-only suite creates fake executables in `.downloads/package-tests-*`.
Remove those later with `scripts/clean-local-builds.ps1`.

References: [GitHub artifact handling](https://docs.github.com/en/actions/tutorials/store-and-share-data),
[Cargo cache action](https://github.com/actions/cache), and
[Rust path-remapping limitations](https://doc.rust-lang.org/rustc/remap-source-paths.html).
