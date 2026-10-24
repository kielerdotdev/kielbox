# Offline GOP quality comparison

After `verify-gop-encoding.ps1` has completed, run:

```powershell
pwsh -File scripts/verify-gop-quality.ps1 -Summary '.downloads/gop-encoding-UUID/summary.json'
```

The input must be a successful, marked encoding fixture with GOP 120/60/30,
1,200 eight-bit frames per variant, 60 fps, matching dimensions and verified PTS.
The source must still match the encoding fixture's SHA-256. The script checks
that the fixture used the known source seek and normalization filter. For the
initial fixture, this selects source seconds 30–50; the start comes from the
summary rather than being hard-coded.

One sequential CPU FFmpeg job per variant computes PSNR and SSIM together.
Both inputs use `fps=60,format=yuv420p`, trim to 1,200 frames, then receive an
exact 1/60 time base and ordinal timestamps. The source uses the same input
seek as encoding; reference and derived frames pair by index. Framesync repeat
is disabled and the shorter input terminates comparison. Each metric must
contain exactly 1,200 consecutive sample indices, preventing a short reference
from silently being filled by repeated frames. NV12-to-planar YUV420P changes
plane layout without rescaling the image.

Results go only into a new `.downloads/gop-quality-UUID` directory with its own
marker. Inputs, the encoding summary and all three derived video files are
read-only. Their SHA-256 hashes are recorded before and rechecked in `finally`,
including failed/cancelled runs. Abrupt process termination or machine shutdown
cannot execute a script finalizer. No original, project, derived video or prior
quality artifact is overwritten. No encoded output video is produced.

Subprocess arguments use .NET's argument list, including paths with spaces.
FFmpeg is resolved from `RCR_FFMPEG` or PATH. The recorded tool version can
therefore be compared with the encoding tool version. Each job has a deadline
below 60 seconds, is killed/reaped on failure or cancellation, and drains both
logs asynchronously while retaining at most 1 MiB each. Per-frame statistics are
bounded by the 1,200-frame filter limit and separately checked below 1 MiB each.

`summary.json` includes FFmpeg's average PSNR/SSIM, minimum per-frame scores,
exact paired counts, command arguments, execution time, and hash checks.
Per-frame statistics and bounded diagnostic logs remain available. The second
PSNR field calculated from per-frame MSE is explicitly labelled as using rounded
statistics; FFmpeg's own average is the primary result. This avoids averaging
per-frame PSNR decibels, which is not the same aggregate measure.

These scores measure **additional compression relative to the already decoded
owned recording**, not fidelity to an uncompressed original. They do not replace
visual review of small text, scrolling and motion, especially around keyframes.
No quality threshold is imposed and capture defaults are not changed.
