# DPDFNet pretrained weights

`dpdfnet2_48khz_hr.weights.f32` is CEVA's Apache-2.0 DPDFNet-2 48 kHz pretrained model, embedded in the executable for offline, optional microphone suppression. It is data, not executable native code.

- Size: 10,329,776 bytes (2,582,444 little-endian FP32 values).
- SHA-256: `cb7248b8fccbff7b32f254ec2ed0061e604514b2ccd98d997cd081a876424e7b`.
- Source: https://raw.githubusercontent.com/ceva-ip/DPDFNet/6a5dbd3ea5dbea88c30a2be8e7c689b5eb26b533/native_inference/artifacts/v1/dpdfnet2_48khz_hr/weights.f32
- License/provenance: `crates/dpdfnet/LICENSE-CEVA` and `crates/dpdfnet/NOTICE.md`.

No model download or inference service is used at runtime. The model is instantiated only on a replay-save/export worker when suppression is requested.
