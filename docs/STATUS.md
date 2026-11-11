# Desktop status

Kielbox uses Rune as its only desktop frontend. This is an experimental Windows
preview with incomplete feature parity. The native backends retain existing
recording, project, settings and installation formats.

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
| Packaging | Application-owned DPI manifest, Rune/font licenses, embedded shader metadata; no separate FXC build step |

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
- Native UI interaction proofs still need broader Rune coverage. Unsupported old proof flags
  must not be interpreted as successful Rune tests.

## Validation limits

Cross-builds, Wine tests and software-rendered scenes do not establish native
Windows behavior. Before treating a build as ready for daily use, verify:

- Install, update and uninstall with existing data; interrupt updates and saves.
- Capture, replay, audio sync, device loss and GPU preview on supported hardware.
- Screenshot clipboard/save failures, editable recovery and quit during delivery.
- Native dialogs, keyboard navigation, IME, screen readers and mixed-DPI monitors.
- Disk accounting and cleanup cancellation on disposable fixtures.

See [Development](DEVELOPMENT.md) for repeatable checks and
[Windows builds](WINDOWS_BUILDS.md) for packaging and installer validation.
