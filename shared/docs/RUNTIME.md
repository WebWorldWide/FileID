# Inference runtime — how the engine runs its models

The shared Rust engine (`fileid-engine`) runs every ONNX model it ships (CLIP image
and text, RAM++, YuNet, SFace, Face Mesh, BGE) on **tract**: a pure-Rust ONNX
runtime (`tract-onnx` 0.23.10, MIT / Apache-2.0) compiled into the engine binary.
There is no runtime library to install, locate, pin, sign or bundle, and it is the
same runtime on Windows, macOS and Linux. No ONNX Runtime ships in the engine, the
CLI, the TUI, the GTK app or any Windows package.

| OS | Inference | Library to ship |
|---------|--------------------------------------------------|-----------------|
| macOS | tract, CPU (the app's own MLX adapters handle scan CLIP and RAM++) | none |
| Linux | tract, CPU | none |
| Windows | tract, CPU | none (`pdfium.dll` is for PDF previews, not inference) |

The **AI model weights** are separate (`fileid models download`, Settings → Local AI).
tract loads and runs those `.onnx` files directly.

## The seam

`platforms/windows/src/engine/src/models/inference.rs` is the one inference seam:
named tensors in, tensors out, loaded from a path. Every wrapper (`sface.rs`,
`yunet.rs`, `mobileclip.rs`, `clip_text.rs`, `bge_text.rs`, `ram_plus.rs`,
`face_mesh.rs`) loads through `inference::load_session`.

- A compiled model is shared by every session loaded from the same file with the
  same input contract; each session owns only its run state. A worker pool costs one
  copy of the weights.
- A wrapper states its input contract (element type, fixed or symbolic dimensions)
  because tract compiles against it: a fixed batch of 1 for the face models, a
  symbolic batch for CLIP and RAM++, a symbolic sequence length for BGE.
- Preprocessing and decoding stay in the wrappers; only the session changed.
- A graph with f16 values (RAM++) runs in f32; tract's f16 CPU kernels moved tags
  near their thresholds.

`models::runtime::inference_backend()` names the backend for logs and status: always
`tract-cpu`. The IPC `executionProvider` field carries the same string.

## Threads

tract's matrix multiply runs on a private pool sized by `inference::matmul_threads()`:
the performance cores (`hw.perflevel0.physicalcpu` on Apple silicon, the efficiency-class
split on hybrid Windows parts, physical cores elsewhere), or `FILEID_TRACT_THREADS`
when set (`1` is single-threaded). It is set once before the first model compiles,
so every entry point (engine, CLI, GTK app, tests) behaves the same. The pool is
the `multithread-mm` feature of `tract-linalg`, the same pinned version tract-onnx
already uses. `FILEID_MODEL_POOL_SIZE` sets how many sessions each pooled model
gets (workers wait on a session's lock, so a pool of 1 serializes RAM++).
Measurements and the chosen defaults are in DECISIONS (2026-11-18, tract everywhere).

## GPU

There is no GPU inference backend, on any platform. A GPU backend (tract-metal,
tract-cuda or wgpu) is a later, measured, cross-platform decision: adding one on a
single platform would make the platforms differ. The llama.cpp runtimes (Describe
Files, Chat) are separate and still use Vulkan or CUDA when present.

## Parity guard

`shared/test-corpus/model-golden/*.json` holds golden outputs of the seven models for
fixed synthetic inputs, generated with ONNX Runtime 1.22.0 (CPU) before it was removed
(each file's `generator` says so): output shape, L2 norm, the first 16 values and 16
values spread across each output. The engine tests in `models/*/golden_tests` compare
against them when `FILEID_TEST_MODELS_DIR` points at a models folder in the engine
layout (`sface/`, `yunet/`, `mobileclip/`, `clip_text/`, `bge_text/`, `face_mesh/`,
`ram_plus/`); without it they print a skip line and pass, so a release must run them
with the models present:

```sh
FILEID_TEST_MODELS_DIR=<models dir> cargo test --lib golden_tests -- --nocapture
```

`FILEID_GOLDEN_WRITE=1` creates a golden that does not exist yet. Overwriting one also
needs `FILEID_GOLDEN_REBASE=1`, and the file then records that tract rebased it:
a rebased golden no longer checks tract against an independent runtime. Do that only
for a deliberate model or runtime change, and review the diff.
