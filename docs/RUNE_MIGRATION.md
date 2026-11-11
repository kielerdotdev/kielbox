# Rune desktop migration

The default `desktop` binary now uses Rune, pinned to revision
`a9da7e3ebf5a8ab9bf19c3015032674b6e042df2`. GPUI is absent from its Cargo
dependency graph, and the workspace no longer patches or builds the vendored
GPUI crate. This is a working replacement frontend, **not complete feature
parity with the previous desktop**.

This inventory includes the 11 November 2026 audit fixes on baseline `d96346b`.
Boat cross-build/Wine validation passed; native hardware gates remain open in
[the implementation tracker](../plans/implementation-tracker.md).

The native media, recording, annotation, disk, settings, installation and update
backends retain their existing formats. The new entry point is `src/main.rs`;
views and adapters are in `crates/desktop/src/rune_*.rs`. Older unreferenced GPUI
view/proof files and the vendor directory remain as migration references. They
are not compiled or linked. Git retains the previous implementation.

## Current Rune implementation

| Area | Rune frontend |
| --- | --- |
| Shell | Six pages, keyboard focus, native file dialogs, existing tray and global shortcut service |
| Library | Background scanning, imports, search, filters, favorites, open, 100-item paging, persistent Trash recovery, visible-page thumbnails with video length |
| Capture | NVIDIA AV1 recording, instant replay/save, display/audio selectors, saved endpoint identities, GPU preview |
| Screencast | Existing WGC/FFmpeg worker, desktop recording, pause, stop and finalization |
| Screenshots | Full desktop and active window save straight to the library. Region capture freezes the monitor under the pointer in a Rune overlay: drag to select, move/resize it, annotate with the toolbar under the selection, then copy, save or open in the editor |
| Video editor | Load/validate existing edits, preview/playback, frame seeking, split/delete, drag-to-trim grips with ripple (one undo step), audio mute, undo/redo, autosave and MP4 export |
| Images | Existing annotation actor, all tool choices, drag input, text labels, crop, rotate, undo/redo, image/project save |
| Disk | Existing cancellable directory/NTFS scanner, progress, directory navigation and size bars |
| Utilities | 17 task-specific forms, capture handoff, result-bound copy/save/reveal and format selection |
| Settings | Capture folder, resolution/FPS, audio flags, replay microphone cleanup, tray behavior, shortcuts and updates |
| Packaging | Application-owned DPI manifest, Rune/font licenses, embedded shader metadata; no GPUI FXC build step |

Workers own media/filesystem operations. Screenshot failures retain their
session; Copy is clipboard-only. Image close/replacement/quit waits for an editable
recovery checkpoint. Video saves, utility workers and delivery workers remain
owned through shutdown. Native lifecycle acceptance is still required.

## Remaining parity work

- Custom-name editing, sort controls, grid/list layout,
  and watch queues. Paging and persistent Trash recovery are now implemented.
- Application-audio selectors and capture recovery presentation,
  quick-recorder window/region targets, countdown and floating controls. Existing
  saved recording display and endpoint identities are still honored.
- Dragging/moving timeline pieces, speed/gain controls, waveforms,
  multi-clip joining, export presets, native GPU video playback, watch/share/fullscreen
  windows and editor-session caching. Playback currently uses the existing CPU path.
- Annotation selection overlays, clipboard/IME integration, detailed style controls,
  insert-image/sticker dialogs and configured screenshot/upload workflows. Source
  annotation models remain available, but the full former workflow UI is not wired.
- Last-region recall, screen ruler/pin/color overlays and radial/treemap disk views.
  OCR/QR capture handoff and utility-specific forms are implemented. Region
  screenshots already freeze the monitor under the pointer; this is not pending.
- Full setup diagnostics UI. Installed first run now opens Storage guidance and
  Use existing data selects an existing root for the next launch. Portable builds
  retain `--working-directory`. Capture folder is a separate setting.
- The old GPUI native UI proofs need Rune equivalents. Unsupported old proof flags
  must not be interpreted as successful Rune tests.

## GPU ownership

`rune_gpu.rs` opens the producer's keyed-mutex D3D11 texture on a separate copy
device on the same adapter. It copies into three named BGRA textures, publishes
only after a nonblocking completion query succeeds, and reuses a published slot
only after Rune signals its shared release fence. No screen-pixel CPU readback is
introduced. Preview errors are reported separately from recording errors.

Rune's headless renderer defaults to a low-power adapter. On a hybrid system,
choose the producer's adapter explicitly for the generated GPU proof. A named
texture/fence import failure on another adapter is not a valid preview result.

## Validation

Boat validation passed 268 Windows/Wine tests, 19 Linux tests, release build,
smoke and six render scenes. Three Windows tests remain ignored; see the
[execution ledger](../plans/implemented-validation.md). Native Windows/GPU,
installer and accessibility acceptance remains separate. The following commands
remain useful native gates; no local compilation was used for this work.

```powershell
cargo build --locked -p desktop -j1
cargo test --locked -p desktop -j1
./scripts/test-package-windows.ps1
./scripts/verify-rune.ps1 -SkipBuild -GpuAdapter NVIDIA
```

On Linux, `scripts/ci/build-windows-cross.sh` (the `Windows cross build` workflow)
cross-compiles the Windows release with cargo-xwin, runs the Windows tests of `disk`,
`clean` and `desktop` under Wine (with pinned Windows FFmpeg and MinGit), smoke-tests the
executables and renders every page of `desktop.exe` through Wine's Vulkan on Mesa
lavapipe. It does not cover installer, capture or GPU-preview behaviour.

The scene checks render PNGs and element trees with isolated default state:
they do not load personal preferences, enumerate captures, register shortcuts,
record audio, or capture the screen. The GPU scene uploads a generated checkerboard,
checks submitted/completed draws, and verifies the resulting green/purple pixels.
Evidence is written beneath `.downloads/rune-proof-*`.

Historical initial migration verification reported 146 passing desktop tests (3 opt-in tests ignored), 39
packaging assertions, all six page renders, and a 61-frame generated
D3D11-to-Rune proof on an NVIDIA RTX 5070, including checkerboard pixel checks.
These checks do not establish live capture/recording parity, native IME correctness,
release-installer acceptance, or performance equivalence with the former UI.

The November 11 audit separately ran 14 disk and 2 clean integration tests on
Boat Linux at `d96346b`, plus `clean --help`. Those results predate the fixes and
prove no Windows UI behavior. Preserve the [execution ledger](../plans/qa-vm_validation.md)
and [final audit](../plans/qa-final.md) as historical evidence.
