# Seek proxy quality fixture

```powershell
pwsh -File scripts/verify-seek-proxy-quality.ps1 -Recording 'target/debug/recordings/owned.mkv'
```

Default input section: source seconds 30–36, 360 frames at 60 fps. The script
requires the known eight-bit SDR 60 fps source and does not record the desktop.
Three variants are generated sequentially with CUDA decoding/scaling:

| Variant | Maximum dimensions | Encoding |
| --- | --- | --- |
| Historical low-bitrate comparator | 1280×720 | H264 P1/ULL, all intra, CBR 8 Mbps |
| Isolate compression | 1280×720 | H264 P3/ULL, all intra, constant QP 18 |
| Detail plus bounded preroll | 1920×1080 | H264 P3/ULL, GOP 6, constant QP 18 |

All variants disable B frames, lookahead, multipass and AQ. Scaling preserves
aspect/SAR, rounds dimensions to an even number, and never upscales a small
source. `-IncludeGop12` adds an optional 1080p GOP-12 variant. QP mode has no
bitrate cap; benchmark file limits reject incomplete outputs rather than lowering
quality. These are candidate recipes, not production/default changes.

`-BalancedOnly` instead runs only three 1080p P3/ULL constant-QP candidates:
QP 24 / GOP 6, QP 26 / GOP 6, and QP 24 / GOP 12. This replaces the default
recipes, including any `-IncludeGop12` selection. Their lower storage cost must
be checked against text/detail quality, rather than assumed equivalent to QP 18.

`-SliceOnly` runs the 1080p/QP-26/GOP-6 and 1080p/QP-26/all-intra two-slice candidates. It takes
precedence over the other recipe-selection switches. In addition to the same
360-frame metrics and PNGs, a bounded stream-copy `trace_headers` job checks
twelve access units. Every sampled unit must contain two VCL
NALs and two distinct first-macroblock positions, with the first at zero. This
checks actual emitted slices rather than relying on the requested option; the
sample does not claim to scan the entire bitstream. All 360 probed keyframe
positions and the twelve sampled packet keyframe flags must match the requested
GOP (every sixth frame or every frame, respectively).
The header sample also records `slice_qp_delta` and initial slice luma QP from
the PPS base plus each slice delta. `-qp 26` is the requested base parameter;
FFmpeg's I-frame quantization factor/offset can make actual initial I-slice QP
different from P-slice QP. The benchmark leaves those defaults identical across
candidates rather than assuming that all slices use exactly 26. Header QP is
not a claim to have inspected every macroblock's quantization.

`-IntraBalancedOnly` takes precedence over all recipe switches and instead runs
only 1080p/all-intra/two-slice candidates with actual initial slice QP 24 and 26.
They explicitly set `-i_qfactor 1 -i_qoffset 0` alongside `-qp`, avoiding the
default I-frame adjustment. Both slices of all twelve sampled frames must have
the expected QP. This isolates the quality/storage tradeoff against the earlier
nominal-QP-26 all-intra candidate whose sampled initial I-slice QP was 21.
The resulting comparison informed the current all-intra/QP-24 recipe 7 below;
this benchmark switch itself does not change production cache budgets or recipe.

Each output must contain 360 aligned PTS samples at 60 fps. CPU FFmpeg then
computes PSNR/SSIM against the source downscaled to that output's actual
resolution, with both sides normalized to YUV420P and ordinal timestamps. Both
metrics require 360 consecutive paired samples with no last-frame repetition.
The reference uses software bicubic while preparation uses CUDA's scaling;
kernel differences contribute to the score. A higher score at 720p does not imply
more source detail than a 1080p picture, since the comparison references differ.

At relative frame 84 (source second 31.4 by default), the script exports reference
and proxy PNGs plus identical top-left-quarter crops at native pixel size. These
show actual decoded pixels with common RGB conversion, without enhancement,
annotation or compositing. Crop placement is deliberately explicit rather than
an inferred claim about text location. Inspect full images too, and judge motion
and dragging latency separately.

Artifacts stay inside a fresh marked `.downloads/seek-proxy-quality-UUID`
directory. All subprocesses are sequential, have deadlines below 60 seconds,
use proper .NET argument lists, and are killed/reaped on failure/cancellation.
Logs retain at most 4 MiB stdout and 1 MiB stderr per job while continuously
draining both pipes. Benchmark video outputs use a 120 MiB emergency size limit
and must remain below 128 MiB. Per-frame statistics stay below 1 MiB, PNGs below
16 MiB each, and all PNGs below 128 MiB combined (at most sixteen with optional D).
No input/project/earlier result is overwritten. Original SHA-256 is checked in
`finally`, including failure/cancellation; abrupt host termination cannot run a
finalizer.

`summary.json` records actual encoder arguments, tool versions, PTS/keyframes,
dimensions, six-second sizes, metric averages, diagnostic image paths, and
estimates for the full source duration. Full-source size is content dependent:
the short section is an estimate, not a guarantee of fitting a cache budget.
Production acceptance must retain a hard file cap and fall back to the original
instead of silently retrying at a lower quality.

