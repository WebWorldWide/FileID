# FileID isolated benchmark (Windows and Linux)

Python 3 standard library only. Run from the repository root. This is a FileID IPC scan benchmark, **not** a generic filesystem throughput test or a model recommender.

```powershell
python -B tools/fileid-benchmark/bench.py --sample-drive 'H:\' --max-entries 1000 --max-dirs 25
python -B tools/fileid-benchmark/bench.py --engine platforms/windows/src/engine/target/debug/FileIDEngine.exe --files 8 --timeout 300
python -B -m unittest discover -s tools/fileid-benchmark -p test_bench.py -v
```

On Linux, pass a built native FileIDEngine executable and a **verified mounted filesystem root** for optional read-only sampling:

```sh
python3 -B tools/fileid-benchmark/bench.py --sample-drive /path/to/mounted-volume-root --max-entries 1000 --max-dirs 25
python3 -B tools/fileid-benchmark/bench.py --engine /path/to/FileIDEngine --files 8 --timeout 300
python3 -B -m unittest discover -s tools/fileid-benchmark -p test_bench.py -v
```

`--sample-drive` is optional and accepts **only an attached volume root**. Confirm the label with `Get-Volume | Select-Object DriveLetter,FileSystemLabel,DriveType,FileSystem` rather than assuming a letter. The sampler calls `scandir` and non-following `stat` only, never opens file content, never recurses through junctions/symlinks, never sends that path to FileID, and prints **volume identity plus aggregate counts and elapsed time only**. `--max-entries` and `--max-dirs` bound work; counts are a **sample**, not whole-drive totals or engine/AI throughput. `bounded: true` means the limit was reached. It creates nothing on that volume, including its root.

Linux identifies mounts from `/proc/self/mountinfo` and checks mounted device labels via `/dev/disk/by-label` when available. An unlabelled external mount (including WSL `9p`/drvfs) is **refused for writable temp/model/library paths**; symlinks are resolved before checking mount identity. `--sample-drive` also forbids the same mounted filesystem for temporary outputs. The mount root is not inferred from names such as `/mnt/h`, and a Linux sample with no available label reports `volume_label: null` rather than guessing.

For an actual engine run, compile `cargo build --bin FileIDEngine` from `platforms/windows/src/engine` or pass an existing engine with `--engine`. The harness creates a unique temporary library and state directory on a non-Adlon volume, redirects `LOCALAPPDATA`, sets the FileID model directory, and sends `startScan` **only for that temporary library**. Temporary files, SQLite, logs, face crops, and thumbnails remain outside Adlon; the harness cleans up only its own temporary directory. There is no scan of the external drive, no model download command, and no real library DB access. The engine path, models path, and temp volume are checked against the sampled volume and the Adlon volume label. If the default model directory is absent, the engine gets an empty **temporary** models directory and reports `model_unavailable`; you can explicitly pass `--models C:\path\to\installed\Models` on another volume. Never pass models on Adlon. Engine output goes to stdout JSON; don't redirect it onto Adlon.

The models tree is checked for junctions and symlinks before engine launch; redirected model paths are refused rather than risking output on an external drive.

On Linux, engine state is redirected through `XDG_DATA_HOME` to the isolated temporary directory; on Windows, through `LOCALAPPDATA`. Timeout/engine errors return JSON with `mode: engine_failed`. Throughput requires both a `completed` phase and a successful `scanComplete` event.

`engine_scan` includes actual `scanComplete` IPC counts, engine `totalSeconds`/files per second, and independent `startScan`-to-completion wall time (including model load). `environment` records engine version, startup-advertised hardware/EP and SHA256/size of present scan weights (RAM++, CLIP ViT-B/32, YuNet, SFace). SHA256s identify files, not a benchmark-side pin certification; use the normal FileID pinned installer. Compare repeated runs with the **same** file count, fixture version, engine, EP, hardware, installed model hashes, and warm/cold cache conditions. Scan performance needs installed, verified models. A `model_unavailable` result has no invented throughput or accuracy; no model ranking/recommendation is implied. Startup-advertised EP is not proof every stage actually ran on it.

The generated library has `--files` tiny, deterministic image/text files plus a copy of the shared OCR fixture. Quality checks require every generated file to be catalogued without failure, expected FileID kind (`image`/`doc`), extracted text from each document, and both shared-corpus OCR tokens (`OCR`, `12345`). OCR token recall uses the real corpus labels, but there are **no labeled OCR negatives**, no rights-cleared face pairs, and no ground-truth semantic tag labels: precision and AI quality are not estimated. The shared collisions assertion requires a *mutating restructure/apply* run; this read-only/isolated scan deliberately does not claim to exercise it. Kind checks here are synthetic extension-derived regression checks, **not AI classification accuracy**. Treat OCR fixture results as engine quality evidence only when models are loaded and the engine scan completes.

Exit codes: `0` sampling only or passing engine scan; `1` scan/quality failure; `2` missing engine, invalid/unavailable volume or configuration; `3` missing/unloadable models (no benchmark result). No external dependencies, network calls, or telemetry are added.
