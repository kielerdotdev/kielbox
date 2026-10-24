# Third-party source and runtime components

## GPUI 0.2.2

`vendor/gpui-0.2.2` is the published GPUI 0.2.2 crate from the Zed project, licensed under Apache-2.0. Its license is retained as `vendor/gpui-0.2.2/LICENSE-APACHE`.

Local changes add a Windows D3D11 shared-texture surface source, scene/window plumbing, keyed-mutex synchronization and cached SRV import in the DirectX renderer. Two floating point literals in the Taffy integration have explicit `f32` types for Rust 1.99 compatibility. The return type of the already-public window input dispatcher is public so native hidden-window interaction regression checks can call it. The workspace pins this copy through `[patch.crates-io]`.

`crates/desktop/src/text_input.rs` is adapted from this crate's `examples/input.rs`, retaining the GPUI text input, IME, selection and clipboard approach under Apache-2.0. RCR-specific changes expose an editable value and use the application's styling.

Upstream: https://github.com/zed-industries/zed/tree/main/crates/gpui

## NVENC bindings

`crates/nvenc-sys` contains the Windows NVENC 12.1 generated bindings, GUID definitions and API-version constants distributed with `nvidia-video-codec-sdk` version 0.4.0 by Viliam Vadocz. That crate's MIT license is retained in `crates/nvenc-sys/LICENSE`. The original downloaded crate is in the ignored `.downloads` development directory; it is not needed to build.

The local fork represents generated C enums as transparent integer newtypes with associated constants. Native zero-initialized structs and future driver enum values therefore do not create invalid Rust enum discriminants. Generated layout tests remain in the source. The runtime wrapper loads NVIDIA's installed system driver library; the repository does not redistribute NVIDIA driver binaries or the SDK.

Published source: https://crates.io/crates/nvidia-video-codec-sdk/0.4.0

## FFmpeg and FFplay

The Windows seek worker can also dynamically load user-installed FFmpeg 7 shared libraries (avformat/avcodec 61, avutil 59 and swscale 8). Its Rust FFI declarations use the public n7.1.1 ABI prefixes and reject other library majors before dereferencing those structures. NVIDIA acceleration uses the installed driver through FFmpeg CUVID/NVDEC. These shared libraries are not bundled. API references: https://github.com/FFmpeg/FFmpeg/tree/n7.1.1 and https://github.com/FFmpeg/FFmpeg/blob/n7.1.1/libavcodec/cuviddec.c.

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


## Screenshot and workflow code origin

The screenshot, annotation, workflow and upload libraries originated in
[SSX](https://github.com/kielerdotdev/ssx), revision
`bd8edc6f56b22becaaca15d2e0b226a5d967cb4c`, under GPL-3.0-or-later.
They are now maintained directly as RCR workspace crates under `crates/`.
There is no vendored SSX workspace or external SSX application dependency.
Original notices are preserved in source and `LICENSE-GPL-3.0`.

See [screenshot source provenance](docs/SCREENSHOT-PROVENANCE.md) for the migration
and retained compatibility identifiers. RCR's GPUI interface uses these native
libraries; the optional standalone tools are separate workspace packages.

## Utility libraries and references

QR reading uses `rqrr` (MIT OR Apache-2.0), a Rust QR detector/decoder based on quirc; its dependency notices remain in the published crates. The existing SSX QR renderer supplies QR creation. `sha1`, `sha2`, `md-5` and `crc32fast` supply streaming file digests under their respective published licenses. Windows OCR uses the installed Windows.Media.Ocr engine; no model or external OCR executable is bundled. Video utilities invoke user-installed FFmpeg/FFprobe without a shell.

ShareX's official tools list and utility documentation informed the feature checklist and interaction behavior. No ShareX source code or assets were copied for these utilities. References: https://getsharex.com/ and https://getsharex.com/docs/index-folder.
