# Actual hardware recording Stop evaluation — 2026-11-05

**Current packaged release: all four actual 6/60-second Desktop cases passed, with Stop→UI idle 93–252 ms and Library ready 125–313 ms.** An earlier development matrix also passed 12 worker cases and four Desktop cases (20 clips), with worker Stop 48–139 ms and Desktop UI idle 31–62 ms. The reported 30+ second delay was not reproduced in either scoped matrix. Loaded gameplay, microphones, hour-long sessions and clean-machine portability are not established by these results.

## Current packaged-release Desktop hardware results

The actual staged optimized executable is `dist/windows-reviewed-2026-11-05/rcr.exe`, SHA-256 `1EEA531A1D2AB8D8151BE1E84C8781A6A04F0E025D413450DC2B01A97698E63D`. Its independent aggregate `.downloads/plan-continuation/release-hardware/desktop-evaluation.json` reports `success=true`. The same RTX 5070 / driver 610.88, 2560×1440 60 FPS AV1 profile was exercised with microphones disabled, an owned nonactivating activity marker and inaudible loopback render. No uploads or synthetic OS input were used.

| Packaged release case | Stop→UI idle painted | Stop→file observed | Stop→Library ready | GPU preview draws | Encoded/fully decoded frames | Actual PCM packets |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 6 s, no audio | 93.6879 ms | 30.9144 ms | 125.1730 ms | 201 | 366 | 0 |
| 6 s, system loopback | 251.8607 ms | 220.7198 ms | 312.9326 ms | 206 | 364 | 604 |
| 60 s, no audio | 93.2552 ms | 32.5841 ms | 124.8396 ms | 1,942 | 3,606 | 0 |
| 60 s, system loopback | 93.2172 ms | 60.9981 ms | 156.1196 ms | 1,940 | 3,605 | 6,006 |

All four outputs passed full decoding, active/nonblack/changing source, timestamps/counts/metadata, real GPU preview, requested audio health, actual worker retirement and saved-Library gates. Production Stop dispatch took 0.0012–0.0021 ms and finishing-state paint about 1 ms. File observation, UI idle and Library observation are separate events measured by the proof's polling/render schedule; a file can be observed before the next idle paint. These are not physical mouse-dispatch or monitor-scanout timings.

Total worker finalization for the four rows was **14.1705 / 208.1103 / 12.5795 / 24.8232 ms**, respectively. The slower six-second system case spent **203.0252 ms** in durable writer finalization; the other rows spent 8.3396, 6.9775 and 19.4429 ms in that stage. This is observed finalization variability; its underlying cause was not established. It remains visible in the report rather than being excluded as an outlier or attributed to an unsupported cause. It is still far below the reported 30-second symptom.

This adds four validated release clips to the earlier 20 development clips; it does not replace their measurements or imply release replay-handoff coverage. Separate release clean-working-folder CLI/render/native checks, final six viewer runs/80 unchanged budgets and post-runtime package verification subsequently passed. The [hash-bound acceptance JSON](../.downloads/plan-continuation/release-acceptance.json) closes the local review wave; see [the packaging report](WINDOWS_PACKAGING_2026-11-05.md). No clean-machine portability or manual foreground fullscreen experience is claimed here.

## Unlocked resume, 18:18 local time

The user explicitly authorized continuing after unlocking the PC. Read-only preflight now reports `available`, input desktop `Default`, foreground `T3 Code (Nightly).exe` and an enabled NVIDIA display. No wake, unlock or synthesized input was used for the resumed tests. Fresh evidence is under `.downloads/hardware-resume-20261105-181802/`.

All six `worker-short/` and six `worker-long/` cases passed independent full decoding, source-content checks, timestamp/frame-count/metadata checks and the actual desktop-update gate:

| Case | No audio Stop→worker return | System loopback Stop→worker return |
| --- | ---: | ---: |
| 6 s continuous recording | 56.66 ms | 56.41 ms |
| 6 s replay-only, saved on Stop | 77.25 ms | 78.28 ms |
| 6 s replay requested near Stop | 62.24 ms | 48.29 ms |
| 60 s continuous recording | 61.07 ms | 72.19 ms |
| 60 s replay-only, saved on Stop | 119.28 ms | 139.18 ms |
| 60 s replay requested near Stop | 59.61 ms | 98.24 ms |

Short system cases contain 600–601 actual WASAPI PCM packets; long cases contain 6,000. Short worker cases observed 206–363 desktop updates and long cases 1,370–2,503. There are no reported audio errors or raw CPU capture readbacks. The two near-Stop scenarios preserve both the full recording and requested replay, giving eight clips in each worker matrix. Saves completed before background-job handoff was needed, so hardware coverage establishes accepted-request preservation, not long-running microphone-cleanup handoff.

