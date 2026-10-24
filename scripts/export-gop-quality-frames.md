# GOP quality diagnostic frames

```powershell
pwsh -File scripts/export-gop-quality-frames.ps1 -QualitySummary '.downloads/gop-quality-UUID/summary.json'
```

The default zero-based frames are 780, 959 and 960. In the initial quality run,
frame 780 is the largest measured GOP-30 SSIM drop relative to GOP-120:
0.968724 versus 0.980633 (difference 0.011909). It is a GOP-30/GOP-60 keyframe,
halfway through the GOP-120 interval. Frame 959 exercises the frame just before
their shared keyframe at 960; GOP-30 scores slightly better than GOP-120 at 959.
Selection is diagnostic and deliberately includes a measured adverse case.

Each source/variant job exports at most three PNGs, sequentially, with a deadline
below 30 seconds. The source extraction uses the encoding summary's original
input seek (30 seconds in the initial fixture), fps normalization and frame
ordinal alignment. The derived files must still match the completed quality
run's SHA-256 hashes. There is no screen capture or new video encoding.

Four input files produce twelve PNGs by default. Results go into a fresh marked
`.downloads/gop-frames-UUID` directory. Names ending `-01`, `-02`, `-03` correspond
to the ascending frame indices in `manifest.json`; the manifest includes exact
fixture/source timestamps. PNG signatures and dimensions are checked. Limits
are twelve PNGs, below 16 MiB each and 96 MiB combined, with 1 MiB retained per
stdout/stderr log. Source/derived hashes are rechecked in `finally`, including
failure/cancellation; inputs and earlier artifacts are never overwritten.
Abrupt host termination cannot execute a script finalizer.

The images use identical FFmpeg YUV420P-to-RGB conversion with no resizing,
enhancement, compositing or annotation. Inspect them at 100% to compare text,
edges and texture, and also review motion separately. A single frame or metric
cannot establish overall subjective capture quality.

An optional identical crop is supported with `-Crop 'width:height:x:y'`, for
example `-Crop '960:540:0:0'`. Choose its position after reviewing the full frame;
the script does not infer where useful text is located. Cropping happens after
RGB conversion, so integer crop coordinates are not rounded to chroma blocks.
Optional `-Frames` accepts one to three fixture indices when invoked from a
PowerShell session. Existing encoding/quality scripts and capture defaults are
unchanged.
