# Read-only installation check

The installation check explains missing media tools and data-location problems without starting recording, encoding, playback, or an audio endpoint. It does not upload, copy to the clipboard, save settings, create data folders, or load optional FFmpeg DLLs.

## Running it

The full diagnostics UI is not wired in Rune. Run the command-line check before the graphical application starts:

```powershell
.\rcr.exe --diagnose
.\rcr.exe --diagnose --human
.\rcr.exe --diagnose --data-folder D:\Captures
```

JSON is the default; `--human` prints concise summaries and actions. JSON also includes version, build profile, executable/working-directory paths, per-check status, bounded diagnostic details, and elapsed times. Review paths before sharing a report.

The CLI does not load saved preferences. Its capture-folder check defaults to `recordings` relative to the launch folder; pass `--data-folder` to inspect another location. Projects/settings remain relative to the launch folder.

Exit status is 1 for a blocking missing/unusable dependency, incompatible listed encoder support, conflicting data path, missing required debug shaders, or output failure. Warnings and unverified hardware/DLL checks alone do not make the command fail.

## What each result establishes

| Check | Establishes | Does not establish |
| --- | --- | --- |
| FFmpeg / FFprobe | An explicit `RCR_FFMPEG` / `RCR_FFPROBE` override or PATH candidate exists and successfully identifies itself with `-version`. A broken explicit override is not silently replaced. | Every filter/codec works, media can be decoded, or a GPU encoder can initialize. |
| AV1 NVENC listing | `ffmpeg -hide_banner -encoders` lists the exact video encoder token `av1_nvenc`. A description mentioning that name is insufficient. | GPU model/driver compatibility, encoder availability under load, or successful recording/export. These remain explicitly **Not tested**. |
| Data locations | The folder can be inspected, is missing, conflicts with a file, or is unavailable. | Write permission, free-space sufficiency, or successful saving. No write probe is performed. |
| Development shaders | Required shader source files exist at the build-time checkout location. Release builds report embedded shaders. | Cross-machine portability of a development executable or successful GPU rendering. |
| Optional native FFmpeg runtime | A configured library directory can be included as report context. | DLL presence, ABI compatibility, successful DLL loading, native playback eligibility, or a GPU test. Compatibility stays **Not tested**. |

Only `-version` and the encoder-list command are permitted tool operations. Windows tool candidates must be executable `.exe` files; no shell or script wrapper is invoked. The reused toolkit subprocess supervisor closes stdin, hides child console windows, caps stdout at 64 KiB and stderr at 4 KiB, and stops each command after three seconds. Encoder listings that hit the capture cap without finding the requested token are reported as inconclusive, not falsely missing. Cancellation stops outstanding tool work and skips subsequent data-location checks.

The timeout applies to subprocesses. Filesystem metadata/directory operations remain read-only worker operations; a disconnected or stalled filesystem can impose OS-level latency. The report does not promise a hard wall-clock bound for such filesystem calls.