The strict worker aggregates are `worker-short/hardware-evaluation.json` and `worker-long/hardware-evaluation.json`, each with `success=true`. Every output fully decodes with its exact expected frame count, matching embedded metadata, monotonic PTS and an initial keyframe. All sources are nonblack/changing: long clips have 119–121 distinct images across 120–121 samples. The microphone remained disabled.

### Production Desktop, GPU preview and Library

The first resumed `desktop-short/` run had `preview_draws=0`. It remains preserved as limited controller timing. A proof-only correction explicitly sizes the hidden host, counts preview submissions after rendering and retains the peak across retirement. The following fresh runs passed actual GPU preview submission, active-source and complete-file checks:

| Production Desktop case | Stop→UI idle | Stop→Library ready | Actual GPU preview draws | Captured desktop updates | Encoded/decoded video frames | Loopback PCM packets |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 6 s, no audio | 60.95 ms | 93.10 ms | 198 | 145 | 364 | 0 |
| 6 s, system audio | 61.97 ms | 93.31 ms | 198 | 103 | 365 | 605 |
| 60 s, no audio | 33.17 ms | 94.14 ms | 1,937 | 1,703 | 3,604 | 0 |
| 60 s, system audio | 31.26 ms | 94.14 ms | 1,947 | 3,303 | 3,604 | 6,005 |

There were no preview errors; the owned host viewport was 1440×900. Production Stop handler execution was 0.005–0.023 ms, finishing-state drawing 1.56–2.04 ms. The real worker had retired, the saved file existed and the refreshed Library contained it before success. Both 60-second files contain 121 distinct sampled images and decode all 3,604 frames. The few frames beyond a nominal 60 seconds reflect the UI timer/33 ms poll measurement schedule, not missing or fabricated source frames.

The strict Desktop aggregates are `desktop-preview-fixed/desktop-evaluation.json` and `desktop-long/desktop-evaluation.json`, both with `success=true`. Their tested executable is `target/debug/rcr-desktop.exe`, SHA-256 `E6755412BCCD14EF211B0AE4011F1D51C6255FA4E5D8BB384FEFD026C8C9AF86`. These measurements use the production Stop handler and GPU submission through a hidden window; they do not include physical mouse dispatch latency or prove monitor scanout.

## Earlier locked-session attempts

A short real desktop recording passed. Later recordings encoded a black/frozen source while Windows showed LockApp and the NVIDIA display was inactive. Those later timings establish limited GPU/audio pipeline shutdown behavior, not successful normal desktop capture or a fix for the reported 30+ second Stop delay. Device attempts stopped on identifying that condition and resumed only after the user's later explicit instruction and a successful availability preflight.

## Measured results and limits

Hardware: NVIDIA GeForce RTX 5070, driver 610.88. Native DXGI/D3D11 capture and NVENC AV1, requested 2560×1440 at 60 FPS, 25 Mbit/s. All files remain local. No microphone or uploads were enabled.

| Case | Stop measurements | Source/stream evidence | Acceptance |
| --- | --- | --- | --- |
| Actual desktop, 6 s, no audio | Capture loop stopped **16.58 ms** after Stop; file published **76.46 ms**; worker returned **77.08 ms**, joined **77.49 ms** | 362 encoded and independently decoded frames, 129 desktop updates, 6.033333 s, 5,551,895 encoded video bytes; zero CPU readbacks; monotonic timestamps and initial keyframe. All 13 sampled images differ; source is not black/frozen. Parent full-clip `blackdetect` found no black interval. | **Short worker-path pass only.** Does not establish long recording, replay, gameplay load, microphone, or normal Desktop UI behavior. |
| Desktop-loopback enabled, 6 s | Worker returned about **47.4 ms** after Stop | 361 frames decode; PCM track exists but contains **zero audio packets**; only one desktop update | **Limited pipeline timing.** Does not establish audio-content capture or active desktop capture. |
| Actual NVENC + WASAPI, 60 s, owned zero-PCM render | Capture loop stopped **0.33 ms** after Stop; file published **60.65 ms**; worker returned **63.53 ms**, joined **65.11 ms** | 3,601 encoded/decoded frames and **6,000 real loopback PCM packets**; no audio errors. Only one desktop update; all 121 sampled images are identical and entirely black. | **`blocked_external_state`.** Valid mux/decode and pipeline timing, but not a complete active-content capture pass. |
| Production Desktop Stop handler, 6 s, no audio | Handler **0.020 ms**; finishing draw **0.89 ms**; UI idle/worker retired **64.59 ms**; file observed **34.51 ms**; Library ready **127.82 ms** | File exists and is in Library, but source has one update; `active_source_updates_verified=false`, preview draw count zero; the proof reports `success=false` | **Partial controller/retirement/Library timing only.** Normal capture preview and active-content UI acceptance remain unverified. |

The source-content checks decode existing files in software; they do not take additional desktop screenshots. Black-pixel measurements use 160×90 grayscale samples including the original first frame and approximately two frames per second, with a near-black threshold of 20/255. An active-source test fails when all samples are ≥99.9% black or the expected animated source remains frozen. Successful decoding alone is insufficient.

