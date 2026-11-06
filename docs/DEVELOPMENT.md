# Development notes

A combined screenshot, annotation, sharing, capture, replay and disk analysis toolkit in Rust with a native GPUI desktop, image editor and quick video editor. The first hardware target is **Windows, NVIDIA NVENC AV1, 2560×1440 at 60 FPS, SDR**.

This is a working development prototype. Windows capture, replay saving, separate WASAPI audio tracks, and edited AV1 exports have run on an RTX 5070. Linux capture and AMD/Intel encoders are not implemented yet. Paused editor seeking has a native NVDEC path and reusable seek previews. An opt-in AV1 GPU playback path has passed native GPUI pause/resume and CPU-fallback checks; default playback and composed projects retain the CPU path. Playback and export use the original media. See [validation scope](VALIDATION.md).

## Build and run

Install the MSVC Rust toolchain, Visual Studio C++ build tools, and a current NVIDIA driver on a Windows desktop with an AV1-capable NVIDIA GPU. The repository pins Rust 1.99.1. An NVIDIA SDK or CUDA toolkit installation is not needed to run the recorder.

```powershell
cargo build -j1
cargo run -p rcr-desktop -j1
```

Builds default to one worker with incremental caches disabled to limit memory/disk pressure.

Stage a release application with `powershell -File scripts/package-windows.ps1`. The output is `dist/windows/rcr.exe`; `-SkipBuild` uses an existing release. For a debug build, use `-DevelopmentExecutable <exe>` to stage it in `dist/windows-dev/`. Cargo build artifacts are not runtime files.

Release shaders, fonts and icons are embedded. Debug shaders require this checkout, and video-editor features still require external FFmpeg tools/libraries. Settings, projects and default recordings remain relative to the working directory; packaging does not migrate them. Keep the same working directory to retain existing data locations.

For editor import, waveform generation, playback, and export, put `ffprobe` and `ffmpeg` on `PATH`. FFmpeg must include `av1_nvenc`, AAC, drawtext, and the standard composition/audio filters. Executable paths can be overridden with `RCR_FFPROBE` and `RCR_FFMPEG`. Windows editor monitoring uses native WASAPI; FFplay is needed only by the older standalone monitor example and the optional generated-tone audio proof. `RCR_FFPLAY` overrides the older monitor's path; the proof script accepts `-FFplay`. Recording and replay muxing are native Rust and do not launch FFmpeg.

Run from the repository root. Recordings go into `recordings/`; edits autosave separately for each recording in `projects/clips/` and restore when it is reopened. Exports go into `recordings/exports/`. Closing the window keeps RCR running in the tray by default (toggle this in Settings). **Quit rcr** from the tray stops capture, finalizes its output, stops editor playback, and requests export cancellation. Each render uses a unique `.rcr-export-*` staging directory beside its destination. After FFmpeg starts, failed or cancelled renders retain any `.partial` file there; the export notice's **Show in folder** locates it for inspection or cleanup. **Show details** exposes the bounded actual error text with **Copy details**, separately from autosave failures. Empty staging directories are removed, and successful renders publish without replacing an existing destination. Cancellation during audio preparation publishes no output. A failed recording can leave either a finalized playable prefix or a `.partial` file; its statistics report the error. Existing recording/export destinations are not overwritten.

