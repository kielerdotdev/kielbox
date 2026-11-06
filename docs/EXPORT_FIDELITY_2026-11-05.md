# Export fidelity and publication evaluation — 2026-11-05

The actual FFmpeg export graph now preserves samples at normal speed. The
previous `atempo=1` stage applied WSOLA even to an identity operation and altered
the end of the generated reference. Normal speed now uses `anull`; non-unit
speeds retain the tempo filter. Native preview uses the same identity rule.

The independent 48 kHz stereo reference checks full mix, trimmed gain, mute,
solo, and trimmed fades. All five actual filter executions passed: four had
zero maximum sample error and the fade case had 2.98e-8, below the unchanged
2e-5 threshold. Logs retain both the failed pre-fix reference and the passing
result under `.downloads/plan-continuation/export-fidelity-{before,after}.log`.
Five existing native audio reference/parity fixtures also passed after the fix;
unsupported trim/origin cases keep their explicit fallback rather than silently
expanding native eligibility.

The new optional H.264/AAC MP4 export was exercised on the RTX 5070 using the
real NVENC encoder, a generated source and an independent lossless trimmed
graph. Full decode returned 60 frames over one second, 48 kHz stereo AAC, and
SSIM 0.987799 against the lossless graph (required minimum 0.98). Original source
bytes and project state were unchanged; an existing destination remained intact.
Evidence: `.downloads/plan-continuation/h264-export-proof.log`. These are media
processing checks, not a Discord upload or a guarantee of any service's size cap.

Immediately before publishing an export, the worker now rechecks the original
asset paths against the initial source stamps. Changed or missing media prevents
publication, leaves the partial recoverable, and preserves an existing output.
Unit regressions also cover cancellation at this boundary. Source stamps use
filesystem metadata, not a whole-file hash; identical metadata replacement and
the final filesystem race are outside this check's guarantee. Publication still
uses the existing non-clobber operation.

The ordinary editor export retains AV1 by default. The watch-mode action opts
into H.264/AAC using a cloned project, preserving edit history and original media.
Its layout and fullscreen proofs are recorded separately in the watch audit.
