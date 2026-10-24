# RCR

Screen recording, instant replay, screenshots, and disk usage in one Windows app.
Built in Rust with GPUI.

**Early development.** Recording currently requires an NVIDIA GPU with AV1 encoding
support. Linux and AMD/Intel recording are planned.

## Download

1. Open [Windows builds](https://github.com/kielerdotdev/rcr/actions/workflows/windows.yml).
2. Select the latest successful run and download **rcr-windows-x64** under **Artifacts**.
3. Extract the download into a writable folder and run **rcr.exe**.

You need to be signed into GitHub to download build artifacts. Builds are unsigned.

For video editing, install FFmpeg and FFprobe and add them to `PATH`. Use a build
with `av1_nvenc` support for NVIDIA exports. FFmpeg 7 shared DLLs are optional for
accelerated editor previews; set `RCR_FFMPEG_LIB_DIR` to their folder.
Recording and screenshots do not require FFmpeg.

## What you can do

- **Record or save a replay.** Open **Capture**, choose your screen and audio sources,
  then start recording or enable instant replay. **Save replay** keeps the recent
  footage without stopping capture. Desktop/application audio and the microphone
  are recorded as separate tracks.
- **Edit a clip.** Open it in **Library**. Drag the clip edges to trim, split it,
  adjust audio, reframe, and export. Edits autosave separately from the recording.
- **Take a screenshot.** Press **Ctrl+PrintScreen**, select a region, annotate it,
  then copy or save. Sharing destinations are configured under **Tools**.
- **Find large files.** Open **Tools → Disk usage** and choose a drive or folder.
  Run RCR as administrator for fast NTFS scans of a whole drive.

Optional microphone noise suppression runs when saving a replay or exporting a
clip, so it does not add work to live capture.

## Shortcuts

| Shortcut | Action |
| --- | --- |
| Ctrl+Shift+F9 | Toggle instant replay |
| Ctrl+Shift+F10 | Save replay |
| Ctrl+Shift+F11 | Start / stop recording |
| Ctrl+PrintScreen | Screenshot a region |
| Alt+PrintScreen | Screenshot the active window |
| Shift+PrintScreen | Screenshot the desktop |

Change these in **Settings → Global shortcuts**. If another app already owns a
shortcut, RCR reports the conflict; the buttons still work.

## Where files go

Recordings go to `recordings/`, exports to `recordings/exports/`, and saved edits
and toolkit settings to `projects/`. These paths are relative to the folder RCR
starts in. Keep the same working folder when updating; keep your data folders.
You can change recording and screenshot locations in the app.

Closing the window normally leaves RCR in the tray. Use **Quit rcr** from the tray
to exit completely.

## Build from source

Install [Rust](https://rustup.rs/) and Visual Studio Build Tools with **Desktop
development with C++** and the **Windows SDK**. The Rust version is pinned in
`rust-toolchain.toml`. An NVIDIA SDK or CUDA toolkit is not needed.

```powershell
git clone https://github.com/kielerdotdev/rcr.git
cd rcr
cargo run --locked -p rcr-desktop -j1
```

For a release build:

```powershell
pwsh -File scripts/package-windows.ps1
```

The executable is written to `dist/windows/rcr.exe`. Windows CI builds it on pushes
to `main`, version tags, pull requests, and manual runs.

## More

[Tools and utilities](docs/UTILITIES.md) · [Development notes](docs/DEVELOPMENT.md) ·
[Testing and known limitations](docs/VALIDATION.md) · [Third-party licenses](THIRD_PARTY.md)

The combined app is **GPL-3.0-or-later**. Original RCR core packages remain
Apache-2.0; see [LICENSE-GPL-3.0](LICENSE-GPL-3.0) and [LICENSE](LICENSE).
