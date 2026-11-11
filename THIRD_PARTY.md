# Third-party source and runtime components

## Rune

The desktop uses [Anomaly's Rune](https://github.com/anomalyco/rune), pinned to
`a9da7e3ebf5a8ab9bf19c3015032674b6e042df2` in Cargo.toml and Cargo.lock.
Rune and its motion, staging, and layout crates use the MIT license, retained
in `assets/licenses/Rune-MIT.txt`. Rune embeds Inter and IBM Plex Mono under
the SIL Open Font License; their complete notices are retained alongside it.

Windows preview frames cross from the existing D3D11 producer to Rune's D3D12
renderer through named shared GPU textures and release fences in
`crates/desktop/src/rune_gpu.rs`.

## NVENC bindings

`crates/nvenc-sys` contains the Windows NVENC 12.1 generated bindings, GUID definitions and API-version constants distributed with `nvidia-video-codec-sdk` version 0.4.0 by Viliam Vadocz. That crate's MIT license is retained in `crates/nvenc-sys/LICENSE`. The original downloaded crate is in the ignored `.downloads` development directory; it is not needed to build.

The local fork represents generated C enums as transparent integer newtypes with associated constants. Native zero-initialized structs and future driver enum values therefore do not create invalid Rust enum discriminants. Generated layout tests remain in the source. The runtime wrapper loads NVIDIA's installed system driver library; the repository does not redistribute NVIDIA driver binaries or the SDK.

Published source: https://crates.io/crates/nvidia-video-codec-sdk/0.4.0

## FFmpeg and FFplay

The Windows seek worker can dynamically load user-installed FFmpeg 7 shared libraries (avformat/avcodec 61, avutil 59 and swscale 8). Its public-prefix Rust FFI declarations originate from n7.1.1 and reject incompatible library majors. Separate generated full-structure bindings used by native playback, optional GPU decoding and native audio require the exact supported FFmpeg 7.0.2 runtime before structures are accessed; native audio also checks its resampling/filter components. Header notices, provider license and generation hashes are preserved under `crates/editor-core/src/playback_gpu/ffmpeg7/`. NVIDIA acceleration uses the installed driver through FFmpeg CUVID/NVDEC. These shared libraries are not bundled. Public-prefix API references: https://github.com/FFmpeg/FFmpeg/tree/n7.1.1 and https://github.com/FFmpeg/FFmpeg/blob/n7.1.1/libavcodec/cuviddec.c.

The editor invokes user-installed FFmpeg, FFprobe and FFplay executables. Their licenses depend on the user's build configuration. These executables and codec binaries are not bundled here. Recorder/replay output uses RCR's Rust Matroska writer.

## ClipFlow

ClipFlow is a design reference for a small instant-replay workflow. No ClipFlow source code or assets are included in RCR.

Reference: https://github.com/jaimitus/ClipFlow

## Other Rust dependencies

`crates/dpdfnet` is a safe Rust port of CEVA's Apache-2.0 DPDFNet-2 48 kHz inference graph/kernels, with streaming framing inspired by HushMic under Apache-2.0. The pretrained weight data is bundled in `assets/models`; upstream licenses, exact source revisions and hashes are retained in `crates/dpdfnet/NOTICE.md`, `LICENSE-CEVA`, and `LICENSE-HUSHMIC`. The denoiser does not compile or invoke HushMic's C engine or ONNX Runtime. Python/ONNX are used only to regenerate development reference fixtures. RustFFT supplies FFT processing in Rust.

Exact dependency versions are recorded in `Cargo.lock`; each dependency retains its own license. Original RCR core packages retain Apache-2.0. The combined desktop and toolkit packages are GPL-3.0-or-later.

## Geist and Geist Mono

`assets/fonts/Geist.ttf` and `GeistMono.ttf` are the Latin subsets embedded in the user-supplied `rcr Quick Editor.html` design. The source WOFF2 files were converted to TrueType containers with FontTools for Windows font loading; the outlines, family names and font metadata are retained. They are bundled under the SIL Open Font License 1.1; both original copyright notices and complete licenses from the supplied document are retained in `assets/fonts/OFL.txt`.

The HTML is a visual reference. Its bundled JavaScript and mock recording data are not executed or distributed with the application. `assets/icons` contains locally authored monochrome SVG icons.


## Kielbox Cloud

`cloud/` is a TanStack Start application for Cloudflare Workers (not a Cargo member).
Its direct npm dependencies are pinned in `cloud/package.json` and `cloud/package-lock.json`;
licenses below are read from the installed packages. Transitive packages keep their own
licenses and are recorded in the lock file.

| Package | Version | License |
| --- | --- | --- |
| `@tanstack/react-start`, `@tanstack/react-router`, `@tanstack/react-query` | 1.169.5, 1.170.46, 5.107.1 | MIT |
| `react`, `react-dom` | 19.3.1 | MIT |
| `tailwindcss`, `@tailwindcss/vite` | 4.3.5 | MIT |
| `@base-ui/react` | 1.9.0 | MIT |
| `lucide-react` | 1.72.0 | ISC |
| `hono` | 4.13.18 | MIT |
| `zod` | 4.6.7 | MIT |
| `aws4fetch` | 1.0.20 | MIT |
| `jose` | 6.2.17 | MIT |

Build and test tooling (not shipped): `vite` 8.3.8 (MIT), `@vitejs/plugin-react` 6.3.0 (MIT),
`@cloudflare/vite-plugin` 1.69.0 (MIT), `@cloudflare/vitest-plugin` 1.8.0 (MIT), `vitest`
4.1.12 (MIT), `wrangler` 4.160.1 (MIT OR Apache-2.0), `typescript` 6.0.3 (Apache-2.0),
`@types/node`, `@types/react` and `@types/react-dom` (MIT).

The explorer serves its own copy of Geist and Geist Mono (`cloud/public/fonts`, identical
to `assets/fonts`) under the SIL Open Font License 1.1, with `OFL.txt` alongside. No font
or script is loaded from a third-party host.

`crates/cloud` is the Rust client (GPL-3.0-or-later). New direct Cargo dependencies it and
the desktop's Cloud code add, from `cargo metadata`: `bytes` 1.12.1 (MIT), `futures-util`
0.3.34 (MIT OR Apache-2.0), `tokio-util` 0.7.20 (MIT), `url` 2.5.8 (MIT OR Apache-2.0), and
for tests `wiremock` 0.6.5 (MIT/Apache-2.0). `tokio`, `reqwest` and `tempfile` were already
in the graph. The desktop also enables `image`'s `jpeg` feature for upload thumbnails.
Cloudflare's services are used through their public APIs; no Cloudflare code is bundled.

## Screenshot and workflow code origin

The screenshot, annotation, workflow and upload libraries originated in
[SSX](https://github.com/kielerdotdev/ssx), revision
`bd8edc6f56b22becaaca15d2e0b226a5d967cb4c`, under GPL-3.0-or-later.
They are now maintained directly as RCR workspace crates under `crates/`.
There is no vendored SSX workspace or external SSX application dependency.
Original notices are preserved in source and `LICENSE-GPL-3.0`.

See [screenshot source provenance](docs/SCREENSHOT-PROVENANCE.md) for the migration
and retained compatibility identifiers. Kielbox's Rune interface uses these native
libraries; the optional standalone tools are separate workspace packages.

## Utility libraries and references

Windows installers are built with [Inno Setup 6.7.4](https://github.com/jrsoftware/issrc/releases/tag/is-6_7_4).
Its license permits distributing generated installers; the build tool is downloaded
separately and is not part of the application payload. The updater downloads the
same installer executable using the channel's size and SHA256 catalog.

QR reading uses `rqrr` (MIT OR Apache-2.0), a Rust QR detector/decoder based on quirc; its dependency notices remain in the published crates. The existing SSX QR renderer supplies QR creation. `sha1`, `sha2`, `md-5` and `crc32fast` supply streaming file digests under their respective published licenses. Windows OCR uses the installed Windows.Media.Ocr engine; no model or external OCR executable is bundled. Video utilities invoke user-installed FFmpeg/FFprobe without a shell.

ShareX's official tools list and utility documentation informed the feature checklist and interaction behavior. No ShareX source code or assets were copied for these utilities. References: https://getsharex.com/ and https://getsharex.com/docs/index-folder.

## Marketing website

The standalone `site/` project builds with Astro 7.4.11 (MIT) and deploys static
assets with Wrangler 4.160.1 (MIT OR Apache-2.0). Neither tool is shipped as browser
JavaScript. Geist and Geist Mono are self-hosted in `site/public/fonts` under
SIL Open Font License 1.1, with `OFL.txt` alongside. The supplied desktop screenshots
are documented in `site/public/landing/ASSETS.md`.
