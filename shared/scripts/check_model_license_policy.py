#!/usr/bin/env python3
import argparse
import json
from datetime import date
from pathlib import Path
from urllib.parse import urlparse

ALLOWED_POLICIES = {
    "MIT": ("MIT", False),
    "Apache-2.0": ("Apache-2.0", False),
    "Gemma": ("LicenseRef-Gemma", True),
    "NVIDIA-cuDNN": ("LicenseRef-NVIDIA-cuDNN", True),
    "NVIDIA-CUDA": ("LicenseRef-NVIDIA-CUDA", True),
}


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate FileID model license metadata")
    parser.add_argument(
        "manifest",
        nargs="?",
        default="shared/models/manifest.json",
        type=Path,
    )
    args = parser.parse_args()
    try:
        manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        print(f"ERROR: unreadable model manifest: {error}")
        return 1
    if not isinstance(manifest, dict):
        print("ERROR: model manifest must be a JSON object")
        return 1
    errors: list[str] = []

    policies = manifest.get("licensePolicies", {})
    if set(policies) != set(ALLOWED_POLICIES):
        errors.append(
            f"licensePolicies keys must be {sorted(ALLOWED_POLICIES)}; got {sorted(policies)}"
        )
    for key, (spdx, terms_required) in ALLOWED_POLICIES.items():
        policy = policies.get(key, {})
        if policy.get("spdx") != spdx:
            errors.append(f"{key}: spdx must be {spdx}")
        if policy.get("termsRequired") is not terms_required:
            errors.append(f"{key}: termsRequired must be {terms_required}")
        url = policy.get("licenseUrl", "")
        parsed = urlparse(url)
        if parsed.scheme != "https" or not parsed.netloc:
            errors.append(f"{key}: licenseUrl must be an absolute HTTPS URL")
        reviewed = policy.get("reviewedAt", "")
        try:
            parsed_reviewed = date.fromisoformat(reviewed)
        except (TypeError, ValueError):
            parsed_reviewed = None
        if parsed_reviewed is None or parsed_reviewed.isoformat() != reviewed:
            errors.append(f"{key}: reviewedAt must be a valid YYYY-MM-DD calendar date")

    artifacts = manifest.get("artifacts")
    repos = manifest.get("vlmRepos")
    if not isinstance(artifacts, list) or not artifacts or not isinstance(repos, list):
        errors.append("artifacts must be a non-empty list and vlmRepos a list")
        artifacts, repos = [], []
    licenses: list[str] = []
    for entry in [*artifacts, *repos]:
        name = entry.get("id") or entry.get("repo") if isinstance(entry, dict) else None
        key = entry.get("license") if isinstance(entry, dict) else None
        if not name:
            errors.append(f"manifest entry without an id or repo: {entry!r}")
        elif key not in ALLOWED_POLICIES:
            errors.append(f"{name}: unknown license policy {key!r}")
        else:
            licenses.append(key)

    repository = Path(__file__).resolve().parents[2]
    if args.manifest.resolve() == repository / "shared/models/manifest.json":
        from check_model_catalog import check
        try:
            entries = check(repository)
            print(f"Offline model catalog checked: {len(entries)} permissive entries.")
        except (ValueError, KeyError, TypeError, OSError) as error:
            errors.append(f"Model catalog: {error}")

    if errors:
        for error in errors:
            print(f"ERROR: {error}")
        return 1

    restricted = sum(1 for key in licenses if ALLOWED_POLICIES[key][1])
    print(
        f"Model license policy OK: {len(artifacts)} artifacts, {len(repos)} repos, "
        f"{restricted} restricted entries require explicit terms acceptance."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
