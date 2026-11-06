# Local utilities

## Disk usage

For developer artifact cleanup, use the separate [clean CLI](../crates/clean/README.md):
`clean C:\src --apply` removes eligible dependencies, build output, project
caches and Git worktrees older than 30 days. Omit `--apply` to preview. It shares
the disk scanner with the desktop explorer and runs without the UI.

**Explore** pairs a sunburst or large treemap with a ranked folder list. Click a folder sector or list row to enter it; click the sunburst center, **↑**, or a breadcrumb to go back. Hovering a sector or row previews its size and counts. Branch colors connect the sunburst with the list, which retains native Windows file-type icons. Chart work is bounded to five rings and 512 sectors; remaining entries keep their proportional area in grouped sectors, while the list retains every item. This interaction takes inspiration from [SquirrelDisk](https://github.com/adileo/squirreldisk).

Open **Tools → Disk usage**, choose a drive or folder, and scan it. **Tree View** shows compact size-sorted rows with parent-percentage bars, logical size and file/folder counts beside the file-type table. **File View** shows the largest individual files and their paths across the full width; clicking a file type filters this view. Expand tree rows or double-click folders to explore them; **↑** and **Root** navigate back. **Reveal** and **Copy path** act on the selection. **Stop** keeps partial results; **Rescan** refreshes them; **Clear** releases results from memory.

Tree View shows the nested treemap below the tables; File View gives its table the full height by default. Each view remembers its own **Treemap** toggle during the session. Folder frames contain descendant files colored by extension. Rendering uses a bounded detail budget and groups remaining entries into directory tiles; the tree and file lists retain every scanned entry. Rows are 21 pixels high, and both tables are virtualized.

File rows and the extension table use Windows' registered file-type icons; tree folders use native closed/open folder icons. Icon lookups run on a separate worker and use bounded caches. The extension table retains its color swatches as the treemap legend.

Scanning runs on a cancellable worker; file lists are virtualized. Results use logical file lengths, counting hard links per path. They exclude links, junctions and offline/recall files, so they can differ from allocated disk space or a drive's used-space total. Error counts and a copyable report explain failures. It does not delete files.

For whole local NTFS drives, run RCR as administrator to enable **NTFS MFT** scanning. This reads file records and lengths in sequential batches, then assembles the directory tree without opening each file. The status bar identifies the backend actually used. Folders, other filesystems and inaccessible raw volumes use **Windows directory batches**, obtaining names, sizes and attributes together rather than querying each file separately. Other platforms retain ordinary directory traversal. Raw NTFS reads do not lock or modify the volume; a changing filesystem can still produce an inconsistent snapshot. Unsupported MFT layouts fall back to directory scanning.

For repeatable timing in PowerShell, `cargo run -p toolkit --example disk_scan_bench -j1 -- 'C:\'` prints backend, elapsed time and totals. Add `--directory` to force the fallback. Use the same root and account privileges when comparing runs; filesystem caches affect timings.

You can also launch `rcr.exe --analyze-folder "C:\Users"`. `scripts/verify-disk-usage.ps1` scans a small owned fixture and captures the app's GPUI render target at two window sizes without desktop automation. Layout follows the linked-list/treemap approach described by [WinDirStat](https://github.com/windirstat/windirstat/wiki/Reading-Results); logical versus allocated sizes and privileged scanning are explained in the [WizTree FAQ](https://diskanalyzer.com/faq).

## Screenshot and file utilities

Open **Tools → Utilities** or use the tray's **Tools** menu. Select a utility, choose inputs, set its options and click **Run**. Image results offer preview, copy, save, edit and pin actions. Text results offer copy and save; long results show a bounded preview while copy/save retains the complete result. Generated files can be revealed in their folder.

| ShareX tool | RCR implementation |
| --- | --- |
| Color picker | Hex/RGBA input, color preview, RGB/HSL/HSV values |
| Screen color picker | Frozen-desktop overlay; click copies `#RRGGBB`; Esc cancels |
| Ruler | Drag on a frozen desktop; physical X/Y, width/height, distance and angle; Enter finishes |
| Pin to screen | Select a region or pin an image result; independent topmost movable window with zoom and Esc/close |
| Image editor | Existing 25-tool annotation editor; open files/projects or edit a utility result |
| Image beautifier | Padding, background color, corner radius and soft shadow |
| Image effects | Grayscale, sepia, invert, blur, pixelation, sharpen, brightness, contrast, saturation, hue, gamma, threshold, rounded corners, border and shadow |
| Image viewer / converter | Preview, copy, edit, pin and save PNG/JPEG/WebP/BMP |
| Image combiner | Vertical/horizontal layout, input order, gap and background color |
| Image splitter | Columns × rows; every edge pixel retained; new unique output folder |
| Image thumbnailer | Maximum width/height with aspect ratio preserved and no upscaling |
| Video converter | MP4/H.264/AAC, WebM/VP9/Opus, GIF with FPS/width controls; all audio tracks mapped in MP4/WebM |
| Video thumbnailer | Timestamp in seconds and maximum width; PNG preview/copy/save |
| OCR | Local Windows OCR from a file or selected screen region |
| QR code | Text/URL creation; QR reading from a file or selected screen region |
| Hash checker | Streaming CRC32, MD5, SHA-1, SHA-256, SHA-384, SHA-512; expected-digest comparison |
| Directory indexer | Text/HTML/JSON/XML, recursion depth, relative paths and file sizes |
| Clipboard viewer | On-demand image, text and file-list reading |
| Inspect window | Window titles, handles, PID, class, position and size |
| Borderless window | Explicit selected-window border removal; Restore windows restores saved styles and geometry |
| Monitor test | Fullscreen black, white, RGB and gray colors; click/key cycles; Esc closes |

Screen selections stay open after release; Enter confirms and Esc cancels. Screen tools capture before showing the overlay, so its controls are not part of the sampled image. Coordinates and measurements refer to physical source pixels. Mixed-DPI/HDR display configurations still need interactive verification on the target machine.

OCR requires an installed Windows OCR language pack. FFmpeg must be installed for video tools. Utilities do not upload their results. Image outputs use new filenames; video conversion publishes the completed temporary output without overwriting an existing destination. Failed/cancelled video conversions do not publish a partial final file. A cancelled image split removes its temporary tile folder. Protected/elevated windows can refuse border/style changes. Original styles are retained in the current app session and restored when the restore action runs or its state is dropped; forcibly killing the app cannot run restoration.

Image inputs/outputs are limited to 32 megapixels, image files to 256 MiB, combiner inputs to 128 MB of decoded pixels, grids to 1000 cells, and indexes to 100,000 entries / 128 nested levels. Indexes skip hidden entries, symlinks and Windows reparse points. Thumbnail/preview sizing preserves aspect ratio. Raster kernels use a two-thread worker pool; FFmpeg decoder/encoder and filter thread counts are bounded.

This implements the straightforward tools in the supplied ShareX menu, not complete ShareX product parity. DNS changing, configurable utility hotkeys, batch profiles, full ShareX effect presets and persistent clipboard history remain pending. Windows-specific OCR and window utilities need native Linux adapters later.

Behavior references: [ShareX tools](https://getsharex.com/), [directory indexer](https://getsharex.com/docs/index-folder), [Windows OCR](https://learn.microsoft.com/en-us/uwp/api/windows.media.ocr.ocrengine).

```powershell
cargo test --workspace -j1
cargo clippy --workspace --all-targets -j1 -- -D warnings
cargo run -p toolkit --example utility-proof -j1 -- recordings/mux-audio-fixture-001.mkv
.\scripts\verify-toolkit.ps1 -Executable target/debug/desktop.exe
```

The proof uses synthetic image/text fixtures and the existing synthetic two-track recording. Native smoke tests use hidden windows with synthetic pointer/key events; they never capture the real desktop or write the system clipboard.
