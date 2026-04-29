# FileID — Orientation for Claude

You're working on **FileID**, a macOS 14+ SwiftUI app that uses Apple's Neural Engine (Vision framework + SwiftData) to tag, rename, and organize images, videos, and documents. The user is the sole developer, on an M1 MacBook Pro 16GB, testing against a 50,000-file directory.

## Read these before doing anything else

1. **`docs/STATE.md`** — current snapshot: what's working, what's broken, last toolchain version. Updated at the end of every session.
2. **`docs/NEXT.md`** — top 3 priorities in order, each with explicit acceptance criteria. This is the work queue.
3. **`docs/DECISIONS.md`** — append-only log of *why* the code looks the way it does. Read it before "fixing" something that looks weird.
4. **`~/.claude/plans/okay-so-i-am-toasty-clarke.md`** — the seven-phase overhaul plan. Phase 0 is complete. Phase 1 is next.

## Build & run

```bash
./run.sh
```

That's it. `run.sh` builds via `swift build` (forcing `DEVELOPER_DIR` at Xcode because SwiftData macros only ship with Xcode, not Command Line Tools), assembles a `.app` bundle, and launches it. **Always re-run after changes** — there's no test suite to lean on yet, so the only validation is launching and using the UI.

For a quick syntax check without launching:

```bash
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift build
```

## Architecture in 10 lines

- **`Sources/FileIDApp.swift`** — `@main` entry point. Wires `MainWindowView` into a `WindowGroup` with the `.modelContainer` for SwiftData.
- **`Sources/MainWindowView.swift`** — root shell. NavigationSplitView with sidebar tabs (Library / Cleanup / Restructure / People / Review / Settings) and a bottom status bar.
- **`Sources/AppViewModel.swift`** — `@MainActor` orchestrator. Holds `isProcessing`, `processedCount`, `totalCount`, the active folder URL, etc. UI binds here.
- **`Sources/Services/MediaProcessor.swift`** — the scan engine (actor). `FileStream` (lazy enumerator) → `withTaskGroup` → per-file `processFile` → result coalesces into SwiftData via batched saves.
- **`Sources/Services/VisionProcessor.swift`** — wraps Vision: classify, animals, OCR, face rectangles + feature prints, scene prints, saliency.
- **`Sources/Services/FaceClusteringService.swift`** — assigns each detected face to a `PersonRecord`. Currently broken (compares only first 8 of 512 dimensions); slated for Phase 2 rewrite.
- **`Sources/Models/FileRecord.swift`** + **`PersonRecord.swift`** — SwiftData `@Model` classes.
- **`Sources/LavaLampAesthetics.swift`** — animated gold/orange `Canvas` background with `.ultraThinMaterial` blur. **The user loves this. Preserve it.**

## Working principles

- **The user runs the build.** Don't claim a UI change works until you've seen it land in the app yourself, or until the user has. Type-checks aren't proof of correctness.
- **Update `docs/STATE.md` and `docs/NEXT.md` at the end of every session.** This is the working memory for the next Claude session — the user explicitly asked for it because session-over-session context loss was painful.
- **Append to `docs/DECISIONS.md` for any non-obvious call.** Particularly: choosing one architecture over another, working around an Apple API quirk, deliberately leaving something untouched.
- **Preserve `LavaLampAesthetics.swift`.** It's the user's favorite part of the UI.
- **No third-party Swift packages without asking.** SwiftData, Vision, AVFoundation, NaturalLanguage, Compression are all system frameworks — prefer them.
- **The seven-phase plan is the source of truth for scope.** Don't expand a phase mid-flight. If something new comes up, add it to `docs/NEXT.md` for later.

## Conventions

- Swift 6 language mode. Strict concurrency. Sendable closure-capture warnings will become errors — fix them as you touch the surrounding code.
- `@MainActor` for UI state, `actor` for shared mutable services, `nonisolated` for stateless helpers.
- Errors are mostly `try?` swallow today — that's a Phase 1+ cleanup target, not a current refactor.
- File-naming: `Service.swift` for actors / static helpers, `View.swift` for SwiftUI views, `Model.swift` for SwiftData entities.