The toolbox also includes **Tools → Disk usage**: a cancellable folder scan with a size-sorted tree, largest files, file-type filters and a treemap. See [local tools](UTILITIES.md#disk-usage) for controls and size-accounting limits.

## Capture and instant replay

In Capture or Settings, the audio-source dropdown chooses **Desktop audio** or a running application; the microphone remains a separate optional source and is off by default. Application audio includes the selected process's children. Select the running game/app rather than its launcher, and reselect after it relaunches. The selection is locked while capture is active. Application PIDs are not persisted: restarting RCR returns the source choice to Desktop audio. **Start recording** writes continuously and also maintains replay history. **Enable instant replay** captures into memory without writing a continuous recording. **Save replay** snapshots the retained history into an independently playable MKV without stopping capture. Its completion notice offers **Edit** and **Show in folder** for that saved replay. Starting, ready, recording and finishing states show which actions are available.

The **input device** and **desktop audio device** controls choose a microphone or an output endpoint to record. Windows default remains the initial choice. Explicit choices persist across restarts; a disconnected saved device stays visible for reconnection or replacement and is never silently replaced with another device. Selecting an output chooses which sound RCR captures; it does not reroute applications or change Windows playback settings. Hardware choices are locked during capture. Application capture uses its selected process independently of the saved desktop device.

Application selection retains a process creation-time fingerprint, so a stale selection cannot silently attach to a reused PID. Startup/activation failures are reported without substituting desktop audio. If the selected process exits, its audio source stops with an explicit notice; video and an enabled microphone can continue. Application tracks retain their `Application: <executable>` name in the recording and quick editor.

Application capture uses Microsoft's process-loopback API and checks support at runtime. The [activation-parameter documentation](https://learn.microsoft.com/en-us/windows/win32/api/audioclientactivationparams/ns-audioclientactivationparams-audioclient_activation_params) specifies build 20348, while the [activation-function page](https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/nf-mmdeviceapi-activateaudiointerfaceasync) currently says 20438. This is not a general Windows 10 compatibility claim. Controlled tone tests on Windows build 26200.9711 verified selected-process isolation, child-tree inclusion, stale-fingerprint rejection and explicit target-exit reporting. See [validation scope](VALIDATION.md); these short proofs do not establish behavior for every game or audio driver.

Default global Windows shortcuts while RCR is running; change or disable each binding in **Settings → Global shortcuts**:

| Shortcut | Action |
| --- | --- |
| Ctrl+Shift+F9 | Enable / disable instant replay; stop a full recording first |
| Ctrl+Shift+F10 | Save retained replay history |
| Ctrl+Shift+F11 | Start / stop continuous recording |
| Ctrl+PrintScreen | Capture a screenshot region |
| Alt+PrintScreen | Capture the active window |
| Shift+PrintScreen | Capture the full desktop |

Shortcut registration conflicts are reported in Settings; capture buttons remain available. Quitting RCR unregisters its shortcuts. The Windows tray shows capture state through its icon, tooltip and status row, with separate replay toggle/save actions, Start/Stop recording, screenshots, Tools, Library and Settings. Labels reflect configured shortcuts, and actions that cannot run while starting or finishing are disabled.

The default history is 120 seconds. Its bounded packet budget now sizes itself from replay duration, video bitrate, a preceding GOP, enabled PCM sources, packet overhead and 25% video bitrate headroom, with a 512 MiB minimum and 4 GiB maximum. Settings shows the estimated memory budget. The ring retains the keyframe preceding the requested time window, rather than dropping it as soon as the target duration is exceeded. An explicit smaller byte ceiling or bitrate bursts beyond the estimate can still shorten available history; actual retained duration remains visible. Each capture session allows one replay save at a time. Desktop retains up to two unfinished saves across stopped sessions; each can pin an additional snapshot of up to that session's buffer payload. Files contain AV1 video and independently named 48 kHz stereo float PCM audio tracks. PCM avoids an additional encoder in this first implementation, at about 384 kB/s per audio source. No source mixing is performed by the recorder. The quick editor exports one audio mix with only the sounds you enabled; the original recording retains separate tracks.

Short headless proofs:

```powershell
# Four seconds, video only.
cargo run -p rcr-windows --bin rcr-capture -j1 -- --seconds 4 --output recordings/proof.mkv

# Independent system and microphone tracks.
cargo run -p rcr-windows --bin rcr-capture -j1 -- --seconds 4 --audio system,mic --output recordings/audio-proof.mkv

# Replay-only operation: save the retained history on stop.
cargo run -p rcr-windows --bin rcr-capture -j1 -- --seconds 4 --audio system,mic --replay-only --save-on-stop --output recordings/replay.mkv

# Save while the continuous recording is still running.
cargo run -p rcr-windows --bin rcr-capture -j1 -- --seconds 6 --audio system,mic --output recordings/session.mkv --save-at 3 --save-replay recordings/moment.mkv
```

CLI options include `--monitor`, `--width`, `--height`, `--fps`, `--bitrate` and `--replay-seconds`. Use unique output names.

New MKV recordings and replay clips embed configuration, audio-track descriptions and clip counts in `_RCR_METADATA`; full recordings also include capture-stream-drain statistics. Inspect it with:

```powershell
ffprobe -v error -show_entries format_tags=_RCR_METADATA -of json clip.mkv
```

Recording publication precedes offline replay processing, so final UI statistics may include later results absent from the tag. MKV no longer needs a statistics sidecar. Video-only IVF proofs retain `.stats.json`; editor autosaves remain separate.

For application audio, `--audio-process <PID>` replaces system audio and preserves a requested microphone, independently of argument order:

```powershell
cargo run -p rcr-windows --bin rcr-capture -j1 -- --seconds 4 --audio mic --audio-process 1234 --output recordings/app-proof.mkv
```

Replace `1234` with the current application PID. This CLI option binds the running instance at startup; it does not remember or follow an executable across relaunches. To check isolation without recording desktop video, microphone audio or PCM files, build the audio-only example and run the optional controlled proof:

```powershell
cargo build -p rcr-windows --example process-audio-proof -j1
./scripts/verify-process-audio.ps1
./scripts/verify-process-audio.ps1 -IncludeChildren
./scripts/verify-process-audio.ps1 -ExpectExit
```

The script requires `ffplay` on PATH (or `-FFplay <path>`). It starts two quiet audible generated-tone players, checks inclusion/exclusion or target exit, and cleans up its own players. It saves JSON/text evidence under `.downloads`; it creates no recording or captured-audio files. Child-tree and exit checks can also be combined.

## Optional microphone noise suppression

Enable **Clean up replay microphone** in **Settings → Audio** to suppress the microphone in future replay saves. It is off by default and can be changed while capture is running; each save uses the setting at the time of the request. Microphone capture, the replay ring and the continuous recording stay raw. Video and system audio remain unchanged. Processing runs on background replay-save workers at reduced Windows thread priority. Stopping capture transfers unfinished saves to Desktop ownership after GPU drain and recording publication, so capture can stop while replay cleanup continues. Completion and errors still appear in Library notices. Closing the app cancels pending suppression and joins owned jobs; a disk mux already underway must finish. CLI save-on-stop remains synchronous.

For editor export, open the dropdown beside **Export** and enable **Clean microphone when exporting**. The setting belongs to the microphone track, autosaves with the recording's edits and supports Undo. It applies before speed, volume and mixing. Only source intervals used by the export are processed. Preview monitoring stays raw; original media is never modified.

The implementation uses **DPDFNet-2 48 kHz HR**, with pure Rust FP32 neural kernels, RustFFT and bundled, hash-checked pretrained weights. It has no ONNX Runtime, C inference engine, Python runtime or model-download requirement. The speech model downmixes the selected source to mono; saved PCM tracks duplicate that result to stereo. Its 50 ms algorithmic delay is removed, its tail is flushed, and the saved sample count/timestamps are preserved. Independent segments reset model state at discontinuities.

Suppression adds CPU work and clip-save/export delay. A one-second synthetic development fixture took about 0.23 seconds of processing in one run; longer clips can take several seconds. This is a local measurement, not a general hardware or quality benchmark. Replay-save memory can additionally include processed microphone payload and the model's scratch/weights, beyond the pinned raw snapshot.

```powershell
cargo run -p rcr-windows --bin rcr-capture -j1 -- --seconds 6 --audio system,mic --suppress-mic --output recordings/raw-session.mkv --save-at 3 --save-replay recordings/clean-moment.mkv
```

Here `raw-session.mkv` contains the original microphone, and `clean-moment.mkv` contains a separately named DPDFNet-processed microphone track. Recording/replay suppression is native Rust; editor source decoding still uses the editor's existing FFmpeg dependency. See [model provenance](../crates/dpdfnet/NOTICE.md).

## Quick clip editor

Choose **Watch clips** in Library, a video card, or **Clip show** (Ctrl+Enter) in the editor to watch videos with a larger preview, a lightweight scrubber and Previous/Next controls. Space pauses, Left/Right changes clips, M mutes local playback and Escape opens editing. Playback volume is separate from saved track gains and export audio. Watching skips editor timeline, waveform and thumbnail work. It uses existing saved edits, stops at the queue boundary or a playback error, and supports sharing the window in a voice/chat app. The queue follows the Library search, filters, favorites and sort order across pages; the editor dropdown still renders at most 64 choices. Synchronized remote watch parties are not implemented.

Open **Library** and choose **Edit** on a video, or **Open in Editor** while watching. The recording opens directly, or restores its previous edits. There are no projects, media bins or inspectors in the interface.

- Drag the visible grips at either edge of a piece to trim it, including restoring trimmed material. The time scale stays fixed so the grabbed edge follows the pointer and stays where you release it. Other pieces keep their timeline positions. Use **Fit** to fit the current edit to the timeline. Click a piece to select it and seek; drag its body to place the selected piece and its linked audio/video group at another timeline position.
- **Split** divides a piece; **Delete** removes the selected piece and leaves its timeline gap. Trimming and deleting do not automatically close gaps or reorder neighboring pieces. Move a piece's body manually to close a gap.
- **+ Add clip** offers up to 64 recent Library videos with their display names, plus **Browse videos…**, and joins the chosen video at the end. The picker follows completed Library scans; accepting a choice or closing the popup returns focus to the editor.
- Speed presets apply immediately to the selected piece; its video and every audio source stay together. Its start and other pieces' positions stay fixed while its duration changes.
- Each audio source has a waveform, a **mute** icon and a volume slider. The settings apply to every piece in the export, and update monitoring while playing. Named recorder sources appear as **Game** (system/desktop loopback), **Application: <executable>**, and **Microphone**. Explicit application labels survive names containing words such as mic, game or system; other named sources retain useful names. Unlabelled sources appear as **Recorded sound**, without inventing a microphone or game source.
- **Export** saves an MP4 in the selected save folder’s `exports/` directory with original resolution and sensible defaults. Its dropdown offers **Original**, **1080p**, and **Small file**, with size estimates. Completion shows **Saved**, **Show in folder**, and **Copy**; on Windows, Copy puts the actual file on the clipboard.

Edits autosave against the opened recording. The header shows **Saving…** until the latest revision is acknowledged, then **Saved**. A failure names the affected capture and offers **Retry save** while retaining the unsaved project. Reopening another recording replaces the strip rather than appending it. Add clip is the explicit joining operation. Old video-only `.ivf` proofs contain no audio; the editor says so rather than inventing lanes. New `.mkv` recordings include each enabled audio source.

Undo/Redo and linked-edit mode survive switching between clips during the same app session, with up to eight inactive editor sessions retained by recent use. Disk autosaves restore the project after restarting the app; undo history remains in memory. **Reset edits** restores the opened recording as one complete piece with its original tracks and supports a single Undo. If saved edits are damaged, **Open original** first keeps a recovery copy of those edits instead of silently replacing them.

Reopening also checks source files on the media worker before displaying saved edits or restoring cached history. New imports store canonical path, file size and modification time; legacy projects are checked against freshly probed duration, dimensions and stream indices/kinds before receiving a baseline. A changed or unavailable source leaves the old edits intact and shows a recovery message. **Open original** backs up existing disk edits and any latest unsaved project before opening the current file at the original path with fresh metadata. It does not recover previous source bytes. Offline Windows paths retain access to existing sidecar names; reconnecting a source allows its edits to be checked again. Export rejects detected source changes before creating output. These metadata checks are conservative, not a content hash or a guarantee against changes during use.

| Editor shortcut | Action |
| --- | --- |
| Space | Play/pause |
| Left / Right | Step one frame |
| Shift+Left / Shift+Right | Step one second |
| S | Split at the playhead |
| Delete | Remove the selected piece, leaving its gap |
| I / O | Trim the selected piece to the playhead |
| Ctrl+Z / Ctrl+Shift+Z | Undo / redo |
| Ctrl+E | Export |
| + / − | Zoom timeline in / out |
| Ctrl+0 | Fit the timeline |

Playback renders at the displayed physical pixel size, preserving aspect ratio and capped at source resolution, at up to 60 FPS. A persistent session warms bounded video/PCM queues while paused; play/pause retains the decoders and audio device. Quick edits use a direct sequence decode path with a prefetched next batch, instead of the layered export compositor. Windows WASAPI supplies the audio clock, and Rust mixes separate sources with live mute/volume settings. Late video frames are discarded rather than accumulating playback delay. Video frames have a 64 MiB queue budget, and each source buffers at most one second of PCM. Cold opening needs decoder initialization; warm resume retains the existing session. Paused seeking uses the separate native decoder/cache path described below. Export composes on the CPU and encodes with NVENC; end-to-end GPU composition and a shared recorder/editor texture path remain future work. Hidden native layout/input fixtures cover specific interactions; manual visual operation and clipboard pasting have not been verified with Computer Use.

Experimental Windows GPU playback is available under **Settings → Editor playback → Use GPU video preview**, off by default and applied to the next playback session. It searches for compatible FFmpeg 7 shared libraries beside an absolute `RCR_FFMPEG` executable and in local absolute `PATH` entries; `RCR_FFMPEG_LIB_DIR` can supply a preferred absolute directory. GPU playback requires the supported FFmpeg 7.0.2 ABI. A loaded library family is retained until app exit, so switching an incompatible runtime requires restarting RCR. No libraries are downloaded and system search paths are not modified. The process override `RCR_GPU_PLAYBACK=1` remains available for diagnostics.

The path uses the actual GPUI adapter, D3D11VA AV1 decoding, GPU color conversion and a private GPUI GPU copy, without downloading video pixels to the CPU. The first eligible case is one continuous, untransformed source-rate clip with explicit limited BT.709, square pixels and 8-bit NV12 output. Other compositions, unavailable libraries and GPU presentation failures use the CPU path while retaining the audio clock. This opt-in does not change export or paused scrubbing. Its hardware-specific evidence and limits are in [VALIDATION.md](VALIDATION.md).

## Design implementation

The supplied **rcr Quick Editor.html** is implemented as native GPUI screens: Capture, Library, Editor and Settings. The shared charcoal surfaces, green accent, bundled Geist/Geist Mono fonts, thin SVG icons, compact navigation and rounded cards match the reference's visual language. The editor uses a full-width toolbar, large preview, a time ruler, 64px thumbnail pieces, aligned 28px audio lanes and a continuous playhead. Export presets and Add clip open floating panels.

- **Capture** shows the real GPU preview, replay duration/memory, recording elapsed time/size, separate audio sources and recent saved captures. The configured recording shortcut starts/stops recording (**Ctrl+Shift+F11** by default). Switching an armed replay session to recording restarts the capture session and its buffer; replay stays enabled during recording.
- **Library** has real 320×180 source thumbnails, duration badges, search, type filters, favorites, newest/oldest/name/size sorting, grid/list views and selection. Thumbnail decoding runs serially at reduced priority; the resident cache holds 64 images, the disposable disk thumbnail cache is bounded to 32 MiB, and pages show 24 clips. Arrow/Home/End navigation, Space selection, page Ctrl+A and F2 naming work without stealing text-input keys. Display names persist in preferences without renaming source media or project sidecars.
- **Import and discovery** accept dropped files or the Library's **Import** picker. Videos, images and `.ssxe` projects remain at their original paths; Library stores references, with a 64-file batch limit and 512 imported-reference limit. Discovery, metadata checks and newest-first ordering run on a worker with bounded latest-request/result slots; stale results are discarded and the previous catalog remains visible until replacement. Offline imported references remain visible as **File unavailable**. Removal moves managed local captures into `.trash`; external or offline imported references are unlinked from Library without moving or deleting their originals. **Undo removal** is session-only, available until RCR closes, and preserves names, favorites and edit sidecars; partial failures retain the remaining Undo work.
- **Trash recovery** opens a background-scanned browser for the current capture folder. Managed removals write a recovery journal before moving files, allowing **Restore** after restarting RCR without overwriting an existing original. Journals remain after restoration. Older files without journals stay accessible through **Open folder** for manual recovery; reference-only removals are not durable Trash entries. Scans inspect up to 256 journals and list 2,048 files, prioritizing newest journals among at most 4,096 discovered directory items. Additional byte/move limits and corrupt or unsafe entries produce warnings; no automatic deletion occurs.
- **Settings** persists resolution, frame rate, AV1 bitrate, replay length, audio defaults, optional microphone cleaning, save folder, favorites, view mode, capture display names, imported references, tray behavior and six editable global shortcuts in `projects/preferences.json`. Recording changes apply next session; stop capture before changing audio sources or the save folder. Defaults remain 1440p60, 25 Mbps AV1, 120 seconds and no microphone suppression.
- **Format → 9:16** shows a draggable crop region over the wide preview. Framing autosaves and supports undo/redo. Export crops the real source while retaining the original recording and separate edit tracks. **16:9** restores the wide output. The 1080p and Small presets adapt their bounds to portrait output.

Game detection/folders, automatic recording/replay cleanup and a user-configured capture storage cap are not implemented. Disposable thumbnail and seek caches have their own bounds; those are separate from retention of original captures. Desktop is the actual capture source. Visual layout and mouse interactions still need user verification; Computer Use was not used. See [UX research and implementation](UX.md) for official ShareX, Flameshot, NVIDIA, Medal and SteelSeries references and screenshot links.

## Capture architecture

### Independent quick screencasts

**Tools → Screenshots → New capture**, the tray's screenshot command and
**Ctrl+PrintScreen** open one desktop picker; **Record video** and
**Ctrl+Shift+PrintScreen** open the same picker in video mode. RCR minimizes before
preparing the frozen desktop. Selection handles and screen/window hit testing live
on that desktop, with a compact bottom toolbar for Selection/Screen/Window,
Screenshot/Video, a round shutter and the pointer option. Sound and microphone
controls appear for video; More holds 30/60 FPS and an optional three-second
countdown. Escape cancels the picker or countdown. The main Tools page is a launcher
and recording status surface, without a capture-settings card.

A drag release only finishes adjusting the selection. The shutter or Enter confirms
it. Video areas must fit within one display; window/screen recording passes the
selected native source ID to the independent WGC worker. Area and screen screenshots crop the
chosen frozen image and pass its pixels and physical origin to the screenshot
workflow. Window screenshots pass the native window ID to the existing capture
backend, so overlapping desktop windows are not baked into the result. The workflow
opens the existing selection directly instead of asking for a second selection. Pointer-on/off snapshots are frozen
before showing the overlay. RCR's own windows are excluded from window hit testing.

Pause/resume, Stop and Discard stay in the main app's recording status row; the
shortcut and tray stop a screencast. Normal shutdown finalizes the clip. The tray
indicator turns red independently of game replay. The picker supports S/C/W, V, P,
Enter/Space and arrow keys; Shift+arrows resizes an area. A screenshot can be taken
while a screencast runs, with a second video session blocked. Open image, Share files
and More tools remain on the Tools page; annotation/upload options and QR creation
are under More tools. Saved screencasts provide Copy file and Show file actions.

The quick recorder owns a separate Windows Graphics Capture source and recording-tools
session, with no game recorder commands or replay-buffer access. It uses CPU pixel
readback and external FFmpeg `libx264` with two encoding threads, followed by AAC muxing
when audio is enabled. FFmpeg is an executable dependency (`RCR_FFMPEG` overrides PATH),
not a linked development-library dependency. Temporary output stays in an owned hidden
directory until publication without overwrite. Output fits within 1920×1080, and recorded
duration is capped at ten minutes. Audio uses Windows default devices and reports source
failures. The OS capture border stays enabled; the controller requests capture exclusion.

Repeat the hidden menu proof with `rcr-desktop --smoke-capture-menu --proof-output <absolute-folder>`.
It uses an isolated profile, generated video/audio and internal GPUI input events to
exercise the actual Studio window and desktop picker at three viewport sizes, frozen
pixel/metadata routing, resizing, countdown cancellation, simultaneous still capture,
pause/resume, save, discard and Escape. It performs no Computer Use,
desktop capture, physical input or audio-device access. The separate opt-in
`--smoke-quick-recorder` WGC proof captures a generated, owned native window and requires
an ordinary Windows desktop process; the restricted sandbox reports WGC unavailable.

```text
DXGI Desktop Duplication (NVIDIA-attached monitor)
  -> owned BGRA D3D11 texture (GPU copy; release desktop frame)
  -> D3D11 video processor (BT.709 RGB -> NV12; optional scale)
  -> persistent registered NVENC input pool
  -> asynchronous NVENC AV1 bitstream
  -> Arc packet history / bounded writer queue -> native Matroska

Owned BGRA -> shared keyed-mutex BGRA -> GPUI D3D11 SRV -> GPU preview
WASAPI system + microphone -> shared QPC epoch -> separate PCM packet queues
```

**The capture path has no CPU raw-video readback.** It still performs GPU copies/conversion, copies compressed NVENC output to owned packet memory, and copies WASAPI PCM before releasing its buffer. Calling the entire application literally zero-copy would be inaccurate. Replay snapshots share packet allocations through `Arc`.

Capture/encode objects remain on one worker. The UI never waits for encoder completion; live preview drops publication when its shared texture is busy. Disk and replay writers use separate workers and bounded queues. An output queue overflow stops recording with an error instead of silently corrupting its timeline. Audio startup failures/discontinuities are visible in statistics. Stop and terminal errors attempt both video and audio drains and join the disk writer, preserving finalization failures rather than silently skipping the other stream.

Desktop Duplication access loss starts a visible reconnect attempt bounded to five seconds on the same GPU/output with unchanged SDR dimensions and rotation. Successful recovery requests a fresh keyframe. Device removal/reset, changed geometry, HDR or an expired recovery window stop capture with an error; recovery does not migrate to another display or GPU. This is an implemented recovery path, not proof of real display-mode/session transitions; those transitions have not yet been exercised.

`rcr-media-core` defines `VideoCapture`, `VideoEncoder` (associated surface), and `AudioCapture` traits. The concrete Windows implementation owns D3D11 resources and NVENC sessions. Future AMF, oneVPL, and PipeWire/DMA-BUF implementations must preserve native surface ownership rather than add CPU frame buffers to the core interface.

## Validation and limits

```powershell
cargo test --workspace -j1
cargo clippy --workspace --all-targets -j1 -- -D warnings
./scripts/verify-windows.ps1             # short hardware proof, no microphone
./scripts/verify-windows.ps1 -Audio      # include system + microphone capture
```

See [docs/VALIDATION.md](VALIDATION.md) for measurements and reproduction details. Short desktop proofs do not establish gameplay overhead or long-session A/V drift. The `raw_cpu_readbacks` statistic describes the implemented path; it is not a driver trace.

Known missing pieces: Linux/PipeWire capture, AMD/Intel encoder implementations, a Windows Graphics Capture recording backend, recorder cursor composition/HDR, rotated/hybrid display handling, audio-device selection/recovery, end-to-end GPU editor composition and a self-contained signed distribution. Continuous recording and replay currently support desktop duplication on an NVIDIA-attached SDR output; the screenshot toolkit has its own capture backend and HDR-to-SDR mapping.

## Attribution

The lightweight replay workflow takes inspiration from [ClipFlow](https://github.com/jaimitus/ClipFlow). This implementation uses GPUI and native Rust packet/muxing code; no ClipFlow source was copied. See [THIRD_PARTY.md](../THIRD_PARTY.md) for the vendored GPUI patch, NVENC binding provenance, and licenses.


## Screenshot and sharing toolkit

The **Tools** tab embeds SSX's Rust capture, annotation, workflow, uploader, clipboard and SQLite history services into RCR. The original SSX egui applications and recording backend are not launched. Recording and instant replay remain on RCR's NVENC path.

- Capture a region, full desktop, monitor, active window, or the last selected region. Screenshot capture and annotation share a borderless, topmost frozen-desktop overlay with a floating toolbar. The region tool highlights detectable native windows/panes with a black-and-white outline, coordinates and a pointer magnifier. Click a highlighted target to select it or drag freely to draw a rectangle. Selection stays open on release; drag inside it to move it, or drag its handles to resize, then explicitly copy/save. Detection uses a bounded background snapshot of native window/control rectangles; web/custom-drawn subregions may require manual selection. There is no transition to the main Tools editor. The capture service includes HDR-to-SDR mapping.
- The overlay provides arrows, shapes, pen, text, numbered steps, highlighting, blur and pixelation, with more tools in its popover. Properties stay visible for the selected annotation or active drawing tool, including color, thickness, text size and effect strength where applicable. Live effect regions use a contrasting UI-only outline. `F6` switches focus between the canvas and toolbar. On the canvas, `Tab` selects the region tool, `Space` selects the full screen, arrows move the region, Shift+arrows resize it, and `Delete` clears it. In the toolbar/properties, `Tab`/`Shift+Tab` traverse controls and `Enter`/`Space` activate the focused control. `Ctrl+C` copies, `Ctrl+S` saves to the configured folder, `Ctrl+Z`/`Ctrl+Shift+Z` undo/redo, and `Esc` cancels; canvas `Enter` copies. Copy/save actions finish directly; only the explicit configured-workflow button runs upload steps. Output contains only the selected image pixels and annotations.
- The advanced image/project editor remains available for opened Library files. It exposes the 25 SSX tools: shapes/arrows, pen, text, balloons, numbered steps, blur, pixelation, spotlight, magnifier, highlighting, crops, cutout, stamps, and image insertion. Undo/redo, object ordering, crop, rotate, flip, grayscale, fit/100% and panning are available. Annotation rendering runs on a worker and updates only dirty 512-pixel GPU tiles. Completed previews and events wake the UI directly; static tiles remain cached and tool selection does not rasterize. Large effect previews may reduce raster detail to bound interactive work, with a visible notice. Logical zoom, selection and annotation coordinates stay unchanged; saved/copied images render at their full requested resolution. This screenshot path is not zero-copy.
- Save/copy the flattened image, or save an editable `.ssxe` project. Open images and projects from Tools or the unified Library. Video files continue to open the quick video editor.
- Configure and run SSX after-capture/after-upload workflows, import ShareX `.sxcu` uploaders, select destinations, generate QR images, edit clipboard images, and view capture/upload history. **Share** is available on Library selections and completed video exports.
- New installations use a local destination. Remote uploads occur only when a sharing workflow/action is requested. Uploader credentials retain SSX's keyring integration.

RCR keeps toolkit settings in `projects/toolkit/settings.toml`, uploader definitions in `projects/toolkit/uploaders/`, and history under `projects/toolkit/data/`. Screenshot files initially use the recording root. Tools → Preferences can change the screenshot folder, format, cursor, naming pattern and HDR mapping. SSX settings import copies settings; relative `sxcu` file references are resolved against the original configuration folder. Import sibling `.sxcu` definitions individually. Existing SSX history is not migrated. Imported workflows can include external commands; inspect them before running. Imported workflow trigger bindings are retained in configuration but the active global keys are the RCR shortcuts listed above.

Paused video seeking uses a persistent Windows NVDEC decoder for ordinary source-FPS clips, a 48 MiB CPU frame cache, a bounded 192 MiB accounted GPU surface cache and a latest-request scrub worker. Drag the timeline ruler to scrub; click a clip to preview it immediately. Decoded preroll/neighbor frames remain resident for forward/backward dragging, and idle warming advances up to 0.75 seconds ahead with input checked between frames. Completed frames wake GPUI directly. Quick scrub previews are capped at 1920×1080; settled original-source refinement uses the physical preview bounds independently of export resolution. Retimed/effected compositions use the video-only filter-graph fallback. The fast path dynamically loads user-installed FFmpeg 7 shared libraries (`avformat-61`, `avcodec-61`, `avutil-59`, `swscale-8`) from `RCR_FFMPEG_LIB_DIR`, beside `RCR_FFMPEG`, or an absolute PATH directory. Unsupported/missing libraries fall back to the FFmpeg executable. Set `RCR_SCRUB_SOFTWARE=1` to test the persistent software decoder.

Windows prepares a reusable all-intra H.264 seek preview for an eligible single source up to four minutes long. It uses up to 1920×1080 without enlargement, fixed QP 24 and two slices per frame. Original decoding remains available during preparation or failure; settled refinement, playback and export use the original media. Effects and retiming use the composition fallback.

Completed previews persist in `%LOCALAPPDATA%/rcr/cache/seek-v3-managed`. `RCR_SEEK_CACHE_DIR` overrides the base directory; RCR creates its validated `seek-v3-managed` child there. The managed cache is capped at 3 GiB with reader-safe LRU eviction; each file must be below 2 GiB. Preparation requires 2,576 MiB free and may be blocked by active readers or ambiguous files that cannot safely be reclaimed. Legacy `seek-v2` and unrelated files are not migrated or cleaned automatically. Closing cancels unfinished preparation while completed copies remain reusable. Set `RCR_DISABLE_SEEK_PROXY=1` to disable preparation/reuse. See [cache ownership and limits](SEEK_PROXY_CACHE_AUDIT_2026-11-05.md); quality, memory and latency measurements are in [VALIDATION.md](VALIDATION.md).

**Tools → Workflows** uses a grouped sidebar and a focused details pane. Sidebar choices have visible keyboard focus: `Tab`/`Shift+Tab` navigate and `Enter`/`Space` select a workflow without running it. Actions appear once, in execution order, with controls to move, remove and append steps. Save-folder and destination settings are linked from the workflow; custom names and imported steps are retained.

**Tools → Utilities**, also available in the tray's **Tools** submenu, adds the everyday utilities from ShareX's Tools menu. See [utility coverage](UTILITIES.md) for controls, supported formats and remaining gaps. Screen color picking, rulers, pin selection, OCR selection and QR scanning use frozen-desktop overlays. Image processing, file hashing, folder indexing, clipboard access, OCR and FFmpeg jobs run off the UI thread with cancellation and bounded image-worker parallelism.

This combined build targets Windows. SSX's Linux service backends are retained in source; the combined Linux GPUI app is not implemented. RCR supplies Windows OCR and native pin-to-screen tools alongside the retained upstream services. Recording workflows open RCR's recorder; SSX GIF recording and its separate settings UI are not embedded. Remote destinations require user configuration and have not been tested against live accounts.

Leaving the video editor releases decoder sessions, decoded frames, preview images, thumbnails and waveforms. Closing to the tray also suspends the editor. Reopening rebuilds those caches while preserving edits and Undo/Redo history. Closing the image editor retires its image tiles without requiring another render of that editor. Process memory need not return to startup: loaded media libraries, graphics resources and allocator capacity remain. `scripts/verify-editor-memory.ps1` measures four warm/close/reopen cycles with an actual recording and checks resource ownership separately from Windows process memory.

Native rendering smoke check, using a synthetic image and hidden GPUI window:

```powershell
.\target\debug\rcr-desktop.exe --smoke-toolkit
```

The combined `rcr-desktop` and `rcr-toolkit` packages are **GPL-3.0-or-later**, with the original screenshot-library notices preserved in source. Original RCR core packages retain Apache-2.0. See `LICENSE-GPL-3.0`, `LICENSE`, `THIRD_PARTY.md` and `docs/SCREENSHOT-PROVENANCE.md`.
