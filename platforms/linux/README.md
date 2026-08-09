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
| Engine | Shared with Windows; `cargo check --target x86_64-unknown-linux-gnu` passes on V15.5 |
| GTK app shell | Scaffolded: window, HeaderBar, dark mode, brand CSS, folder picker, engine spawn |
| Library tab | Pending (Phase 1) |
| People / Cleanup / Deep Analyze / Restructure / Settings | Pending (Phase 1) |
| Shell ops (trash/thumbnail/ocr/video/reveal/tags/sleep) | Stubs return Err; real Linux impls planned (see CLAUDE.md table) |
| Flatpak / AppImage distribution | Phase 2 |
