# Kielbox

Screen capture, clips, and disk tools in one desktop app. I’m building it to
replace the handful of utilities I install and configure on every machine.

Currently Windows only. macOS and Linux are planned. The Rune frontend is an
experimental preview with incomplete parity; see [current capabilities and gaps](docs/STATUS.md).
Configured upload destinations (the generic uploader library) and the former watch/share
windows are not wired in Rune. The one upload target that is wired is Kielbox Cloud, a personal
Cloudflare-hosted library you deploy yourself; see [the setup guide](docs/CLOUD.md). It is new
and has not yet run on Windows or against a real deployment.

- Screen recording and instant replay, with separate audio tracks.
- Screenshots and annotations with local image/project output.
- Clip trimming, audio editing, and video export.
- A local library with search, filters, favorites and video thumbnails.
- Optional Kielbox Cloud: upload captures to your own Cloudflare account, browse them on the
  web and share revocable links ([setup](docs/CLOUD.md)).
- Disk usage views and a CLI for cleaning old development files.

This is early software. Game recording requires an NVIDIA GPU with AV1 encoding.
Editing, exports, and quick screencasts require FFmpeg and FFprobe on `PATH`.
Screenshots and native recording work without them.

## Download

Download [Kielbox-Setup.exe](https://github.com/kielerdotdev/kielbox/releases/download/preview/Kielbox-Setup.exe)
and run it. Kielbox installs for your Windows account and appears in the Start
menu. The setup wizard lets you choose the installation folder, a desktop
shortcut, and whether Kielbox starts when you sign in. Preview builds are unsigned.

Used RCR or an older Kielbox build? In an installed build, open
**Settings → Storage → Use existing data…** and choose the existing workspace.
Quit safely and reopen to activate it. Files stay in place; nothing is moved or
merged. First-run guidance opens Storage. Source/portable builds can select a
root with `--working-directory "C:\path\to\existing-data"` on each launch.
**Capture folder** changes only the recordings location, not the whole workspace.
Native installer and migration acceptance remain pending.

Installed builds check for updates in the background. **Settings → App → Updates**
lets you check, download, and restart to update. Updates and quit wait for capture, jobs and editable recovery checkpoints.
See the [desktop status](docs/STATUS.md) for validation limits. Closing the window leaves the app in the tray; choose **Quit
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
[Desktop status](docs/STATUS.md) · [Dependencies and licenses](THIRD_PARTY.md)

The desktop app is GPL-3.0-or-later. Original RCR core packages are Apache-2.0.
See [LICENSE-GPL-3.0](LICENSE-GPL-3.0) and [LICENSE](LICENSE).
The public marketing website is a separate [Astro project](site/README.md),
configured for [box.kieler.dev](https://box.kieler.dev). The cloud companion
remains a separate deployment in `cloud/`.
