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

Open [Windows builds](https://github.com/kielerdotdev/kielbox/actions/workflows/windows.yml),
select a successful run, and download **kielbox-windows-x64** under **Artifacts**.
GitHub requires you to sign in. Builds are unsigned.

Extract the whole package. Choose a folder for your data and launch from the
extracted package:

```powershell
New-Item -ItemType Directory -Force "$env:USERPROFILE\Videos\Kielbox" | Out-Null
.\Start-rcr.ps1 -WorkingDirectory "$env:USERPROFILE\Videos\Kielbox"
```

If you already use Kielbox, keep your existing working folder. Recordings,
projects, and settings depend on it. Closing the window leaves the app in the
tray; use **Quit rcr** to exit.

Kielbox was previously called RCR. Executables, Cargo packages, and existing
data formats still use that name. Automatic updates are [being designed](docs/AUTO_UPDATE.md).

## Build

Install Rust and Visual Studio Build Tools with **Desktop development with C++**
and a Windows SDK. The Rust version is pinned in `rust-toolchain.toml`.

```powershell
git clone https://github.com/kielerdotdev/kielbox.git
cd kielbox
cargo run --locked -p rcr-desktop -j1
```

To build a packaged release:

```powershell
powershell -NoProfile -File scripts/package-windows.ps1
```

The package is written to `dist/windows/`. [Windows CI](docs/WINDOWS_BUILDS.md)
builds and checks it on pushes to `main`, version tags, pull requests, and manual runs.

## Details

[Utilities](docs/UTILITIES.md) · [Development](docs/DEVELOPMENT.md) ·
[Validation](docs/VALIDATION.md) · [Dependencies and licenses](THIRD_PARTY.md)

The desktop app is GPL-3.0-or-later. Original RCR core packages are Apache-2.0.
See [LICENSE-GPL-3.0](LICENSE-GPL-3.0) and [LICENSE](LICENSE).
