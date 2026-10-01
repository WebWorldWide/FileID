# FileID next-version agent handoff

Work on `codex/fileid-next-version-current`. The owner requests continued implementation and incremental verified GitHub pushes. The accepted scope and honest completion ledger are in [NEXT_VERSION.md](NEXT_VERSION.md); update that ledger and STATE/NEXT/DECISIONS with each milestone. Do not declare the full release finished while its feature or hardware gates remain.

## Non-negotiable safeguards

Adlon is strictly read-only example data. Never create outputs, caches, databases, tags, logs, sidecars, thumbnails, or temporary files there; never rename, move, repair, or delete its contents. Use internal temporary fixtures for mutation tests. Apply shared Swift/Rust read-only-location guards before writes, including missing destinations and aliases. Preserve managed media-library originals. No telemetry or runtime network features beyond user-initiated Hugging Face model downloads.

## Implementation order and contracts

The catalog/timeline/manual-chapter/concise-name foundation and initial safe photo/chapter export tools are implemented. Draft PR: https://github.com/WebWorldWide/FileID/pull/186. See TOOLS.md for exact formats and adapter limits. The remote history was rewritten; its baseline tree matched the original checkout, so the foundation was cherry-picked onto current main without overwriting owner changes. Continue with resource/capability reporting, persistent hybrid retrieval, People improvements, local chat and typed reversible operations, then temporal analysis, best takes and media tools. Land canonical IPC changes first in `shared/ipc-schema/ipc.schema.json`, mirror Swift/Rust/C# (GTK inherits Rust), and test conformance. New committed migrations are immutable: add v22 and later. Do not promote research models without license/hash/runtime verification and measured FileID quality/latency gates. Preserve the six tabs, native interfaces, palette, springs, and LavaLampBackground.

## Verification

Run resource-heavy checks sequentially on this 16 GB Mac. macOS: from `platforms/apple`, use `FILEID_TEST_ENGINE_PATH=/tmp/fileid-next-build/debug/FileIDEngine swift test --build-system native --scratch-path /tmp/fileid-next-build --jobs 4`, then `swift build --product FileID --build-system native --scratch-path /tmp/fileid-next-build --jobs 4`. Desktop FileProvider metadata can break generated resource-bundle signatures; internal scratch products avoid it. Do not overwrite the installed release or wipe its database.

Rust: from `platforms/windows/src/engine`, run `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --check`; CI pins Rust 1.90. Prepend `/Users/adamnolle/.rustup/toolchains/1.90-aarch64-apple-darwin/bin` to PATH and confirm `cargo clippy --version`: `rustup run 1.90 cargo clippy` alone can select Homebrew Clippy 1.98. C# IPC tests run with `/tmp/fileid-dotnet/dotnet` and `DOTNET_CLI_TELEMETRY_OPTOUT=1`. Full WinUI runtime and GTK4 acceptance require their native hosts; macOS compilation is not proof of parity. Existing broad C# formatting failures predate this work; verify changed files without unrelated mass formatting.

Shared SQL parity: `python3 shared/scripts/check_catalog_schema.py`. Actual database interoperability: `python3 shared/scripts/check_catalog_roundtrip.py --swift-engine /tmp/fileid-next-build/debug/FileIDEngine --rust-engine platforms/windows/src/engine/target/debug/FileIDEngine`. Synthetic keyword benchmark: `shared/scripts/benchmark_catalog_search.py`; its results do not prove semantic/chat/model contention targets. Validate binary privacy and repository policy scripts before pushing.

## Workspace preservation

Uncommitted owner work predates this implementation in `.serena/project.yml`, Apple bulk-mutation tests, Apple release/metallib scripts, and September signing entries in STATE/NEXT/DECISIONS. Preserve it and stage implementation changes separately. Never revert or silently include that work in an unrelated feature commit. The owner now authorizes commits and pushes; push coherent validated milestones to this branch, inspect GitHub CI, and record failures honestly. Full model-based timeline quality, native port UI, hardware matrix, packaging, and release acceptance remain pending.

The runtime-egress policy pins reviewed source digests, including the local decoder Process boundary. Review transport/local-loader changes before refreshing those hashes; do not weaken the gate. Tool export plans/journals are cross-engine compatible; `shared/scripts/check_tools_roundtrip.py` verifies both directions. macOS photo decoding is isolated/cancellable; Rust cancellation must remain unavailable until its worker implementation exists.
