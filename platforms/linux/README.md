# FileID — Linux

GTK4 + libadwaita Rust app that shares its scan/ML engine with the Windows port — feature-complete across the same six tabs (Library · People · Cleanup · Deep Analyze · Restructure · Settings), CI-green on Ubuntu.

See [`CLAUDE.md`](./CLAUDE.md) for the full platform conventions, toolkit rationale, and remaining polish list.

## Build

```bash
sudo apt install build-essential libgtk-4-dev libadwaita-1-dev tesseract-ocr ffmpeg \
                 libheif-examples libheif-plugin-libde265   # HEIC decode plugin
./build/build.sh          # or, from the repo root: ./build.sh -linux
./dist/fileid/fileid-linux
```

## Status

| Surface | Status |
|---------|--------|
| Engine | Shared with Windows; **builds, runs, and scans on real Linux hardware** — the CPU ONNX Runtime is statically linked (see `shared/docs/DECISIONS.md`), so full-ML `scan --models` (RAM++ tags · CLIP · faces) works on CPU. GPU EP is future work. |
| `fileid` CLI / `fileid-tui` | **Verified on Linux** — scan/search/info/dedupe/restructure + the terminal UI. Built via `scripts/build-tools.sh`. |
| GTK app — all six tabs | **Implemented + runtime-verified** (WSLg six-tab walk + earlier on-hardware pass): Library grid/search/preview, People face clusters, Cleanup dupe groups, Deep Analyze, Restructure Sankey + apply/undo, Settings model manager. LavaLamp, gold palette, springs. |
| Shell ops | trash/reveal/tags/ocr/video/heic/**sleep** implemented (std + libc + subprocess, no new crates). HEIC decode needs `libheif-plugin-libde265` at runtime. `thumbnail` has no non-Windows caller (each app thumbnails itself). |
| Flatpak / AppImage / Nix | Recipes in [`packaging/`](../../packaging/); the offline-sandbox Flatpak build is required CI. Graphical launch and AppImage/Nix runtime validation remain native gates. |
