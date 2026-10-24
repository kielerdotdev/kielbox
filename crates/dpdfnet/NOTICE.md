# DPDFNet Rust port

DPDFNet model architecture, pretrained weights and fixed graph originate from CEVA / Daniel Rika, Nino Sapir and Ido Gus, under Apache-2.0. The fixed 48 kHz DPDFNet-2 graph and kernel layouts were ported from the native reference at CEVA commit `6a5dbd3ea5dbea88c30a2be8e7c689b5eb26b533`, as preserved in HushMic commit `d741a1d1151559aca7c1f6308b5ab638ee4a5a1b`. See LICENSE-CEVA.

The Vorbis STFT framing approach and measured four-hop model context delay are adapted from HushMic (copyright 2026 hushmic), under Apache-2.0 at our option. See LICENSE-HUSHMIC. RCR preallocates FFT scratch and implements its own offline streaming, delay compensation and packet mapping.

`generated.rs` is a bounds-checked Rust translation of the fixed generated graph. `kernels.rs` implements the FP32 dense, convolution, GRU and layer-normalization operations in safe Rust. No C source is compiled or linked for this denoiser, and there is no ONNX Runtime/PyTorch/Python inference dependency. RustFFT supplies the Rust FFT implementation.

The bundled weights have SHA-256 `cb7248b8fccbff7b32f254ec2ed0061e604514b2ccd98d997cd081a876424e7b` and 2,582,444 little-endian FP32 values. Loading requires both the exact length and hash. The reference ONNX model used only for development validation has SHA-256 `7f0575a5cec0ba4ffd8f8bd657e06d007e4ccdd955d76faab922b9d3291dc14b`.

Development scripts under `scripts/` can regenerate the fixed Rust graph and independent numerical fixtures. Generated test input audio is synthetic and contains no user recordings. The reference test generator uses NumPy/ONNX Runtime; these are not required for building, running or testing the shipped Rust code and fixtures.

Sources:

- https://github.com/ceva-ip/DPDFNet
- https://github.com/Fovty/hushmic
- https://raw.githubusercontent.com/ceva-ip/DPDFNet/6a5dbd3ea5dbea88c30a2be8e7c689b5eb26b533/native_inference/artifacts/v1/dpdfnet2_48khz_hr/weights.f32
