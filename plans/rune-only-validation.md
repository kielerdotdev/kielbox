# Rune-only source cleanup

Date: 2026-11-11. Base commit: `a52bff7`.

Removed the retired desktop framework's 185 vendored files, 56 unreachable
desktop modules and 13 proof/build helper scripts. The retained entry point and
its module declarations resolve to 29 desktop source files, including test
modules. Screenshot, recording, editing, disk and utility backends remain.

Removed the desktop's unused direct `raw-window-handle` and
`unicode-segmentation` dependencies. These packages remain available through
other consumers in the lockfile. Removed obsolete shader-compiler packaging
tests; retained the package integrity and user-data protection tests.

Current development, utility, migration, packaging and license documentation
describe Rune. Dated audit records are marked historical. The original source
manifest and exact historical reports remain in git at `a52bff7`; their hashes
were not rewritten to describe the new tree.

## Checks

- Repository scan: no retired-framework paths, imports, package references or
  documentation references remain in tracked files present in the checkout.
- Current README, license, development, utilities, migration and Windows build
  documentation: all local Markdown link targets exist.
- `git diff --check`: passed.
- On Boat, `cargo tree --locked --target x86_64-pc-windows-msvc -p desktop`
  passed and contained no retired-framework packages.

## Remote execution

Disposable Boat VM `bx_jhdfuej2`, source `/home/user/kielbox`. Transferred
`git archive HEAD` and applied the complete working-tree diff. All Rust
compilation and execution occur on this VM, with no local Rust builds.

`scripts/ci/build-windows-cross.sh setup` passed. The VM uses Wine 9.0,
cargo-xwin 0.23.1, pinned Windows FFmpeg 9.0.2 and MinGit 2.56.0.4.

Validation command:

```sh
CARGO_BUILD_JOBS=2 scripts/ci/build-windows-cross.sh test build smoke render
```

The test stage passed: 1 clean unit test, 2 clean CLI integration tests,
169 desktop tests and 39 disk tests, 211 total with 3 ignored desktop tests.
Release build, smoke and all six Rune scene renders passed. The combined
command exited 0; release compilation took 5m29s. Compiler warnings included
unused code and missing Microsoft CRT debug symbols; they were not treated as
errors. No code fixes were required by validation.

Desktop executable SHA-256:
`aff28cb2898b01a2a80ba2bc9e4246244b341d7625edd1f2953a4e617a19baf9`.
Logs and scene images are saved in `/tmp/kielbox-rune-only-validation/`.
The disposable VM shutdown was requested at 18:45:50 UTC.

All 826 source/build files in the local manifest matched the VM before these
checks. Manifest SHA-256:
`2f3a2a8fd5016d18fdfa5090811b3a60b313961cb2d5a89cfed83dc9c0ef0806`.
 Native Windows GPU capture, installer behavior,
accessibility and game compatibility are outside this Wine validation. The
PowerShell packaging tests were not executed in this pass.
