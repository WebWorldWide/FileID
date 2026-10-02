#!/usr/bin/env python3
"""Keep persistent Adlon runners behind reviewed-main event guards."""

from pathlib import Path
import re
import sys

GUARD = "github.ref == 'refs/heads/main' && github.event_name != 'pull_request'"
LINUX = "fromJSON('[\"self-hosted\",\"Linux\",\"X64\",\"fileid-adlon\"]')"
WINDOWS = "fromJSON('[\"self-hosted\",\"Windows\",\"X64\",\"fileid-adlon\"]')"
ROUTES = {
    "linux.yml": f"{GUARD} && {LINUX} || 'ubuntu-latest'",
    "windows-app.yml": f"{GUARD} && {WINDOWS} || matrix.runner",
    "windows-engine.yml": f"{GUARD} && matrix.label != 'arm64-native' && {WINDOWS} || matrix.runner",
}


def check(root: Path) -> list[str]:
    failures = []
    for path in sorted(root.glob("*.yml")):
        source = path.read_text()
        routes = re.findall(r"^\s*runs-on:\s*(.+)$", source, re.M)
        multiline = bool(re.search(r"^\s*-\s*self-hosted\s*$", source, re.M))
        if not any("self-hosted" in route for route in routes) and not multiline:
            continue
        if re.search(r"^\s*pull_request_target\s*:", source, re.M):
            failures.append(f"{path.name}: privileged pull-request events cannot target Adlon")
        expected = ROUTES.get(path.name)
        for route in routes:
            if "self-hosted" in route and route != "${{ " + str(expected) + " }}":
                failures.append(f"{path.name}: unreviewed self-hosted route")
        if multiline:
            failures.append(f"{path.name}: unguarded multiline self-hosted route")
    return failures


if __name__ == "__main__":
    errors = check(Path(__file__).resolve().parents[2] / ".github/workflows")
    if errors:
        print("Self-hosted runner policy failed:\n  " + "\n  ".join(errors))
        sys.exit(1)
    print("Self-hosted runner policy passed: Adlon runs reviewed main, PRs remain hosted.")
