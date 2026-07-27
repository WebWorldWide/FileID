# FileID — Linux

GTK4 + libadwaita Rust app that shares its scan/ML engine with the Windows port and implements the same six tabs (Library · People · Cleanup · Deep Analyze · Restructure · Settings). WSL/WSLg gates are green; native packaging, hardware, ARM64, and hosted-CI validation remain release gates.

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
| Engine | Shared with Windows; WSL builds/clippy/tests and prior native scans cover the CPU path. Portable builds statically link CPU ONNX Runtime (see `shared/docs/DECISIONS.md`); native distro/ARM64 and non-NVIDIA hardware validation remain open. |
| `fileid` CLI / `fileid-tui` | **Verified on Linux** — scan/search/info/dedupe/restructure + the terminal UI. Built via `scripts/build-tools.sh`. |
| GTK app — all six tabs | **Implemented and exercised under WSLg**: Library grid/search/preview, People face clusters, Cleanup dupe groups, Deep Analyze, Restructure Sankey + apply/undo, Settings model manager. Packages do not bundle `llama-mtmd-cli`: Deep Analyze is unavailable in Flatpak and requires a compatible runner on `PATH` in unsandboxed builds. |
| Shell ops | trash/reveal/tags/ocr/video/heic/**sleep** implemented (std + libc + subprocess, no new crates). HEIC decode needs `libheif-plugin-libde265`. Cross-filesystem Restructure/Trash moves fail closed with the source untouched. |
| Flatpak / AppImage / Nix | Recipes in [`packaging/`](../../packaging/); the offline-sandbox Flatpak build is required CI. Graphical launch and AppImage/Nix runtime validation remain native gates. |