Rate-control rationale follows the [FFmpeg NVENC option documentation](https://ffmpeg.org/ffmpeg-codecs.html#NVENC-encoders)
and [NVIDIA encoder guide, rate control](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.1/nvenc-video-encoder-api-prog-guide/index.html).
Constant QP specifies quantization rather than bitrate. Target-quality VBR can
still lose requested quality when its maximum bitrate is restrictive; avoiding a
low bitrate ceiling is intentional in this comparison.

## Candidate recipe 7: all-intra, larger disk budget

The current candidate is 1080p maximum, constant QP 24, all-intra H264 P3/ULL,
with two slices per picture, explicit bicubic interpolation and no enlargement
of smaller sources. `-i_qfactor 1 -i_qoffset 0` keeps initial intra-slice QP at
24 instead of applying NVENC's default intra quantizer adjustment. Every frame
is independently decodable, eliminating inter-frame dependency preroll. This
improves the pixels used while dragging; final release is not the only quality
path. It remains a lossy derivative, not source-identical footage. Recipe 7
invalidates earlier copies. The actual 119.566-second source produced a complete
published 1,235,247,828-byte copy. The aligned six-second actual-QP-24 section
used 62,217,909 bytes and measured 41.010379 dB / 0.973697 SSIM against its
1080p resized reference. These are additional-compression measurements against
an already-compressed source, not lossless fidelity.

The earlier QP-26/GOP-6 candidate completed the 119.566666-second recording in
544,723,460 bytes, but actual counters still showed one through six decoded
frames per seek. Two-slice/two-thread decoding did not meet the latency target:
the traced full-source run had a 138.87 ms final response and 33 paints in two
seconds. Later controls identified synchronous trace logging as a substantial
performance perturbation; a whole-line single-write fix subsequently passed a
traced control on the rebuilt normal executable (114 paints/two seconds,
46.5556 ms final response). The writer remains synchronous. A default untraced full-source recipe-7 control
produced 116 changing paints in two seconds, 29.8348/46.4142/48.1233 ms
median/p95/maximum request-to-paint and a 28.8026 ms exact final response.
Two further untraced controls also passed, with 111/114 changing paints and
27.8404/28.7999 ms final responses. These desktop UI paint-call timings exclude GPU
completion and monitor scanout. Historical receipts and trace-enabled failures are in the recipe-7 entry of
`docs/VALIDATION.md` in git history.
The former 720p/8 Mbps derivative visibly lost text detail. Its 35.543 dB /
0.896955 SSIM and the earlier 1080p QP-26/GOP-6 score of 41.3973 dB / 0.977515
use different resized references, so native-pixel crops remain necessary.

The larger disk allowance trades storage for fewer decoded frames per seek;
decoded-frame memory limits are unchanged. Decoder threads default to one.
The optional bounded two-thread slice experiment must explicitly request
`thread_type=slice`; frame threading can introduce lookahead. NVENC chooses
slice division, so exact geometric halves are not assumed. Cross-slice filtering
stays enabled. Compare metrics, crops and one-versus-two-thread latency before
selecting the decoder setting.

A completed copy must remain below 2 GiB; the total cache budget is 3 GiB.
Preparation reserves 2 GiB plus 16 MiB overhead and requires that reservation
plus 512 MiB free (2,576 MiB) before starting. The FFmpeg emergency size limit
is 2 GiB plus 8 MiB, above acceptance,
so a file stopped by that limit is rejected. The 90-second encoder and
120-second overall deadlines remain, as do source identity checks, exclusive
encoder/index locks, active-reader leases and process cleanup. Eligible sources
remain capped at four minutes, with actual bytes/time authoritative. A size or
time failure falls back to original decoding instead of retrying at a higher QP,
reduced resolution or fixed low bitrate. The six-second size projection
does not establish that every full recording fits; full-source acceptance and
dragging latency must be measured separately.

A pinned 1,235,247,828-byte copy plus the full new preparation reservation
exceeds the 3 GiB cache budget. Another source therefore cannot prepare while
that reader lease remains active; it retains original decoding even if both
eventual copies could fit. Active readers are never evicted to admit preparation.

`RCR_TRACE_SEEK` enables preparation lifecycle/error events and at most one
progress event per second, including byte counts, actual encoder arguments,
exit status and the final retained 16 KiB of stderr. Ordinary untraced operation
uses null stderr and creates no diagnostic reader thread. Traced stderr is
continuously drained into a bounded ring. Teardown kills/reaps the child and
closes its Windows job before checking the reader; it joins only an already
finished reader after successful child reaping. If an override wrapper spawned
an out-of-job descendant still holding stderr, teardown detaches the reader
instead of blocking the seek worker. That reader retains a shared encoder lease
until it finishes, preventing repeated encoder/reader admission. Its tail is
snapshotted with a nonblocking lock and the trace marks detachment explicitly.
