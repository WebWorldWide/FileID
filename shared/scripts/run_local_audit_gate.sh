#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"

step() {
  printf '\n==> %s\n' "$1"
}

step "Diff and formatting"
git diff --check
cargo fmt --manifest-path platforms/windows/src/engine/Cargo.toml --all -- --check
cargo fmt --manifest-path platforms/cli/Cargo.toml --all -- --check
cargo fmt --manifest-path platforms/tui/Cargo.toml --all -- --check
cargo fmt --manifest-path platforms/linux/Cargo.toml --all -- --check

step "Shared Rust engine"
cargo test --manifest-path platforms/windows/src/engine/Cargo.toml --locked --all-targets
cargo clippy --manifest-path platforms/windows/src/engine/Cargo.toml --locked --all-targets -- -D warnings

step "CLI"
cargo test --manifest-path platforms/cli/Cargo.toml --locked
cargo clippy --manifest-path platforms/cli/Cargo.toml --locked --all-targets -- -D warnings

step "TUI"
cargo test --manifest-path platforms/tui/Cargo.toml --locked
cargo clippy --manifest-path platforms/tui/Cargo.toml --locked --all-targets -- -D warnings

step "macOS Swift 6"
(
  cd platforms/apple
  swift test -Xswiftc -strict-concurrency=complete -Xswiftc -warnings-as-errors
)

step "Repository policy"
python3 -m unittest discover -s shared/scripts -p 'test_*.py'
python3 shared/scripts/check_workflow_action_pins.py
python3 shared/scripts/check_workflow_permissions.py
python3 shared/scripts/check_bootstrap_supply_chain.py
python3 shared/scripts/check_runtime_egress.py --known-blockers
python3 shared/scripts/check_current_docs.py
python3 shared/scripts/check_model_license_policy.py
python3 packaging/flatpak/generate-cargo-sources.py --check
bash shared/scripts/check_tls_pins.sh

step "Local audit gate passed"
