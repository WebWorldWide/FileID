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
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
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

    artifact_ids = {artifact.get("id") for artifact in manifest.get("artifacts", [])}
    artifact_licenses = manifest.get("artifactLicenses", {})
    if set(artifact_licenses) != artifact_ids:
        missing = sorted(artifact_ids - set(artifact_licenses))
        extra = sorted(set(artifact_licenses) - artifact_ids)
        errors.append(f"artifactLicenses coverage mismatch; missing={missing}, extra={extra}")

    repos = {repo.get("repo") for repo in manifest.get("vlmRepos", [])}
    repo_licenses = manifest.get("vlmRepoLicenses", {})
    if set(repo_licenses) != repos:
        missing = sorted(repos - set(repo_licenses))
        extra = sorted(set(repo_licenses) - repos)
        errors.append(f"vlmRepoLicenses coverage mismatch; missing={missing}, extra={extra}")

    for name, key in {**artifact_licenses, **repo_licenses}.items():
        if key not in ALLOWED_POLICIES:
            errors.append(f"{name}: unknown license policy {key!r}")

    if errors:
        for error in errors:
            print(f"ERROR: {error}")
        return 1

    restricted = sum(
        1
        for key in [*artifact_licenses.values(), *repo_licenses.values()]
        if ALLOWED_POLICIES[key][1]
    )
    print(
        f"Model license policy OK: {len(artifact_ids)} artifacts, {len(repos)} repos, "
        f"{restricted} restricted entries require explicit terms acceptance."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
