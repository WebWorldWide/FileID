# Inference runtime — how the engine runs its models

The shared Rust engine (`fileid-engine`) runs every ONNX model it ships (CLIP image
and text, RAM++, YuNet, SFace, Face Mesh, BGE) on **tract**: a pure-Rust ONNX
runtime (`tract-onnx` 0.23.10, MIT / Apache-2.0) compiled into the engine binary.
There is no runtime library to install, locate, pin, sign or bundle, on any platform.

| OS | Inference | Library to ship |
|---------|--------------------------------------------------|-----------------|
| macOS | tract, CPU (the app's own MLX adapters handle scan CLIP and RAM++) | none |
| Linux | tract, CPU | none |
| Windows | tract, CPU; an `ort-accel` build also binds ONNX Runtime when a GPU/NPU provider is usable | `onnxruntime.dll` beside the engine (ort-accel builds only) |

The **AI model weights** are separate (`fileid models download`, Settings → Local AI).
tract loads and runs those `.onnx` files directly.

## The seam

`platforms/windows/src/engine/src/models/inference.rs` is the one inference seam:
named tensors in, tensors out, loaded from a path. Every wrapper (`sface.rs`,
`yunet.rs`, `mobileclip.rs`, `clip_text.rs`, `bge_text.rs`, `ram_plus.rs`,
`face_mesh.rs`) loads through `runtime::commit_chain_session`.

- A compiled model is shared by every session loaded from the same file with the
  same input contract; each session owns only its run state. A worker pool costs one
  copy of the weights.
- A wrapper states its input contract (element type, fixed or symbolic dimensions)
  because tract compiles against it: a fixed batch of 1 for the face models, a
  symbolic batch for CLIP and RAM++, a symbolic sequence length for BGE.
- Preprocessing and decoding stay in the wrappers; only the session changed.

`models::runtime::inference_backend()` names the backend for logs and status
(`tract-cpu`, or the accelerator's name in an `ort-accel` build). The IPC
`executionProvider` field keeps the enum names (`cpu`, `directml`, …) the apps
already read.

## Windows GPU/NPU: the `ort-accel` feature

tract has no DirectML, OpenVINO or QNN backend, so the Windows release builds enable
`--features ort-accel`: ONNX Runtime (`ort 2.0.0-rc.10`, `load-dynamic`) binds the
vendor execution provider (CUDA, TensorRT, OpenVINO, DirectML, QNN) when the bind
chain starts with one, and every other case — CPU-only boxes, an ONNX Runtime that
fails to start — runs on tract. The Windows build scripts and workflows pass the
feature; a plain `cargo build` on Windows is tract-only.

Microsoft's packaged Windows builds (DirectML NuGet, GitHub zips) are made with
`--use_telemetry` and register ONNX Runtime with Windows' diagnostic-data service.
FileID ships 1.22.0 built from source without it:
`platforms/windows/build/build-ort-runtime.ps1` (CI: `.github/workflows/ort-runtime.yml`)
writes `build/runtime-cache/onnxruntime-1.22.0-dml-notelemetry/<arch>/`, which
`fetch-runtime-deps.ps1` stages. `check_binary_privacy.py --ort-runtime` verifies the
result. None of this applies to macOS or Linux any more.

## Parity guard

`shared/test-corpus/model-golden/*.json` holds golden outputs of the seven models for
fixed synthetic inputs (generated from the ONNX Runtime 1.22.0 build before it was
removed): output shape, L2 norm, the first 16 values and 16 values spread across
each output. The engine tests in `models/*/golden_tests` compare against them when
`FILEID_TEST_MODELS_DIR` points at a models folder in the engine layout
(`sface/`, `yunet/`, `mobileclip/`, `clip_text/`, `bge_text/`, `face_mesh/`,
`ram_plus/`); without it they print a skip line and pass.
`FILEID_GOLDEN_WRITE=1` rewrites the files from the current runtime — do that only
for a deliberate model or runtime change, and review the diff.

```sh
FILEID_TEST_MODELS_DIR=<models dir> cargo test --lib golden_tests -- --nocapture
```
