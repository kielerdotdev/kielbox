# Installation and updates

Windows distributions use Inno Setup 6.7.4. `Kielbox-Setup.exe` installs the native
application, adds a Start menu entry, and registers an uninstaller in Windows.
The desktop shortcut is optional. Neither installation nor updates force-close
running applications. Release builds statically link the Microsoft C runtime.

Download the installer directly from the
[Preview release](https://github.com/kielerdotdev/kielbox/releases/tag/preview).
GitHub Actions artifacts remain ZIP containers for build inspection; they are
not the installation instructions or the primary download.

## Application and data folders

The default application folder is `%LOCALAPPDATA%\Programs\Kielbox`. A versioned
`kielbox-install.json` beside `Kielbox.exe` identifies managed installations.
Source and old portable builds do not acquire automatic updates merely because
they were compiled in release mode.

New installed profiles use `%LOCALAPPDATA%\KielboxData\workspace`. The small
bootstrap configuration lives in `%LOCALAPPDATA%\KielboxData\bootstrap.json`,
outside the replaceable application folder. Recording does not start on launch.

The welcome panel and Settings offer **Use existing data**. Choose the folder
containing the existing projects and recordings; the selection takes effect on
restart. No media is moved or merged. `--working-directory <existing folder>`
provides the same explicit selection. Invalid saved configuration produces an
error instead of silently switching to an empty profile.

Uninstall removes the managed application and its integrations. Captures,
projects, exports, the selected data folder, and bootstrap settings are retained.

## Update controls

Settings shows the installed version, channel, last check, available download,
and **Restart to update** when a verified installer is ready. Automatic checks,
idle downloads, and installation on Quit are independently configurable.
Checks and hashing run in a background worker, with bounded network operations.
An update does not start a recording or upload recordings, paths, or preferences.

The catalog is `update.json` under the fixed project release channel:

- Preview: `https://github.com/kielerdotdev/kielbox/releases/download/preview/`
- Stable: `https://github.com/kielerdotdev/kielbox/releases/download/stable/`

Stable is reserved. A catalog identifies schema version 1, `appid: Kielbox`,
channel, SemVer version, immutable installer filename, size, SHA256, and optional
notes. Installer names use `Kielbox-<version>-Setup.exe`. The application constructs
the download URL from the trusted channel and validated filename; the catalog
cannot supply a different host or filesystem path. Older versions and mismatched
channels are rejected. Cached installers are checked again before applying.

## Shutdown and application

Restart waits for recording and replay finalization, accepted media saves,
exports, project autosaves, native dialogs, toolkit operations, and disk scans.
Idle worker threads are not treated as unfinished jobs. The GUI and its worker
owners retire before the pending installer is launched; the preferences writer
must also confirm a successful flush.

The installer receives `/WAITFORPID` and waits for the old process to exit before
writing application files. It has no force-kill path. Other processes using the
installation are checked before handoff. A bounded handoff marker prevents an
old build reopening while the verified installer runs; a failed update permits
the previous version to reopen with a recovery notice.

Ordinary Quit leaves Kielbox closed. Explicit Restart requests relaunch with the
selected data folder. Closing to the tray neither quits nor applies an update.

## Publication and trust

Main-branch CI builds the release executable and installer. A separate publication
job verifies source provenance and asset hashes, then publishes the owned
`preview` prerelease. Pull requests do not receive release-write permission.

The versioned installer is immutable. The friendly `Kielbox-Setup.exe` download
and catalog are promoted only after upload verification; the catalog is last.
An older run cannot replace a newer channel version. CI versions follow
`0.1.0-ci.<run>.<attempt>` while the base package version remains 0.1.0.

Preview builds are unsigned. HTTPS to the project-owned GitHub release and
SHA256 provide transport and file-integrity checks; a digest is not a publisher
signature. Signing credentials are not included in the repository.

## Verification

Controller and bootstrap tests cover metadata rejection, version ordering,
worker admission, cache integrity, configuration preservation, and handoff
recovery. Packaging checks cover payload and output boundaries.

Native acceptance uses the actual desktop binary and installer, isolated under
an explicitly marked test root with a unique Windows installation identity.
It checks install A, update to B, unchanged media/settings sentinels, shortcut
and uninstall registration, and uninstall while retaining data. Native results
and exact artifact hashes are recorded separately from synthetic tests.

Run `scripts/installer/Test-NativeInstall.ps1 -PackageDirectory <verified-release-payload>`
in PowerShell 7 for this opt-in Windows test. It also checks the public update catalog through the installed app's HTTP client.
The same lifecycle test gates installer publication in CI.
