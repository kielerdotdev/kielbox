# FFmpeg 7.0.2 bindings and proof provenance

`bindings.rs` is copied unchanged from the controlled binding-generation output
identified in `provenance.json`. Generated constant size/alignment/field-offset
assertions compiled successfully in that isolated preparation. The declarations
are included by `../decode.rs` for native decoding, including production playback
and explicit proof examples. The conservative native audio path shares these
declarations. Runtime validation still happens before structures are accessed;
unsupported formats, edits or runtimes use the supported external path.

The source is the Gyan FFmpeg 7.0.2 shared development archive, release source
commit `e3a61e9103`. Its component DLL entries matched the installed runtime
files byte-for-byte before generation. `provenance.json` records archive/header/
DLL hashes, the exact target and generation arguments, Clang/Rust versions and
SDK inputs. Absolute local paths in that record describe the generation machine,
not installation requirements or portable runtime discovery.

Reproduce with `scripts/prepare-ffmpeg7-bindings.ps1` and its sibling helper;
see `scripts/ffmpeg7-bindings/README.md`. No Clang, Python or C bridge is required
to compile normal application code or consume these checked-in declarations.
Decoder loading additionally requires the exact runtime versions 61.3.100 /
61.1.100 / 59.8.100 before any FFmpeg structure is accessed.

FFmpeg public-header copyright/license notices are preserved in
`HEADER-NOTICES.txt`. They identify LGPL-2.1-or-later and other notices as
applicable to the original headers. `PROVIDER-LICENSE` is the provider's GPLv3
license document for its full binary distribution; it is preserved separately
and is not substituted for the individual header notices. Runtime DLLs are not
copied into this repository. The original provider archive/header tree remains
identified by the source links and hashes in provenance.

## Controlled native proof (root runs serially)

```powershell
cargo run -p editor-core --example d3d11_decode_proof -j1 -- `
  --input .downloads/owned-av1-fixture.mkv --dll-dir C:/bin --frames 120
