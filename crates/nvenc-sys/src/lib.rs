//! Pinned NVIDIA SDK 12.1 ABI bindings, dynamically loaded by the safe backend.
//! Derived from nvidia-video-codec-sdk 0.4.0; see LICENSE and NOTICE.md.
#![allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    dead_code,
    clippy::all
)]
#![allow(unpredictable_function_pointer_comparisons, unnecessary_transmutes)]
#[cfg(windows)]
mod guid;
#[cfg(windows)]
mod nvEncodeAPI;
#[cfg(windows)]
mod version;
#[cfg(windows)]
pub use nvEncodeAPI::*;
