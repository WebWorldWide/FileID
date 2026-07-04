# FileID — Linux

GTK4 + libadwaita Rust app that shares its scan/ML engine with the Windows port. Phase 0 scaffold: window + HeaderBar + folder picker + engine spawn over stdio.

See [`CLAUDE.md`](./CLAUDE.md) for the full platform conventions, toolkit rationale, and TODO list.

## Build

```bash
sudo apt install build-essential libgtk-4-dev libadwaita-1-dev  # or distro equivalent
./build/build.sh
./dist/fileid/fileid-linux
```

## Status

| Surface | Status |
|---------|--------|
| Engine | Shared with Windows; **builds, runs, and scans on real Linux hardware** — the CPU ONNX Runtime is statically linked (see `shared/docs/DECISIONS.md`), so full-ML `scan --models` (RAM++ tags · CLIP · faces) works on CPU. GPU EP is future work. |
| `fileid` CLI / `fileid-tui` | **Verified on Linux** — scan/search/info/dedupe/restructure + the 5-tab terminal UI. Built via `scripts/build-tools.sh`. |
| GTK app shell | Scaffolded: window, HeaderBar, dark mode, brand CSS, folder picker, engine spawn |
| Library tab | Implemented |
| People / Cleanup / Deep Analyze / Restructure / Settings | Feature-shaped (CI-compiles); GTK runtime not yet verified on hardware |
| Shell ops (trash/reveal/tags/ocr/video) | Implemented (std + libc + subprocess, no new crates); thumbnail/heic/sleep still TODO (see CLAUDE.md table) |
| Flatpak / AppImage distribution | Phase 2 |
