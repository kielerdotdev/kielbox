# Offline capture GOP comparison

Run with PowerShell 7 and an existing owned 60 fps recording:

```powershell
pwsh -File scripts/verify-gop-encoding.ps1 -Recording '.downloads/owned-recording.mkv' -StartSeconds 0
```

The script reads the recording and writes only into a fresh UUID directory under
`.downloads/gop-encoding-*`, marked `.rcr-gop-encoding`. It never opens Desktop
Duplication, changes capture settings, edits a project, or overwrites media.
It leaves artifacts for review and performs no automatic deletion.

Three sequential AV1 Main encodes use the same 1,200 frames (20 seconds at 60 fps)
with GOP lengths 120, 60 and 30. Explicit settings match capture's P3/low-latency,
VBR average 25 Mbps / maximum 37.5 Mbps, no B frames, lookahead or spatial/temporal
AQ, disabled multipass, and BT.709 limited-range tags. This is **VBR, not CBR**.
The common filter normalizes timestamps, pixel format to NV12 and cadence to
60 fps. Use an SDR source matching capture; do not use this as an HDR comparison.

VBV size and other unlisted parameters inherit FFmpeg's NVENC preset defaults.
Capture inherits its own preset defaults too; their equality has not been
established. Accordingly, this isolates the GOP effect within the FFmpeg fixture
and does not claim bit-for-bit equivalence with native capture. AV1 CBR bitstream
padding differences are irrelevant because every variant uses VBR. More frequent
keyframes may change quality and actual bitrate at this target/max pair; neither
equal sizes nor an automatic size increase is assumed.

Each encoder/probe has a deadline below 60 seconds; cancellation or failure kills
and reaps its child. Encoder output uses a 120 MiB emergency `-fs` limit, leaving
headroom below the separately checked 128 MiB acceptance ceiling. Stdout/stderr
are drained asynchronously with retained prefixes capped at 4 MiB / 256 KiB per
job, so verbose subprocesses cannot fill memory or create unbounded logs. No log
contents are printed automatically. Tools come from `RCR_FFMPEG` / `RCR_FFPROBE`,
or application commands on PATH; arguments use .NET's argument list, including
paths containing spaces.

`summary.json` records encoder arguments, elapsed time, sizes/container bitrate,
stream metadata, actual frame/keyframe counts and positions, PTS endpoints, and
maximum keyframe gap. Full bounded probe JSON retains every frame's PTS and
dimensions. Validation requires 1,200 frames, unchanged dimensions, PTS within
1.1 ms of the 60 fps grid (Matroska timestamp rounding), initial keyframe, and no
keyframe gap beyond the requested GOP. The original SHA-256 is rechecked in
`finally`, including failed/cancelled runs; abrupt process termination or machine
shutdown cannot execute a script's finalizer.

Run native seek latency comparisons only after the encoders exit. Keep the seek
trace and decoder options identical across variants and measure backward jumps
and frames immediately before keyframes. Compare image quality separately before
changing capture defaults. This script deliberately runs no PSNR/SSIM jobs.
