# tools/git-hooks/

Local git hooks for FileID contributors. Drop-in install:

```bash
git config core.hooksPath tools/git-hooks
```

After install, every `git commit` runs the [pre-commit](./pre-commit) script:

- **Privacy-string scan** — fails if any of the 22 forbidden telemetry-SDK strings appear in staged files. Always runs.
- **Rust gates** (when `.rs` files are staged): `cargo fmt --check` + `cargo clippy --no-deps -- -D warnings`.
- **.NET gates** (when `.cs` / `.csproj` / `.xaml` files are staged): `dotnet format --verify-no-changes`.
- **Swift gates** (when `.swift` files are staged AND `swift-format` is installed): `swift-format lint --strict` on changed files.

Designed to finish in under 15 seconds on a warm cache. The full CI matrix (cargo test, dotnet test, cargo deny, cargo audit, coverage gate, parity gate) runs in GitHub Actions; the pre-commit hook only catches what the dev can fix faster locally.

## Override

`git commit --no-verify` bypasses the hook. Per [CLAUDE.md](../../CLAUDE.md), `--no-verify` is **not** the default workflow — only use it if you know exactly why the hook is wrong and you have a follow-up plan.

## Why not Husky / similar?

The git-hooks dir is platform-neutral, has no dependencies, and works for both Windows (Git Bash / PowerShell calling bash) and macOS contributors. A JS-based hook manager would require Node.js on every contributor's machine for a feature that's three shell commands.
