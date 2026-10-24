# Binding provenance

The Windows NVENC 12.1 bindings, API version constants and GUID definitions are derived from `nvidia-video-codec-sdk` 0.4.0, copyright 2023 Viliam Vadocz, distributed under MIT. See the retained LICENSE.

Local changes replace generated Rust C enum representations with `#[repr(transparent)]` integer newtypes and associated constants, preserving the C ABI and generated structure-layout tests. This avoids invalid Rust enum discriminants in zero-initialized native structures and preserves unknown native values.

The library is loaded from the installed NVIDIA Windows driver by the recorder. No driver binary is included.

See also the workspace THIRD_PARTY.md.
