# Actual screenshot capture evaluation — 2026-11-05

Five sequential actual desktop captures passed on the unlocked Windows host through the current production `ScreenSource::detect(TonemapSettings::default())` and `capture_desktop` path. The selected backend was **windows-wgc**, with two displays composited into **5120×1440 RGBA8**. Capture median was **124.74 ms**, with a first-run capture of **286.69 ms**. These are current-path measurements, not a before/after speedup or full screenshot UI latency claim.

The read-only availability preflight and exact metadata report are preserved at:

- `.downloads/plan-continuation/screenshot-availability.json`
- `.downloads/plan-continuation/screenshot-hardware.json`

Backend detection took **46.8650 ms**; explicit monitor enumeration took **1.5641 ms**. The detected source was reused for all five captures, so initialization is measured once and is not included in each capture time. Internal display enumeration/compositing and default tone mapping performed by `capture_desktop` remain included in its measured duration.

| Run | Actual capture | Separate full-image analysis |
| --- | ---: | ---: |
| 1, initial capture | 286.6926 ms | 140.5217 ms |
| 2 | 123.0176 ms | 143.2556 ms |
| 3 | 131.2189 ms | 139.6611 ms |
| 4 | 124.7371 ms | 138.0941 ms |
| 5 | 123.2957 ms | 137.7342 ms |

Minimum/median/maximum capture duration was **123.0176 / 124.7371 / 286.6926 ms**. Each frame had 7,372,800 pixels, a 20,480-byte stride and a 29,491,200-byte buffer. Only one frame was retained at a time. Full-image analysis scans all pixels for brightness, variance, near-black count and an explicitly noncryptographic FNV-1a64 content fingerprint; its 137.73–143.26 ms cost is excluded from capture latency. It is evaluation overhead, not part of the application screenshot pipeline.

Every frame contained a full 0–255 luminance range and nonzero variance (**892.61–965.45**), with mean luminance 17.21–17.58. Approximately 84.90–85.12% of pixels had every RGB channel at most 20, consistent with the dark desktop content captured. These frames are not uniformly black. The full-image statistics are evidence of pixel content, not a detector for lock state, missing windows or capture correctness against an independent reference. A still screenshot is not required to change between runs.

No PNG or other screenshot pixels were written to disk; only bounded JSON metadata was persisted. The evaluator excluded the cursor and performed no window/title enumeration, clipboard action, network operation, microphone activation or synthetic OS input. It was explicitly run by the parent after availability preflight and central build, not automatically by tests or staging.

## Reproduction and verification boundary

The standalone example is `crates/rcr-platform/examples/eval-capture.rs`. Build centrally and run only with authorization to capture the actual desktop:

```powershell
cargo build -p rcr-platform --example eval-capture -j1
target/debug/examples/eval-capture.exe --output <fresh-existing-parent>/capture-report.json
```

Without `--output`, it prints JSON only. An existing report is refused, and final file creation also uses `create_new`; no report or user image is replaced. Failures exit nonzero rather than reporting fabricated timing success. The pure example pixel-statistics test passed, the platform suite passed 16 tests, and the parent's four-package all-target Clippy checkpoint passed. The later release review also passed and is recorded in the [hash-bound acceptance JSON](../.downloads/plan-continuation/release-acceptance.json); these screenshot timings remain scoped to this standalone measured capture example, not inferred release-UI latency.

The measurement excludes hotkey dispatch, configured delays, region selection, capture-overlay creation/rendering, editor preview, PNG encoding/publication, cursor compositing and clipboard transfer. It does not establish those end-to-end latencies, a cross-machine result, loaded-game behavior or release-binary acceptance. The five-run sample is too small to claim a broad percentile distribution. See [the screenshot reliability audit](SCREENSHOT_RELIABILITY_AUDIT_2026-11-05.md) and [the product plan](PRODUCT_IMPROVEMENT_PLAN_2026-11-05.md) for the wider correctness and release gates.
