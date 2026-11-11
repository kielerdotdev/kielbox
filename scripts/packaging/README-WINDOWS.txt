RCR for Windows -- early development, unsigned build

START AND KEEP YOUR DATA
Extract the entire package to a new folder. Do not copy over a running build.
Open PowerShell and run, using your existing working/data folder:
  powershell -NoProfile -File .\Start-rcr.ps1 -WorkingDirectory "C:\Users\you\RCR data"
Create that folder yourself if this is your first installation. The launcher
requires an existing folder; it never migrates or copies your data. Keep using
the SAME working folder after an upgrade. New package folders can sit alongside
old builds. Quit the existing app from its tray before launching another build.

Recordings, exports, projects, settings, logs and caches can depend on the working
folder, NOT the location of rcr.exe. Defaults include recordings\ and projects\.
Recording/screenshot locations selected in Settings may be elsewhere. Keep all
existing folders. Running rcr.exe directly uses the working folder supplied by
Windows or your shell; use the launcher to make this choice explicit. A shortcut
can instead set its "Start in" field to your existing data folder.
Closing the main window normally leaves RCR in the tray. Choose Quit rcr to exit.

CHECK SETUP WITHOUT RECORDING
  powershell -NoProfile -File .\Verify-package.ps1
  powershell -NoProfile -File .\Diagnose-rcr.ps1 -WorkingDirectory "C:\Users\you\RCR data"
Diagnose reports setup as JSON without starting capture or a microphone. Missing
optional capabilities can be reported without making every feature unusable.
No tools are downloaded or installed. No device defaults are changed.

REQUIREMENTS AND OPTIONAL VIDEO TOOLS
Game recording/replay requires a supported NVIDIA AV1 encoder and installed
driver. This package does not include driver binaries, CUDA, FFmpeg, FFprobe or
FFplay. Screenshots and native game capture do not require FFmpeg. Editing,
exports and quick screencasts need compatible user-installed FFmpeg/FFprobe.
Set PATH or RCR_FFMPEG / RCR_FFPROBE explicitly if needed. Optional native media
decoding needs the supported exact FFmpeg 7.0.2 shared runtime; set
RCR_FFMPEG_LIB_DIR to its directory. Unsupported versions use supported external
paths or produce an actionable error. Obtain tools and matching licenses from
their provider. Do not copy arbitrary codec DLLs next to rcr.exe.

PACKAGE AND LICENSES
package-manifest.json records each payload SHA256, size, build kind, source
revision and whether the source tree was dirty. SHA256SUMS.txt also hashes the
manifest. These hashes do not authenticate an unsigned publisher. Rune packages embed their shader metadata and do not require the original
source checkout at runtime. Builds still need clean-folder validation.
The desktop is GPL-3.0-or-later; original core code is Apache-2.0. License texts,
model/font/binding notices and Cargo.lock are included. THIRD_PARTY.md provides
component provenance. No FFmpeg runtime is redistributed by this packager.

Before redistributing a build, review the exact transitive dependency license
requirements and supply the exact corresponding source including local changes,
as required by its licenses. A repository URL or dirty git revision alone does
not include those local changes. The optional corresponding-source archive is
supplied by the packager and must be reviewed to match this build. Staging and
integrity checks alone are not a distribution compliance certification.

Read README.md for features, global shortcuts and development instructions.