```

The proof accepts local drive files, whitelists file I/O and a small set of
container formats, bounds decoding to 120 frames total including the cancelled
prefix, and installs an AVIO interrupt callback with a cooperative deadline.
It never decodes video to CPU memory, starts capture/audio, writes a source,
uses preferences, or opens the application. Deadlines are checked by the host
and FFmpeg; they cannot forcibly interrupt a hung OS filesystem/driver call.

Default device creation follows GPUI's current default D3D11 hardware adapter
selection. Pass both `--adapter-low <u32> --adapter-high <i32>` to require an
observed GPUI adapter LUID instead. The report includes the actual adapter,
texture extent/array slice, visible crop, PTS/time base/rate and unmodified
color/SAR metadata. Hardware decoding never falls back to CPU. The first short
context cancels with a typed reason, is destroyed, and the source is reopened
with a fresh hardware context to exercise cleanup.

`--convert` additionally submits decoded NV12 frames to the isolated tracked
video processor. It requires explicit limited-range BT.709 SDR and SAR 1:1;
unspecified metadata is reported/rejected rather than guessed. The proof
retains each submitted AVFrame until its exact GPU completion ticket returns,
uses at most three outstanding tickets, and exercises shared-output key 1→0
consumption without reading pixels. Fatal shutdown closes the decoder before
releasing any incomplete frame references, preventing decoder-pool reuse.

This proves neither source RGB accuracy nor GPUI playback cadence. No decoded
pixel readback is performed here; the separate synthetic converter color proof
and any later source-to-output color proof have distinct validation scope.

The JSON report aggregates host wall timings without per-frame logging:
device creation, prefix open/decode/close, reopen, reopened loop and cleanup form
exclusive phases with a sum and unattributed remainder. Converter initialization
and cleanup substeps are explicitly nested. Both opens separately report DLL
loading, local open, stream-info probing, hardware-context initialization and
codec opening. Bounded frame samples report count/sum/median/nearest-rank p95/max
for decoding, publication calls, observed input-retirement latency and output
acquisition. Query-poll call time, retry counts and actual sleep time are also
reported. Retirement includes intervening host work; output acquisition includes
its polling/sleep. These overlapping measurements are not GPU timestamps and
must not be added together as total duration. Compare separate cold/warm runs
with/without `--convert` before inferring playback suitability.

## Query-submission diagnostics

Normal completion polls allow submission on the first GetData for each EVENT,
then use DONOTFLUSH on every subsequent poll. They request the actual BOOL and
retain the AVFrame until S_OK with a true result. This is one nonblocking call per
poll, never a CPU wait loop. The proof defaults to the same policy (kick count 1,
BOOL data); diagnostic overrides can reproduce the former status-only/no-kick
baseline with `--query-kick-after-polls 0 --query-status-only`.

Change one option per comparison before combining them:

| Option | Controlled difference |
| --- | --- |
| `--producer-flush-after-release` | Additional producer Flush after ReleaseSync, preserving End→Flush before release. |
| `--consumer-flush-after-release` | Consumer Flush after releasing key 0. |
| `--query-kick-after-polls 100` | At most one flags=0 GetData per submitted EVENT, at its hundredth oldest-query poll; all other polls retain DONOTFLUSH. Zero disables; maximum 1000. |
| `--query-bool-data` | Retrieve the EVENT BOOL instead of valid NULL/zero status-only GetData. |
| `--query-status-only` | Explicit diagnostic override using NULL/zero status-only GetData. |
| `--consumer none` | Single-producer control releasing back to key 0; no consumer device work. |
| `--consumer same` | Default: producer-device key 1→0 round trip. |
| `--consumer separate` | Create a second device on the same observed adapter LUID and open the shared output once. |
| `--consumer-copy` | Copy the shared output into one private consumer texture and Flush before releasing key 0; no CPU pixel readback. |

These flags require `--convert`. The no-consumer control rejects consumer-only
options. JSON identifies every selected option, actual query-kick/BOOL-read counts,
producer post-release Flush count, and shared-resource open count. Consumer device
creation/import has a separate nested initialization timing. Queues remain bounded
to three reads; diagnostics neither wait in GetData nor release an AVFrame early.
The explicit query kick may submit work, and is not a GPU completion wait.

Microsoft documents that [DONOTFLUSH may leave commands unsubmitted](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_async_getdata_flag),
while [NULL/zero GetData is valid for status-only checks](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-getdata).
[Flush submits asynchronously and unnecessary calls are costly](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-flush).
The [ANGLE D3D11 renderer](https://chromium.googlesource.com/angle/angle/+/refs/heads/main/src/libANGLE/renderer/d3d/d3d11/Renderer11.cpp)
also periodically permits GetData submission rather than using DONOTFLUSH forever;
its interval is not copied as a universal tuning value. The selected first-poll
policy was compared with the explicit no-kick baseline using the isolated
same-device and separate-device/private-copy proof. These results do not establish
a driver-internal cause or a general playback performance guarantee. Separate-device copying more closely
models GPU presentation but does not prove GPUI draw/display cadence or RGB accuracy.

`SharedOutput.published_frame` is a shared atomic frame identity, initially zero.
Tracked publication assigns a nonzero ID under producer key 0 before releasing
key 1; `publish_tracked_frame` accepts a caller's explicit ID. Failed conversion
does not publish an ID or key 1. A GUI consumer reads the ID while holding key 1
and retains that exact ID with its private texture, including busy redraws.

## Opt-in transport preparation

`PlaybackSession::new` retains the existing CPU video path. The Windows-only
`new_with_gpu(..., GpuPlaybackConfig)` constructor requests an experimental
single-clip path with an explicit presentation adapter LUID and absolute DLL
directory. It preserves the existing audio device/clock and falls back to CPU
video for unsupported composition, timing, format, device or rendering cases.
Source identity failure stops playback instead of opening replacement bytes.
`gpu_output`, `gpu_status`, `gpu_stats` and `disable_gpu` support an eventual
desktop integration; the application does not select this path automatically.

The transport reserves six extra hardware frames before opening the codec,
requires an observed 18..32-surface pool and bounds caller retention to three
future frames plus three GPU read tickets. The current source bound is 3840×2160,
8-bit NV12, explicit limited BT.709, square pixels and an exact project frame
rate. It seeks a keyframe and performs GPU preroll for a nonzero source-in or
playhead. Future frames stay in the decoder pool until clock-due; only the newest
due frame is published. Pause retains resources. A process-wide admission guard
allows one GPU video worker through actual cleanup; another request uses CPU
video while that worker retires. UI Drop requests cancellation without joining;
explicit `shutdown` joins workers. Cooperative deadlines cannot interrupt a
hung synchronous driver or filesystem call.

The owned-file lifecycle proof is separate from decoder/color/display proofs:

```powershell
cargo run -p editor-core --example gpu_transport_proof -j1 -- `
  --input .downloads/owned-60fps-av1-with-quiet-audio.mkv --dll-dir C:/bin `
  --adapter-low <observed-u32> --adapter-high <observed-i32>
```

It uses the existing silent audio clock by default (`--wasapi` opts into the
actual audio device). Every audio track is muted in memory in both modes while
PCM consumption and device timing continue. It uses a separate same-adapter GPU consumer/private copy and
checks publication IDs against transport position, bounded retention, pause/
resume, a cloned project's 500ms source-in plus frame-6 seek, explicit video
fallback without clock reset, and shutdown. It writes no source, sidecar or
preferences and performs no GPU video readback. A fixture needs at least 90
project frames and an audio stream. Source implementation alone is not a native
validation result or a playback performance guarantee.
