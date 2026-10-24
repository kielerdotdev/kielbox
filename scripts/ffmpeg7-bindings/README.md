# FFmpeg 7.0.2 Rust binding preparation

This is an isolated preparation tool, outside the application workspace. It does
not implement a decoder or change application dependencies. Normal RCR builds
do not use Clang, Python, this helper, or generated preparation artifacts.

Run from the workspace with **PowerShell 7 x64**, only when no other Cargo job is
running. All fetched/extracted/generated files and helper build outputs stay in
`.downloads/tooling/ffmpeg7-bindings/`; the script never installs or replaces
anything in `C:/bin`, a Python environment, or the system toolchain.

```powershell
./scripts/prepare-ffmpeg7-bindings.ps1 -Mode Fetch -RuntimeDirectory C:/bin
```

Fetch uses the provider's exact
[7.0.2 shared-build ZIP](https://github.com/GyanD/codexffmpeg/releases/tag/7.0.2),
whose release identifies FFmpeg source commit `e3a61e9103`. It extracts only
original public `.h` files and licenses, including provider-generated
`libavutil/avconfig.h` and version headers. It hashes the four component DLL
entries directly from ZIP streams and requires byte-for-byte matches with the
existing absolute runtime files. No FFmpeg binary is extracted or executed.
Missing original headers or mismatched DLLs stop preparation; no configuration
header, structure padding, or ABI offset is guessed.

The LLVM-owned [PyPI libclang 18.1.1 distribution](https://pypi.org/project/libclang/18.1.1/)
supplies `libclang-18.1.1-py2.py3-none-win_amd64.whl`, pinned to SHA-256
`4dd2d3b82fab35e2bf9ca717d7b63ac990a3519c7e312f19fa8e86dcc712f7fb`.
Only its exact native entry
`libclang-18.1.1.data/platlib/clang/native/libclang.dll` and license are extracted.
No pip/Python installation runs.
Fetch records archive/header/DLL hashes, source URLs and paths in a fresh
`inputs-<id>/inputs.json`; partial or failed artifacts remain workspace-local.

The provider ZIP has no checksum in the verified release asset list. Fetch
records its computed hash and establishes the runtime byte match. Supply that
hash explicitly to Generate as an integrity pin:

```powershell
./scripts/prepare-ffmpeg7-bindings.ps1 -Mode Generate `
  -InputRecord .downloads/tooling/ffmpeg7-bindings/inputs-<id>/inputs.json `
  -ExpectedArchiveSha256 <hash-printed-by-Fetch>
```

Generate verifies the input hashes, copies this helper into a fresh owned
directory, resolves cached dependencies **offline**, and runs Cargo serially.
Its exact direct dependencies are bindgen `0.71.1` (runtime loading only),
libloading `0.8.9`, and serde_json `1.0.154`. An unavailable cached transitive
dependency fails without downloading it. The resulting helper `Cargo.lock` is
retained and hashed. To reproduce that dependency resolution in a later run,
pass `-DependencyLockfile <previous-generation>/generator/Cargo.lock`.

The helper dynamically loads the existing **absolute** DLL paths using safe
Windows dependency search flags, calls only their version functions, and
requires `61.3.100 / 61.1.100 / 59.8.100 / 8.1.100`. It never allocates a codec
or reads a media file. The wrapper separately verifies matching header versions.

Binding input uses the original headers with explicit
`--target=x86_64-pc-windows-msvc`, C11/MSVC compatibility, and installed MSVC/SDK
include paths. Defaults select a discovered MSVC installation and Windows SDK;
pass `-MsvcInclude <.../include>` and `-WindowsSdkInclude <.../Include/version>`
to pin another installed version. No toolchain or substitute SDK header is
installed/generated. If libclang cannot parse the real installed headers, the
run fails and preserves its configuration for diagnosis.

The allowlist contains complete `AVCodecContext`, `AVFrame`, `AVBufferRef`,
hardware-device/frame contexts, D3D11VA contexts, format/stream/packet types and
their required inline member declarations. The five referenced D3D11 COM types
are explicitly allowlisted and remain opaque; later
Rust host code should use windows-rs interfaces. No C bridge is compiled and no
FFmpeg import library is linked. The existing handwritten decoder prefixes are
not modified.

Output includes `bindings.rs`, generated Rust layout assertions/tests,
`generation.json`, and `provenance.json`. A separate dependency-free Rust crate
includes the generated bindings and runs the layout checks. Provenance records
the exact Clang arguments/version, target, Rust toolchain and Rustfmt version, helper/lock/wrapper
hashes, original header hashes, actually included SDK-header hashes and DLL
versions/hashes. Generated output uses the installed Rustfmt for readable
declarations and diagnostics. The script restores its process environment afterwards.

Generated provenance contains machine-local build paths. Keep the full original
under the ignored `.downloads` tree. Before copying provenance into source for
publication, run `python scripts/sanitize-provenance.py <public-copy.json>` to
retain public versions, hashes and header identities without profile paths or
unique local run IDs. The sanitizer is covered by
`python -m unittest scripts.test_sanitize_provenance`.

Passing layout checks proves consistency with the parsed headers and Rust
target; it does not prove decoder behavior. Review the artifacts before any
production integration. The next proof should be a bounded standalone D3D11VA
decoder using an owned SDR AV1 color fixture, with GPU conversion and proof-only
readback diagnostics. Playback/audio/GPUI integration remains separate.
