# Local utilities in Rune

Updated after the 11 November 2026 implementation and Boat validation. These are
current source/build capabilities; native Windows device, clipboard, dialog and
accessibility acceptance remains open. See the [implementation tracker](../plans/implementation-tracker.md).

## Disk usage

Open **Tools → Disk usage → Scan folder…**. The scan is read-only. Navigate
folders, use the 100-entry pages to reach every child, and use **Reveal** or
**Copy path** for files. The summary retains backend/fallback information,
sampled errors, cancellation state and accounting details after completion.

Sizes are logical file lengths, with hard links counted by path. Links,
junctions and offline/recall files are skipped; errors or cancellation can leave
incomplete totals. They are not allocated sizes or a drive's used-space total.
The overview groups small entries; the paged list retains access to every child.

Developer-artifact cleanup is the separate [clean CLI](../crates/clean/README.md).
Preview first. `--apply` permanently deletes eligible artifacts. Worktrees with
ignored data are preserved unless `--allow-ignored-worktree-data` explicitly
opts into deleting it. Age does not prove disposability. Cancellation returns
completed removals in the JSON ledger and exits 3.

## Screenshot and file utilities

Choose a tool from **Tools** or its search. Each tool has task-specific fields,
units and defaults. Run starts work; Cancel requests cancellation. Switching tools
cancels the old operation and prevents its result from appearing in the new tool.
Image/text results offer appropriate copy/save actions and generated files offer
Reveal/Copy path. Failed saves retain the generated result for retry.

| Tool | Current behavior |
| --- | --- |
| Color picker | Typed hex/RGBA conversion, not screen sampling |
| Window inspector | Window enumeration, not process monitoring or borderless control |
| Text from image / Read QR code | File input or Capture region; Use capture attaches the reviewed saved image to the originating tool |
| Clipboard viewer | Explicit read of current contents, not clipboard history |
| Image viewer / Convert | Preview; save PNG/JPEG/WebP/BMP by chosen extension |
| Beautify / Effects / Combine | Padding/radius, selectable effects, orientation and input ordering |
| Split | Columns/Rows, existing output folder, unique child directory |
| Thumbnails | Aspect-preserving image fit without upscaling |
| Convert video | MP4, WebM or GIF, with format-specific options |
| Video thumbnails | Timestamp in seconds and maximum width |
| Make QR code | Text/URL to local QR image |
| File hash | Streaming digests; optional expected digest starts blank |
| Folder index | Folder picker, depth and text/HTML/JSON/XML selection |

OCR requires an installed Windows language pack. Video tools need FFmpeg;
editing/probing also need FFprobe. Utilities do not upload output. Image/video
publication refuses existing destinations. Split and conversion use temporary
outputs rather than publishing partial final files. Screen sampler, ruler,
pinning and borderless controls in the historical guide are not current Rune UI.

## Captures and editable images

Region **Copy** writes only to the clipboard. **Save**, **Save elsewhere…** and
**Edit** are explicit disk actions. A failed save keeps the frozen image and
annotations for retry. Arrow keys move/create a region, Alt resizes and Shift
steps ten pixels. With an annotation tool selected, arrows position it and Enter
begins/ends placement; brackets select annotations and V returns to region mode.

Editable SSXE projects and recovery checkpoints retain original pixels and
annotations, including covered/cropped material. Review a flattened image for
sharing; blur/pixelation is not a verified secure-redaction guarantee. Leaving an
unsaved image queues a recovery checkpoint and requires its acknowledgement
before close/replacement/quit. Retry the action after the checkpoint completes.
A crash before a checkpoint is requested is not covered. Recovery sidecars move
with their source into Trash and are not separate normal Library captures.

## Validation boundary

Boat passed 268 Windows/Wine tests and 19 Linux disk/clean tests; three Windows
entries were ignored. Release build, smoke and six scene renders passed. Wine
software rendering is not native GPU, capture, clipboard, Narrator or game
acceptance. Full commands and ignored-test reasons are in the
[validation report](../plans/implemented-validation.md).

---

The following guide is retained as historical provenance for the previous GPUI
frontend and shared backend contracts. Its UI instructions, capability table and
proof commands are **not current Rune instructions**. Consult the current
section above before following a task.

# Historical GPUI utility guide

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
