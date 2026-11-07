# Kielbox

Screen capture, clips, and disk tools in one desktop app. I’m building it to
replace the handful of utilities I install and configure on every machine.

Currently Windows only. macOS and Linux are planned.

- Screen recording and instant replay, with separate audio tracks.
- Screenshots, annotations, and sharing through configured destinations.
- Clip trimming, audio editing, and video export.
- A clip viewer for browsing videos while screen-sharing with friends.
- Disk usage views and a CLI for cleaning old development files.

This is early software. Game recording requires an NVIDIA GPU with AV1 encoding.
Editing, exports, and quick screencasts require FFmpeg and FFprobe on `PATH`.
Screenshots and native recording work without them.

## Download

Download [Kielbox-Setup.exe](https://github.com/kielerdotdev/kielbox/releases/download/preview/Kielbox-Setup.exe)
and run it. Kielbox installs for your Windows account and appears in the Start
menu. The setup wizard lets you choose the installation folder, a desktop
shortcut, and whether Kielbox starts when you sign in. Preview builds are unsigned.

Used RCR or an older Kielbox build? Choose **Use existing data** on the welcome
screen and select the folder containing your projects and recordings. Your files
stay where they are.

Installed builds check for updates in the background. **Settings → Updates**
lets you check, download, and restart to update. Updates wait for recording and
saves to finish. Closing the window leaves the app in the tray; choose **Quit
Kielbox** to exit. Uninstall through Windows **Installed apps** or **Apps & features**.

Kielbox was previously called RCR. Existing data formats keep their older
identifiers for compatibility. Source and portable builds use manual updates;
see [Windows builds](docs/WINDOWS_BUILDS.md).

## Build

Install Rust and Visual Studio Build Tools with **Desktop development with C++**
and a Windows SDK. The Rust version is pinned in `rust-toolchain.toml`.

```powershell
git clone https://github.com/kielerdotdev/kielbox.git
cd kielbox
cargo run --locked -p desktop -j1
```

See [Windows builds](docs/WINDOWS_BUILDS.md) for installer packaging, CI, and
advanced portable builds. The app is native Rust. Neither the app nor its
installer requires .NET or a PowerShell launcher.

## Details

[Utilities](docs/UTILITIES.md) · [Development](docs/DEVELOPMENT.md) ·
[Validation](docs/VALIDATION.md) · [Dependencies and licenses](THIRD_PARTY.md)

The desktop app is GPL-3.0-or-later. Original RCR core packages are Apache-2.0.
See [LICENSE-GPL-3.0](LICENSE-GPL-3.0) and [LICENSE](LICENSE).
