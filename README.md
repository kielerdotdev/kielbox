# Kielbox

Your toolbox. Every machine.

Screen recording, instant replay, screenshots, sharing, disk usage, and everyday
utilities in one desktop app. Built in Rust with GPUI. The long-term goal is a
familiar toolbox across Windows, macOS, and Linux; the current desktop app targets
Windows.

Previously called RCR. Existing executable names, Cargo packages, configuration
identifiers, and data locations still use `rcr` for compatibility.

**Early development.** Game recording and replay currently require an NVIDIA GPU
with AV1 encoding support. Linux and AMD/Intel game recording are planned.

## Download

1. Open [Windows builds](https://github.com/kielerdotdev/kielbox/actions/workflows/windows.yml).
2. Select the latest successful run and download **rcr-windows-x64** under **Artifacts**.
3. Extract the download into a writable folder and run **rcr.exe**.

You need to be signed into GitHub to download build artifacts. Builds are unsigned.

For video editing, install FFmpeg and FFprobe and add them to `PATH`. Use a build
with `av1_nvenc` support for NVIDIA exports. FFmpeg 7 shared DLLs are optional for
faster software playback and seeking; set `RCR_FFMPEG_LIB_DIR` to their folder.
The native decoder requires the compatible FFmpeg 7.0.2 DLL versions. Other
runtimes use the external FFmpeg path. GPU preview is separately optional in Settings.
Game recording, instant replay and screenshots do not require FFmpeg.

## What you can do

- **Record or save a replay.** Open **Capture**, choose your screen and audio sources,
  then start recording or enable instant replay. **Save replay** keeps the recent
  footage without stopping capture. Desktop/application audio and the microphone
  are recorded as separate tracks.
- **Edit a clip.** Choose **Edit** on a Library video, or **Open in Editor** while watching. Drag the clip edges to trim, split it,
  adjust audio, reframe, and export. Edits autosave separately from the recording.
- **Watch clips together.** Click a Library video, choose **Watch clips**, or use **Clip show**
  (Ctrl+Enter) in the editor, then share the RCR window in your voice/chat app. Clips play in Library order;
  Left/Right switches clips, Space pauses, M mutes playback, and Escape opens editing.
  Playback volume changes leave the audio in your saved edits and exports intact.
- **Take a screenshot.** Press **Ctrl+PrintScreen**, adjust the selection directly
  on your desktop and press the round shutter. Annotate, copy or save the result. Sharing destinations are configured under **Tools**.
- **Record a quick screencast.** Choose **Record video** under **Tools → Screenshots**, or press **Ctrl+Shift+PrintScreen**.
  The desktop picker opens in Video mode; its photo/video switch works in the same toolbar.
  Choose Selection, Screen or Window and press the round shutter. Area recording lets
  you move/resize the selection and confirm before starting. Sound, microphone and
  pointer controls sit beside capture; **More** holds frame rate and countdown.
  Pause/resume or stop from the app’s recording status row; the shortcut and tray can also stop it.
  **Copy file** prepares the saved H.264/AAC MP4 for pasting into Discord or Slack.
  This recorder runs separately from game recording and instant replay.
  It requires FFmpeg with `libx264` and AAC (or set `RCR_FFMPEG`), saves under
  `recordings/screencasts/`, and limits a clip to ten minutes and 1080p output.
  Regions must fit on one display. Audio uses Windows default devices.
- **Find large files.** Open **Tools → Disk usage** and choose a drive or folder.
  Run RCR as administrator for fast NTFS scans of a whole drive.
- **Clean up agent disk usage.** Run `rcr-clean C:\src C:\Users\me\.codex\worktrees`
  to preview old build output, dependencies, project caches and removable worktrees.
  Add `--apply` to delete; the default age is 30 days. See [cleanup CLI](crates/rcr-clean/README.md).

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
| Ctrl+Shift+PrintScreen | Open capture controls in Video mode / stop screencast |

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
git clone https://github.com/kielerdotdev/kielbox.git
cd kielbox
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
