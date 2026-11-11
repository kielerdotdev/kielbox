# Development

Kielbox uses Rune for its desktop UI. The current feature inventory and remaining
work are in [Rune status](STATUS.md). Older frontend sources, renderer
vendor code and UI proof scripts have been removed; git history retains them.

## Build and test

Windows builds require the pinned Rust toolchain, Visual Studio C++ Build Tools
and a Windows SDK. FFmpeg and FFprobe are required for media tests and exports.

```powershell
cargo build --locked -p desktop -j1
cargo test --locked -p desktop -p toolkit -p disk -p clean -j1
./scripts/test-package-windows.ps1
./scripts/verify-rune.ps1 -SkipBuild -GpuAdapter NVIDIA
```

On Linux, run `scripts/ci/build-windows-cross.sh setup test build smoke render`
on a disposable build VM. This cross-compiles Windows binaries and runs tests
and six Rune scene renders through Wine. It does not validate Windows hardware,
physical input, capture, accessibility or game compatibility. See
[validation scope](STATUS.md#validation-limits) and [Windows builds](WINDOWS_BUILDS.md).

## Source layout

- `crates/desktop/src/main.rs` registers the Rune application and scene fixtures.
- `crates/desktop/src/rune_*.rs` implement views, editor, capture, annotations,
  utilities, disk navigation, thumbnails and GPU preview.
- `crates/toolkit` provides screenshot, annotation and utility workers.
- `crates/editor-core` provides media editing, playback, export and recovery.
- `crates/windows-capture` and `crates/recording-tools` provide recording services.
- `crates/disk` and `crates/clean` provide analysis and developer cleanup.

Workers own long-running operations. Keep UI state changes on the UI thread,
preserve cancellation and shutdown ownership, and use synthetic media and isolated
working folders for tests. Do not treat historical native measurements as results
for the current Rune build. See [desktop status](STATUS.md) for remaining acceptance gaps.