## Preserved evidence

Paths are relative to the repository root. Original worker reports, clips, failed attempts and logs are preserved.

- Short nonblack desktop: `.downloads/hardware-eval-2026-11-05/live-noaudio-6s/worker-report.json` and `hardware-evaluation.json`.
- Silent desktop-loopback configuration: `.downloads/hardware-eval-2026-11-05/live-system-6s/worker-report.json`.
- Sixty-second black-source/WASAPI measurement: `.downloads/hardware-eval-2026-11-05/python-held-long-system/continuous-system-60s-run0/worker-report.json`; independently reclassified as `blocked_external_state` in the parent directory's `hardware-evaluation.json`.
- Partial Desktop proof: `.downloads/hardware-eval-2026-11-05/desktop-active-6s/desktop-6s/desktop-report.json`.
- Read-only blocked preflight: `.downloads/hardware-eval-2026-11-05/blocked-preflight/availability-preflight.json` and `hardware-evaluation.json`. It records an inactive display, foreground class `Windows.UI.Core.CoreWindow`, and a foreground process image unavailable inside the restricted tool context. The parent's separate foreground-process query identified **LockApp**; screen-saver-running was zero. Reading an input desktop named `Default` does not prove the session is unlocked.
- Initial failed starts remain under `live-system-replay-60s/`, `live-awake-system-replay-60s/`, `activity-audio-first/` and `active-display-matrix/`. Attempts with no initial frame did not reach recording or Stop and are not latency samples.

## Repeatable checks

The hardware harness has nonblocking timestamps for actual capture start, capture-loop exit, GPU release, recording publication, replay handoff and completion. Its Stop request is independent of the recording worker. Desktop measurements invoke the production Stop handler and retain the production 33 ms poll/worker-retirement path; their window is rendered privately, with a separate owned changing marker and no synthesized mouse/key input.

```powershell
# Read-only availability check: no capture, audio rendering, wake or power request.
python scripts/eval-hardware-recording-stop.py --preflight-only --output .downloads/hardware-readiness

# Recheck existing files only. This requires neither an unlocked session nor capture permissions.
python scripts/eval-hardware-recording-stop.py --validate-only --output .downloads/hardware-eval-2026-11-05/live-noaudio-6s
python scripts/eval-hardware-recording-stop.py --validate-only --render-silence --output .downloads/hardware-eval-2026-11-05/python-held-long-system
```

The second existing-file validation is expected to return nonzero with `blocked_external_state` for the black source while preserving its partial timing and stream-validation evidence.

For a future explicitly available, unlocked/active-display test session:

```powershell
cargo build --locked -p rcr-windows --example hardware-stop-eval -j1
python scripts/eval-hardware-recording-stop.py --output .downloads/hardware-stop-unlocked-new --seconds 6,60 --audio none,system --scenarios continuous,replay-on-stop,record-and-replay,replay-save-near-stop --render-silence

# Actual Desktop Stop/controller/preview/Library gate, with the same authorization.
python scripts/eval-desktop-hardware-stop.py --output .downloads/hardware-desktop-unlocked-new --exe dist/windows-dev/rcr-improved-2026-11-05.exe --seconds 6,60
```

The live runner checks availability before taking a parent-thread keep-awake request and before every child case. LockApp/LogonUI, an unavailable input desktop or an inactive display produces `blocked_external_state` before capture/audio starts. It never unlocks or forces monitor wake. The parent request spans child processes and restores `ES_CONTINUOUS` on the owner thread. Owned silent-render children are finite and reaped. Existing case directories are not overwritten. Active marker cases require more than ten observed desktop updates as well as nonblack, changing decoded content. Replay-near-stop is queued 5 ms before Stop to exercise accepted-request drain/handoff.

## Remaining scope limits

The short/60-second active-content worker and production Desktop/preview/Library checks above are complete. These tests use normal desktop content plus a small owned changing marker, not controlled CS/gameplay or sustained high-motion 25 Mbit/s pressure. System audio is a real WASAPI stream fed by an owned inaudible zero-PCM render, establishing packet capture/drain and file validity rather than audible-source fidelity. Microphone/suppression, hour-long recordings, device loss and an exact reproduction of the original 30+ second scenario remain outside the demonstrated hardware evidence. Synthetic tone/failure/lifecycle tests cover other behavior separately and are not substituted for hardware measurements.

The earlier staged executable `dist/windows-dev/rcr-improved-2026-11-05.exe` had SHA-256 `4D235E6E74916B916378A9BEF0FCE30851590A05E97749F77CDE70D35D35F87A`; it is not the hash of the corrected Desktop proof build identified above. Final packaging and broader software validation are tracked by [the product improvement plan](PRODUCT_IMPROVEMENT_PLAN_2026-11-05.md). Historical black-source measurements remain excluded from active-content acceptance even though the later unlocked tests now pass.
