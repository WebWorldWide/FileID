# TEST.md — cross-OS end-to-end test runbook

How to verify every FileID surface (desktop **app**, **TUI**, **CLI**) on every OS works end to end. Written as a runbook for an agent or a developer to follow. Pair it with `ARCHITECTURE.md` (what the pieces are) and `SHIP.md` (release gate).

Surfaces × OS:

| Surface | macOS | Windows | Linux |
|---|---|---|---|
| Desktop app | Swift/SwiftUI + MLX/CoreML | WinUI 3 / .NET 8 | GTK4 + libadwaita |
| CLI (`fileid`) | ✓ (engine, ONNX) | ✓ | ✓ |
| TUI (`fileid-tui`) | ✓ (engine, ONNX) | ✓ | ✓ |
| Engine (`FileIDEngine`) | ONNX Runtime¹ | ONNX Runtime (DirectML/CUDA) | ONNX Runtime |

¹ macOS engine needs `libonnxruntime.dylib` provisioned (`fileid runtime install`); the **desktop app** does NOT — it uses MLX/CoreML. See [Per-OS prerequisites](#per-os-prerequisites).

---

## 0. Safety rules — READ FIRST (never violate)

These caused real incidents this project. Every test run obeys them:

1. **Never touch the user's real library.** macOS real DB: `~/Library/Application Support/FileID/fileid.sqlite`. Engine real DB (CLI/TUI default): `~/.local/share/FileID/FileID/fileid.sqlite` (XDG). Before and after any test that *could* write, capture the real DB's md5 and assert it's unchanged:
   ```bash
   md5 ~/Library/Application\ Support/FileID/fileid.sqlite   # before
   # ...run test against an ISOLATED db...
   md5 ~/Library/Application\ Support/FileID/fileid.sqlite   # after — MUST match
   ```
2. **Isolate every test.** Use a throwaway DB / data home / models dir — never the defaults:
   - CLI: `--db /tmp/test.sqlite` (or `$FILEID_DB`).
   - macOS app-support resolution: `CFFIXED_USER_HOME=/tmp/fid_home` (macOS resolves app-support via `getpwuid`, so `$HOME` is **not** enough — you must set `CFFIXED_USER_HOME`).
   - Engine scratch: `XDG_DATA_HOME=/tmp/fid_scratch` (macOS/Linux) or `LOCALAPPDATA=...` (Windows).
   - Models: `FILEID_MODELS_DIR=/tmp/fid_models` to test missing/partial model states without disturbing the real set.
3. **Never run destructive ops on real paths:** no `applyRestructure`, no `fileid restructure --apply`, no `dedupe --apply` against real corpus paths. Use a copied test corpus.
4. **Never run `platforms/apple/.../run.sh` against the real library** — it wipes the DB + UserDefaults. It's for a scratch run only.
5. **No telemetry, ever** is a release gate — see [§7](#7-cross-cutting-checks). 

---

## 1. Test fixtures

A tiny, safe corpus (fast full scan, deterministic):
```bash
rm -rf /tmp/fid_corpus && mkdir -p /tmp/fid_corpus
printf 'Invoice 2023 total due 500 dollars' > /tmp/fid_corpus/invoice.txt
printf '# Notes\nvacation beach sunset' > /tmp/fid_corpus/notes.md
printf 'name,amount\nalice,10' > /tmp/fid_corpus/data.csv
# a couple of tiny valid PNGs so there are "images" to tag
printf '\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89\x00\x00\x00\rIDATx\x9cc\xf8\xcf\xc0\x00\x00\x00\x03\x00\x01\xff\xff\xff\xff\x00\x00\x00\x00IEND\xaeB`\x82' > /tmp/fid_corpus/pic1.png
cp /tmp/fid_corpus/pic1.png /tmp/fid_corpus/pic2.png
```
For real performance/accuracy runs use the `G:\TrueNAS` corpus (Windows dev box) or the external Adlon drive copy — never the originals; copy first.

---

## 2. Build matrix

```bash
# Engine (shared by CLI/TUI/Win/Linux) — its own crate
cd platforms/windows/src/engine && cargo build --release

# CLI — its own workspace
cd platforms/cli && cargo build --release      # -> target/release/fileid

# TUI — its own workspace
cd platforms/tui && cargo build --release      # -> target/release/fileid-tui

# Linux app (GTK4) — needs: sudo apt install libgtk-4-dev libadwaita-1-dev
cd platforms/linux && cargo build --release    # -> target/release/fileid-linux

# Windows app (.NET 8 / WinUI 3) — on Windows
cd platforms/windows && dotnet build           # + dotnet test, dotnet format --verify-no-changes

# macOS app (Swift) — on macOS
cd platforms/apple && swift build && swift test   # or open the Xcode project
```
Install CLI/TUI for interactive testing (macOS: fresh inode + ad-hoc sign avoids "Killed: 9"):
```bash
for b in fileid fileid-tui; do rm -f ~/.cargo/bin/$b; cp <built>/$b ~/.cargo/bin/$b; codesign --force --sign - ~/.cargo/bin/$b; done
```

Gate for any Rust change: `cargo clippy --all-targets -- -D warnings` clean + `cargo test` green, in the crate(s) touched. `cargo check` passing is NOT proof — the GUI/GPU/dlopen paths need real hardware.

---

## 3. CLI — end-to-end (all OS)

Run against an isolated `--db`. Expected results in **bold**.

```bash
FID=~/.cargo/bin/fileid; DB=/tmp/fid_cli.sqlite; rm -f $DB
$FID --version                       # -> fileid 0.1.1
$FID                                 # -> first-run tour (what it is + Get-started commands)
$FID --help                          # -> all subcommands, each with an Example

# Models
$FID models list                     # -> 9 models, ★ = the 2 scan-gate (mobileclip_s2, arcface), installed/missing, total size, models dir
$FID models download --all --dry-run # -> "Would download: 24.9 GB across 9 model(s)", downloads NOTHING

# Metadata scan (NO models needed — works on every OS incl. macOS)
$FID scan /tmp/fid_corpus --db $DB   # -> live progress, "Scan complete … Indexed: 5 · Text-indexed: 3 · NNNN files/s"
$FID search "invoice" --db $DB       # -> matches invoice.txt
$FID search vacation beach --db $DB  # -> matches notes.md
$FID info /tmp/fid_corpus/notes.md --db $DB   # -> metadata, snippet, (tags/people once AI-scanned)
$FID dedupe --exact --db $DB         # -> reports pic1.png/pic2.png as a duplicate group (read-only; NO --apply)

# Full-AI scan (needs models + runtime; see prereqs)
$FID models download arcface mobileclip_s2 --yes        # ~370 MB, real progress bar
$FID scan /tmp/fid_corpus --models --db $DB             # -> full pipeline; tags on the PNGs, completes
$FID people --db $DB                                     # -> face clusters (empty for this corpus; real on a photo corpus)
$FID search --similar 1 --db $DB                         # -> visually-similar to file id 1
```
Pass = every command returns promptly, progress is visible during scan, and a clear summary prints. Missing models/runtime ⇒ a **clear, actionable** message (not a hang, not a cryptic error). `--json` produces clean machine output on stdout.

---

## 4. TUI — end-to-end (all OS): drive it in a real PTY

The TUI is a full-screen alt-screen app; you can't eyeball it from a normal tool shell. Drive it through a real pseudo-terminal and capture rendered frames. This is the method that found the real bugs this session.

`/tmp/drive_tui.py` (set window size or the TUI gets 0×0; feed keys; snapshot the **whole** 40 rows — the status line is at the bottom):
```python
import pty, os, time, select, struct, fcntl, termios, pyte
ROWS, COLS = 40, 120
screen = pyte.Screen(COLS, ROWS); stream = pyte.ByteStream(screen)
pid, fd = pty.fork()
if pid == 0:
    os.chdir("/tmp/fid_corpus"); os.environ["TERM"]="xterm-256color"
    os.execvp(os.path.expanduser("~/.cargo/bin/fileid-tui"), ["fileid-tui"])
else:
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    def drain(t=1.0):
        end=time.time()+t
        while time.time()<end:
            r,_,_=select.select([fd],[],[],0.15)
            if r:
                try: b=os.read(fd,65536)
                except OSError: return False
                if not b: return False
                stream.feed(b)
        return True
    def snap(label):
        print("\n==== %s ====" % label)
        for l in screen.display:               # ALL rows, incl. bottom status line
            if l.strip(): print(l.rstrip())
    time.sleep(1.6); drain(); snap("INITIAL")
    for k,lbl in [(b'\t',"TAB"),(b'5',"Settings"),(b'1',"Library"),(b's',"browser"),(b's',"SCAN")]:
        os.write(fd,k); drain(1.5); snap(lbl)
    for i in range(8): drain(1.0); snap("watch+%d"%(i+1))   # watch for progress / lockup
    os.write(fd,b'q'); os.write(fd,b'q')
```
Run + grep (a hook may compress large reads — grep small):
```bash
python3 /tmp/drive_tui.py > /tmp/tui.txt 2>&1
grep -aoE "Scan phase: [A-Za-z]+|AI models not installed[^|]*|Scanning \[[A-Za-z]+\][^|]*|indexed|No files yet|press D[^|]*" /tmp/tui.txt | sort -u
```
End-to-end checklist (each must hold):
- **Tab / 1–5** switch tabs and the **body actually changes** (Settings reachable, not stuck on the welcome screen).
- Every tab has a real state (Library "press s to scan", People, Cleanup, Restructure, Settings model status), not a blank.
- **`s`** opens the folder browser (arrows/Enter navigate, `.` toggles hidden, `d` jumps to drives); **`s`** again scans.
- **Scan with models present:** progress streams ("Scanning [Discovering] x/y"), then a "files indexed" summary — **no lockup**, and the TUI stays responsive (Tab works during/after).
- **Scan with models/runtime missing:** a **persistent** "AI models not installed — press D…" (macOS: runtime message) — never a bare "Scan phase: Failed" that reverts to blank.
- **`D`** shows the gold install **gauge** filling 0→100 (drives off the CLI's `--porcelain-progress`).
- Launch is clean on a real terminal (macOS: `osascript -e 'tell application "Terminal" to do script "~/.cargo/bin/fileid-tui"'` — do NOT use `exec`, it closes the window on exit; kill stray `drive_tui.py`/`fileid-tui` first).

Note: in a headless CI sandbox the alt-screen teardown can wipe the captured frame — rely on the crate's `ratatui` TestBackend full-frame tests + unit tests as the gate there; PTY-drive on a real box.

---

## 5. Engine — direct diagnosis (all OS)

When a CLI/TUI scan misbehaves, talk to the engine directly — its stdout is the newline-JSON IPC event stream, its stderr the real logs:
```bash
ENGINE=$(find platforms/windows/src/engine/target -name FileIDEngine -type f | head -1)
export XDG_DATA_HOME=/tmp/fid_scratch; mkdir -p $XDG_DATA_HOME/FileID
printf '%s\n' '{"id":"t","payload":{"startScan":{"rootPath":"/tmp/fid_corpus","rootDisplay":null,"rescan":false}}}' \
  | "$ENGINE" 2>/tmp/engine_err.txt | head -20      # events on stdout
tail -30 /tmp/engine_err.txt                          # the REAL error/logs
```
Expect a `ready` event (with hardware/EP detection), `queueState`, `phaseChanged`, `progress`, then `scanComplete` — or a structured `error{kind,message}` (e.g. `models_not_installed`, `model_load_failed`). The engine is authoritative; if the engine reports a clear error but the front-end doesn't show it, the bug is in the front-end's event handling.

---

## 6. Desktop apps — on-hardware E2E

These need the real OS + GPU/NPU; they cannot be verified headlessly. For each, walk all six tabs: **Library · People · Cleanup · Deep Analyze · Restructure · Settings**.

### macOS (Swift / MLX / CoreML) — `platforms/apple`
- Build/run from Xcode (or `swift build`); for a clean scratch run use the scratch `run.sh` (NEVER against the real library).
- E2E: pick a copied photo folder → scan → watch live progress (target throughput on the ref box; RAM++ Swin-L bounds it to ~1 file/s, that's expected) → tags appear (verify real RAM++ tags, not the 4585-count fallback) → People clusters faces → preview opens, **arrow-keys + click navigate between photos** → Cleanup finds dupes → Restructure previews a plan and **only moves on explicit apply** → Settings toggles persist.
- This app uses MLX/CoreML, so it needs **no** ONNX runtime and no `fileid models download`.

### Windows (WinUI 3 / .NET 8) — `platforms/windows`
- `dotnet build`; run the packaged app. Engine auto-detects EP (DirectML on the RTX 2060 dev box; CPU fallback otherwise).
- E2E: same six-tab walk. Verify the Win2D `LavaLampBackground` port renders, springs feel right, gold palette. Run `iterate.ps1` + `scan_assertions.py` against `G:\TrueNAS` for accuracy/throughput (≥140 files/s target on GPU).
- Pre-push gate: `dotnet build` + `dotnet test` + `dotnet format --verify-no-changes`.

### Linux (GTK4 + libadwaita) — `platforms/linux`
- Install system deps, `cargo build --release`, run `fileid-linux`. Verify on Debian/Ubuntu/Arch/Gentoo/NixOS or via the Flatpak/AppImage.
- E2E: six-tab walk; LavaLamp (Cairo) renders, dark theme follows system, gold palette via CSS. Confirm the shell backends work: Trash (freedesktop), reveal (FileManager1/xdg-open), tags (xattr), OCR (tesseract), video thumbs (ffmpeg).

Parity check: the app under test must match the **macOS reference** — same palette (gold `#FFCC00`, lavender `#B19BCE`, cyan `#A0E2EA`, pink `#F2A6C0`), same spring feel (response 0.35–0.4 / damping 0.78–0.8), same six tabs and behaviors.

---

## 7. Cross-cutting checks

- **No telemetry (release gate).** Scan every shipped binary for telemetry/analytics/update-ping strings; the only allowed network egress is user-initiated model/runtime downloads from huggingface.co (and the documented ONNX-runtime source). CI does this; locally:
  ```bash
  strings <binary> | grep -iE "analytics|telemetry|sentry|crashlytics|mixpanel|amplitude|segment|posthog" && echo "FAIL" || echo "clean"
  ```
- **DB byte-faithfulness across engines.** A DB written by the Swift engine and one by the Rust engine for the same corpus must be schema- and content-compatible (open one with the other's reader). Migrations apply cleanly v1→vN.
- **IPC contract.** Anything new must exist in `shared/ipc-schema/ipc.schema.json` first; per-platform DTOs mirror it. Schema drift = build break.
- **Performance.** Match/beat the macOS pipeline on comparable hardware; use GPU/NPU when present, degrade to CPU. Measure files/s from the scan summary; don't guess thresholds — tune against the real corpus.
- **Cancellation / resilience.** Cancel mid-scan → engine stops cleanly, no zombie. Kill the engine mid-scan → front-end shows a clear error (not a hang), respawns where applicable.

---

## 8. Per-OS prerequisites

| Need | macOS | Windows | Linux |
|---|---|---|---|
| AI models (mobileclip_s2, arcface, …) | `fileid models download …` → `~/.local/share/FileID/Models` | `%LOCALAPPDATA%\FileID\Models` | `~/.local/share/FileID/Models` |
| ONNX Runtime (engine/CLI/TUI only) | **`fileid runtime install`** (provisions `libonnxruntime.dylib`)² | bundled EP pack (DirectML/CUDA) | system/bundled `libonnxruntime.so` |
| System libs | — | — | `libgtk-4-dev libadwaita-1-dev`; `tesseract`, `ffmpeg` for OCR/video |

² macOS engine ONNX provisioning — see `RUNTIME.md`. The macOS **desktop app** doesn't need this (MLX/CoreML). Until the runtime is installed, the Mac CLI/TUI do **metadata-only** scans (which work) and full-AI scans report a clear "run `fileid runtime install`" message.

---

## 9. Acceptance — "works end to end"

A surface passes when, on its OS, against an isolated DB/corpus:
1. It builds + (Rust) clippy-clean + tests green.
2. Launch is clean (no crash/Killed:9/lockup).
3. A **metadata** scan completes with visible progress + a summary.
4. With models (+ runtime) present, a **full-AI** scan completes; tags/faces/visual-search populate.
5. Missing models/runtime ⇒ a clear, actionable message — never a silent "nothing happened."
6. Search, info, dedupe (read-only), and restructure (preview) work; destructive ops only on explicit apply.
7. The real user library md5 is unchanged by the whole run.
8. Telemetry scan is clean.

Record results in `STATE.md` (newest on top); file regressions in `NEXT.md`.
