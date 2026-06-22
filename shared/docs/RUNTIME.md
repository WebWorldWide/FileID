# ONNX Runtime — the engine's inference library

The shared Rust engine (`fileid-engine`) runs ML inference (CLIP image search,
RAM++ tagging, YuNet/SFace faces) on **ONNX Runtime**. It is built with `ort`'s
`load-dynamic` feature, so it does **not** statically link ONNX Runtime — it
`dlopen`s the shared library at run time:

| OS      | Library `ort` loads     | Where it comes from                                     |
|---------|-------------------------|---------------------------------------------------------|
| Windows | `onnxruntime.dll`       | Bundled beside the engine / pinned via accelerator pack |
| Linux   | `libonnxruntime.so`     | System package / `download-binaries`                    |
| macOS   | `libonnxruntime.dylib`  | **Installed once — see below**                          |

This is **separate from the AI model weights** (`fileid models download`). Models
are the `.onnx` weights; the runtime is the library that loads + executes them.
A full-ML scan needs **both**.

License: ONNX Runtime is **MIT-licensed** (microsoft/onnxruntime) — commercial-clean.

---

## Why macOS needs a one-time install

`ort 2.0.0-rc.10` with `download-binaries` fetches a prebuilt ONNX Runtime for
the build target. For `aarch64-apple-darwin` that prebuilt is a **static**
`libonnxruntime.a` (pyke's "none" tarball contains no `.dylib` — verified). A
`load-dynamic` build can't use a static archive at run time, so **there is no
`libonnxruntime.dylib` unless you install one**. Without it, a full-ML scan fails:

```
model_load_failed: libonnxruntime.dylib (no such file)
```

(`ort`'s `copy-dylibs` feature wouldn't help — there's no dylib in the download to
copy. The alternative would be to statically link the `.a`; we keep `load-dynamic`
so the runtime is swappable and matches the Windows/Linux model.)

### Required version

`ort 2.0.0-rc.10` targets **ONNX Runtime 1.22.0** and **hard-panics if the loaded
dylib's minor version is < 22**. Install **1.22.x**. A newer build (e.g. Homebrew
shipping 1.23) loads with a compatibility warning; an older one aborts.

---

## Installing on macOS — pick one

### 1. `fileid runtime install` (recommended)

```sh
fileid runtime install      # idempotent; reports if already present
fileid runtime status       # where it is / isn't + the search path
```

It first reuses any runtime already on the machine (Homebrew, beside the engine —
no network), and otherwise guides you to one of the options below. When the
HuggingFace mirror is configured (see *Pinned source* below) it downloads the
dylib directly through the engine's audited, CA-pinned network path.

### 2. Homebrew

```sh
brew install onnxruntime
```

Installs `/opt/homebrew/lib/libonnxruntime.dylib`, which the engine probes
directly — no further step. (Homebrew tracks the latest ONNX Runtime; ≥ 1.22 is
required, and is what Homebrew ships today.)

### 3. The shell script (official Microsoft release)

```sh
shared/scripts/install_onnxruntime_macos.sh          # installs into the runtime dir
shared/scripts/install_onnxruntime_macos.sh --force  # reinstall
```

Downloads the official, MIT-licensed `onnxruntime-osx-arm64-1.22.0.tgz` from
`github.com/microsoft/onnxruntime/releases`, verifies its SHA256 (when pinned —
see below), extracts `lib/libonnxruntime.dylib`, and installs it into the engine
runtime dir.

---

## Where the dylib must live (engine search order)

The engine (`src/ort_runtime.rs`) resolves the dylib in this order and pins
`ORT_DYLIB_PATH` to the first hit, before the first ML session:

1. `ORT_DYLIB_PATH` (if already set to an existing file) — explicit override.
2. **Beside the running `FileIDEngine` binary** (`current_exe()` dir).
3. **The engine runtime dir:** `<state-root>/runtime/libonnxruntime.dylib`, where
   `<state-root>` = `$XDG_DATA_HOME/FileID` or `~/.local/share/FileID`. This is
   where `fileid runtime install` and the shell script write.
4. `/opt/homebrew/lib/libonnxruntime.dylib` (Homebrew, Apple silicon).
5. `/usr/local/lib/libonnxruntime.dylib` (Intel-prefix Homebrew / manual).

If none resolve, a full-ML scan fails fast with a clear `runtime_not_installed`
error pointing at `fileid runtime install` — distinct from a missing-model error.
(ONNX Runtime placed on the dyld search path — `/usr/lib`, `DYLD_LIBRARY_PATH` —
also loads, but isn't auto-detected by the pre-flight.)

---

## Pinned source (CLI download) — status

The `fileid runtime install` **download** path is wired but its source is not yet
pinned, because the project's egress rule prefers **huggingface.co only**:

- **Preferred:** mirror a bare `libonnxruntime.dylib` (1.22.x, macOS arm64) on
  huggingface.co and set `PINNED_DYLIB_URL` + `PINNED_DYLIB_SHA256` in
  `platforms/cli/src/runtime.rs`. Egress stays HuggingFace-only and reuses the
  engine's existing CA-pinned downloader. **TODO(runtime-dylib).**
- **Override (today):** point the installer anywhere with
  `FILEID_ORT_DYLIB_URL` (+ optional `FILEID_ORT_DYLIB_SHA256`). The host must be
  on the downloader's redirect allow-list (`huggingface.co`, `github.com`, …).
- **No-network fallback (today):** `brew install onnxruntime` or the shell
  script. Both work now; the engine finds the result via the search order above.

To produce + pin the artifact for the HF mirror:

```sh
ORT=1.22.0
curl -fL -o ort.tgz \
  "https://github.com/microsoft/onnxruntime/releases/download/v${ORT}/onnxruntime-osx-arm64-${ORT}.tgz"
shasum -a 256 ort.tgz                     # pin this for the shell script (EXPECTED_SHA256)
tar -xzf ort.tgz
DY=$(find . -name 'libonnxruntime*.dylib' | head -1)
cp -L "$DY" libonnxruntime.dylib
shasum -a 256 libonnxruntime.dylib        # pin this for the CLI (PINNED_DYLIB_SHA256)
# upload libonnxruntime.dylib to the HF mirror; set PINNED_DYLIB_URL to its resolve/ URL
```

---

## Windows / Linux

No action needed:

- **Windows** bundles `onnxruntime.dll` beside the engine and, for GPU vendors,
  pins `ORT_DYLIB_PATH` to a matched accelerator-pack runtime (`main.rs`).
- **Linux** uses the system ONNX Runtime / the `download-binaries` shared object.

`fileid runtime status` reports "provided by the platform" on these OSes, and
`fileid runtime install` is a no-op there.
