# Local utilities in Rune

See [desktop status](STATUS.md) for feature gaps and native validation limits.

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
