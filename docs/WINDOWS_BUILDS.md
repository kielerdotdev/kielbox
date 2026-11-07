# Windows installation and builds

## Install Kielbox

Download [Kielbox-Setup.exe](https://github.com/kielerdotdev/kielbox/releases/download/preview/Kielbox-Setup.exe)
and run it. This is a direct executable download from GitHub Releases.

The native Windows wizard installs for your account, adds a Start menu entry,
and registers an uninstaller in **Installed apps**. It offers an installation
folder, an optional desktop shortcut, and optional startup when you sign in.
The default folder is `%LOCALAPPDATA%\Programs\Kielbox`. Neither the app nor its
installer needs .NET or PowerShell. The release executables link the C runtime
statically, so there is no separate Visual C++ runtime installer.

Current previews are unsigned. On first launch, existing users can choose
**Use existing data** and select their RCR/Kielbox working folder. Files are not
moved or merged. User data is separate from the application installation and
is preserved when uninstalling.

Installed builds provide **Settings → Updates**. Installation waits for the
application to close after recording and saves finish. Setup never terminates
an active capture. Closing to the tray is not Quit. The installer and uninstaller
also refuse to modify an application folder while programs are running from it.
See [update behavior](AUTO_UPDATE.md) for the implementation and limits.

## What CI builds

The [Windows workflow](../.github/workflows/windows.yml) runs on pushes to
`main`, `v*` tags, pull requests, and manual runs. It uses `windows-2025` x64,
the pinned Rust toolchain, one Cargo job, embedded shaders, and static CRT.

CI runs packaging and disk-tool tests, builds the desktop and cleanup CLI,
archives the corresponding source, and stages an inventoried payload with
licenses and checksums. It checks packaged cleanup help and the read-only setup
doctor from a separate data folder before compiling the installer.

[Inno Setup 6.7.4](https://github.com/jrsoftware/issrc/releases/tag/is-6_7_4)
compiles the installation wizard. The official distribution is pinned by SHA256
and verified with Authenticode before extracting its build tool in portable mode
under `.downloads/`. It does not install a global compiler. The app executable
is named `Kielbox.exe` inside the installation; Cargo crate names stay unchanged.

Installer versions are `<Cargo version>-ci.<run number>.<run attempt>`.
The build-evidence artifact contains:

- `Kielbox-Setup.exe`, the friendly manual download.
- `Kielbox-<version>-Setup.exe`, identical bytes under an immutable update name.
- `update.json`, the bounded update catalog with version, file, size and SHA256.
- `installer-build.json` and `INSTALLER-SHA256SUMS.txt`.

GitHub Actions wraps artifacts in ZIPs and retains them for 30 days. These are
for inspecting builds. The public **download** link uses the direct installer
asset in GitHub Releases, not an Actions artifact.

## Preview publication

A separate job publishes only successful pushes to `main` in
`kielerdotdev/kielbox`. PR/build jobs have read-only permissions. Publication
uses the repository token with `contents: write`; no token is in the app.

The publisher verifies clean source provenance, current main, the receipt,
catalog and asset hashes. It uploads the immutable versioned installer and
friendly download first, verifies GitHub's recorded digests, then publishes
`update.json` last. Old catalogs still refer to immutable installers. It refuses
to replace a published version with different bytes or downgrade the channel.
Only the automation-owned `preview` tag moves to the new commit. Stable release
publication is not enabled.

Previews use HTTPS and SHA256 integrity checks. They do not have a separate
signed catalog or TUF verification. CI packaging checks do not qualify capture,
audio devices, GPU playback, clean-machine installation, or real update behavior.

## Build an installer locally

Install Rust and Visual Studio Build Tools with **Desktop development with C++**
and a Windows SDK. `scripts/package-windows.ps1` adds static CRT flags for its
release build, preserving and restoring existing encoded Rust flags.

```powershell
New-Item -ItemType Directory -Force .downloads | Out-Null
git archive --format=zip --output=.downloads/kielbox-source.zip HEAD
powershell -NoProfile -File scripts/package-windows.ps1 `
  -OutputDirectory dist/windows `
  -SourceArchive .downloads/kielbox-source.zip
powershell -NoProfile -File scripts/package-installer-windows.ps1 `
  -PackageDirectory dist/windows -Version 0.1.0-ci.0.1 `
  -OutputDirectory dist/windows-installer
```

Choose fresh output folders and the current Cargo version. Packaging refuses
existing destinations, unexpected files, user-data folders and reparse paths.
The installer retains attribution, source and helper executables, while omitting
portable PowerShell launchers. It does not install, launch or publish the app.

Commit changes before making a distributable `git archive HEAD`; uncommitted
changes are not included. CI additionally requires clean source provenance.

## Advanced: portable and source builds

`scripts/package-windows.ps1` also produces a manual-update portable payload.
Keep the entire folder together and use its launcher with your existing data:

```powershell
.\Start-rcr.ps1 -WorkingDirectory 'D:\Kielbox data'
```

Portable executable names and existing data identifiers still use `rcr`.
Installed builds use `Kielbox.exe`; old projects and keyring entries are preserved.

## Validation

```powershell
powershell -NoProfile -File scripts/test-package-windows.ps1
powershell -NoProfile -File scripts/installer/Test-InstallerPackaging.ps1
powershell -NoProfile -File scripts/ci/check-windows-package.ps1 `
  -PackageDirectory dist/windows -ReportDirectory .downloads/package-check
```

The file-only admission suite checks refusal paths without running its synthetic
executables. Native install/update/uninstall qualification uses the same installer
template and application binary with `-ProofId <unique>` to isolate its Windows
registration and shortcuts. `/DIR=<owned test folder>` and `/NOAUTORUN` support
headless qualification without touching an existing Kielbox installation.

References: [Inno Setup documentation](https://jrsoftware.org/ishelp/),
[distribution verification](https://jrsoftware.org/isdl-verify.php), and
[Rust path-remapping limits](https://doc.rust-lang.org/rustc/remap-source-paths.html).
