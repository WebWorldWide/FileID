# FileID — Security Notes

What we've audited, what we've fixed, and the hardening that must land before the v1.0 release.

## Threat model

FileID runs entirely on-device. The only network surface is HuggingFace model downloads from `CLIPModelInstaller`. Three attacker classes are in scope:

- **Local untrusted process.** Can write to the user's filesystem (limited by the OS sandbox + permissions). Goal: read sensitive files, escalate via the engine's IPC.
- **Network MITM.** Intercepting HuggingFace downloads to swap a model for a malicious bundle.
- **Malicious file content.** A crafted image / PDF / zip that the engine processes — VLM, OCR, dHash, etc. — that exploits a parser.

## Findings

### Fixed

- **Engine binary integrity (CRITICAL).** `EngineClient.start()` now refuses to spawn the engine unless two checks pass:
  1. The engine path resolves inside the running app bundle's `Contents/MacOS/` (symlinks that escape are rejected). This is the strong guarantee — an attacker who can write inside the bundle could swap the app itself anyway.
  2. The engine's signing identity matches the app's. Compared via `kSecCodeInfoTeamIdentifier` (`SecCodeCopySigningInformation`): both must share the same Team ID for Developer ID builds, or both unsigned / ad-hoc for dev (`bash run.sh`). A mismatch — engine signed by a different developer — fails with a clear message. Earlier prototypes used the app's full designated requirement, which encodes the app's cdhash and rejected legitimately-different binaries (status -67050); the Team ID match is what realistically catches a swapped binary.
- **Symlink TOCTOU (CRITICAL).** Removed the `fm.fileExists(atPath:)` pre-check before `createSymbolicLink`. The create call now serves as the atomic existence test — `CocoaError.fileWriteFileExists` / POSIX `EEXIST` is treated as a conflict. `convertSymlinksToMoves` reads the actual symlink destination via `destinationOfSymbolicLink` and verifies it matches the proposal's `oldPath` before removing + moving. An attacker who swapped the symlink between apply and convert will be detected.
- **Path traversal containment (MEDIUM).** `RestructureEngine.compute` sanitizes bucket paths and VLM-proposed filenames (drops `..`, leading dots, `/` chars), then verifies the resolved target sits inside the picked library root before recording a proposal. `RestructureEngine.sanitizePathSegment` and `sanitizeFilename` are the choke points.
- **Zip-bomb defense (MEDIUM).** `CLIPModelInstaller.runExtract` checks ≥1 GB free disk on the target volume before invoking `unzip`, and bounds the extract with a 5-minute watchdog that calls `Process.terminate()` if it overruns.
- **Logging redaction (LOW).** `MobileCLIPService` model-load log calls now wrap their path argument in `redactPathForLog(_:)` for parity with the rest of the engine.

### Confirmed clean (no fix needed)

- **SQL injection.** Every `db.execute(sql:)` and `Row.fetchAll(db, sql:)` in the codebase uses `?` placeholders + `StatementArguments`. The dynamic IN-clause in `ReadStore.deleteFiles` builds placeholders via `.map { _ in "?" }.joined(separator: ",")` and passes IDs as arguments — safe. GRDB's typed API enforces this naturally.
- **Command injection.** `/usr/bin/unzip` is invoked via `Process.arguments` (array form), never via shell. The zip path is validated to be a regular file with a `.zip` extension and not a symlink before invocation.
- **App entitlements.** `Info.plist` declares only narrow folder-access usage descriptions (Desktop / Documents / Downloads). No full-disk-access, no AppleEvents, no network entitlements beyond what the model downloader inherits.
- **Engine respawn race.** `EngineClient` keeps a single `process` reference and uses bounded backoff (1 s / 4 s / 16 s, 60 s window). No multi-instance race.
- **File handle lifecycle.** `Process` pipes auto-close on termination. Security-scoped resources are released via `defer { url.stopAccessingSecurityScopedResource() }`.

## Open hardening — gates the v1.0 release (does NOT ship in the current build)

These three items **block the v1.0 ship gate**: they must land before the release tag, but they are not yet implemented and are not required for day-to-day development builds. (Note: on Windows the engine downloader already SHA256-verifies each model file at download time — the gap below is verify-*on-load* and the macOS path.)

- **Per-model SHA256 verification.** Models in `~/Library/Application Support/FileID/Models/` are loaded without checksum verification today. An attacker who can already write to that directory can replace a `.mlpackage`. Mitigation: record SHA256 on download in `CLIPModelInstaller`, verify on every load. Equivalent for the MLX VLMs under `~/Documents/huggingface/models/`.
- **Certificate pinning for HuggingFace.** No cert pinning today; an active MITM (or compromised CA) can swap downloads. Mitigation: pin `huggingface.co` via `URLSession` delegate. Best done together with the SHA256 work above.
- **Tokenizer DoS hardening.** `CLIPTokenizer` has size caps on `vocab.json` / `merges.txt` (8 MB / 4 MB) and a safety counter on the BPE merge loop. Add a sanity check that `bpeRanks.count <= 50000` to short-circuit pathological merges files, plus a deterministic abort when the safety counter exhausts.

## Reporting

Security issues: open a private security advisory on the GitHub repo, or email the project owner. Don't file public issues for vulnerabilities.
