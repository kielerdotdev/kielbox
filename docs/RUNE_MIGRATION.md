# Rune desktop migration

The default `desktop` binary now uses Rune, pinned to revision
`a9da7e3ebf5a8ab9bf19c3015032674b6e042df2`. GPUI is absent from its Cargo
dependency graph, and the workspace no longer patches or builds the vendored
GPUI crate. This is a working replacement frontend, **not complete feature
parity with the previous desktop**.

The native media, recording, annotation, disk, settings, installation and update
backends retain their existing formats. The new entry point is `src/main.rs`;
views and adapters are in `crates/desktop/src/rune_*.rs`. Older unreferenced GPUI
view/proof files and the vendor directory remain as migration references. They
are not compiled or linked. Git retains the previous implementation.

## Implemented

| Area | Rune frontend |
| --- | --- |
| Shell | Six pages, keyboard focus, native file dialogs, existing tray and global shortcut service |
| Library | Background scanning, imports, search, favorites, open, trash and undo |
| Capture | NVIDIA AV1 recording, instant replay/save, saved audio endpoint identities, GPU preview |
| Screencast | Existing WGC/FFmpeg worker, desktop recording, pause, stop and finalization |
| Screenshots | Full desktop, active window and region; region selection uses the existing native Win32 overlay |
| Video editor | Load/validate existing edits, preview/playback, frame seeking, split/delete/trim, audio mute, undo/redo, autosave and MP4 export |
| Images | Existing annotation actor, all tool choices, drag input, text labels, crop, rotate, undo/redo, image/project save |
| Disk | Existing cancellable directory/NTFS scanner, progress, directory navigation and size bars |
| Utilities | Existing 17 utility backends, file/text inputs, result previews and output selection |
| Settings | Capture folder, resolution/FPS, audio flags, replay microphone cleanup, tray behavior, shortcuts and updates |
| Packaging | Application-owned DPI manifest, Rune/font licenses, embedded shader metadata; no GPUI FXC build step |

Workers own media/filesystem operations. Edit saves drain on shutdown; utility
and export workers are cancelled/joined, file operations are joined, and native
recording workers finalize through their existing stop paths.

## Remaining parity work

- Library thumbnails, custom-name editing, sort/filter controls, grid/list layout,
  persistent trash recovery, watch queues and pagination beyond the first 500 matches.
- Display/application-audio/device picker interfaces, capture recovery presentation,
  quick-recorder window/region targets, countdown and floating controls. Existing
  saved recording display and endpoint identities are still honored.
- Dragging/moving timeline pieces and trim grips, speed/gain controls, waveforms,
  multi-clip joining, export presets, native GPU video playback, watch/share/fullscreen
  windows and editor-session caching. Playback currently uses the existing CPU path.
- Annotation selection overlays, clipboard/IME integration, detailed style controls,
  insert-image/sticker dialogs and configured screenshot/upload workflows. Source
  annotation models remain available, but the full former workflow UI is not wired.
- Last-region recall, monitor-under-cursor behavior, screen ruler/pin/color/OCR/QR
  overlays, utility-specific option forms, and the radial/treemap disk views.
- Installed-app welcome/data-folder migration controls and full setup diagnostics UI.
  Startup data-root resolution, the diagnostics CLI and isolated installer smoke
  commands remain implemented.
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

```powershell
cargo build --locked -p desktop -j1
cargo test --locked -p desktop -j1
./scripts/test-package-windows.ps1
./scripts/verify-rune.ps1 -SkipBuild -GpuAdapter NVIDIA
```

The scene checks render PNGs and element trees with isolated default state:
they do not load personal preferences, enumerate captures, register shortcuts,
record audio, or capture the screen. The GPU scene uploads a generated checkerboard,
checks submitted/completed draws, and verifies the resulting green/purple pixels.
Evidence is written beneath `.downloads/rune-proof-*`.

Initial verification passed 146 desktop tests (3 opt-in tests ignored), 39
packaging assertions, all six page renders, and a 61-frame generated
D3D11-to-Rune proof on an NVIDIA RTX 5070, including checkerboard pixel checks.
These checks do not establish live capture/recording parity, native IME correctness,
release-installer acceptance, or performance equivalence with the former UI.
