#!/usr/bin/env python3
import argparse
import json
import re
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

CATALOG_LICENSES = {"Apache-2.0", "MIT", "Gemma"}

# Never offered, whatever a model card's license field says. Hugging Face
# cards inherit wrong license and base_model metadata, so these are matched
# against names and repositories, not declared licenses. The generator applies
# the same list to every lineage hop.
RESTRICTED_MODELS = (
    (r"qwen2\.5-(vl-|coder-|math-|omni-)?3b", "Qwen Research License (non-commercial)"),
    (r"(qwen[0-9.]*|qvq)-(vl-|coder-|math-)?72b|internvl(2\.5|3)-78b", "Qwen License (monthly-active-user cap)"),
    (r"(^|[^a-z])(code)?llama|meta-llama/", "Llama Community License (includes Llama-derived distills)"),
    (r"fastvlm|mobileclip|(^|[^a-z])apple/", "Apple research license"),
    (r"(^|[^a-z])lfm[0-9]|liquidai/", "LFM Open License (revenue threshold)"),
    (r"codestral-22b|mistral-large|pixtral-large|ministral-8b-instruct-2410|devstral-2-123b",
     "Mistral Research or revenue-capped license"),
    (r"embeddinggemma-2", "card requires the Gemma Prohibited Use Policy (owner decision 2026-11-06)"),
    (r"abliterat|uncensor|heretic|obliterat|dolphin-?(\d|mistral|llama|qwen|mixtral|yi|phi)|cognitivecomputations/|dphn/",
     "safety training removed"),
    (r"claude|(^|[^a-z])(opus|sonnet|haiku|fable|composer)-?\d|gpt-?[45]|gemini|(^|[^a-z])grok-?\d|deepseek-v\d+-pro|fuseo1",
     "named after another vendor's model (unclear provenance)"),
)

# Generators that are not language models. Nothing in FileID consumes an
# image-generation or text-to-speech role, so the catalog leaves them out.
NON_LLM_MODEL = re.compile(
    r"qwen-image|ernie-image|z-image|flux|stable-diffusion|sdxl|(^|[^a-z])sd-?3|(^|[^a-z])wan-?2|hunyuan-?(image|video)"
    r"|(^|[^a-z0-9])tts([^a-z]|$)|voxcpm|kokoro|chatterbox|cosyvoice|musicgen|text-to-speech|text-to-image"
)
NON_LLM_FILE = re.compile(r"(^|/)(transformer|vae|text_encoder|unet|scheduler)/")

# Gemma lines Google released under plain Apache-2.0
# (https://ai.google.dev/gemma/docs/gemma_4_license, checked 2026-11-06).
# Every other Gemma repository is under the Gemma Terms.
APACHE_GEMMA = re.compile(r"(?<![a-z])(gemma-4|diffusiongemma)-")

_SIZE = re.compile(r"(?<![a-z0-9.])([ae]?\d+(?:\.\d+)?[bm])(?![a-z])")


def _normalized(text: str) -> str:
    return text.lower().replace("_", "-")


def _canonical(text: str) -> str:
    return re.sub(r"[^a-z0-9]", "", text.lower())


def _sizes(text: str) -> set[str]:
    return set(_SIZE.findall(_normalized(text)))


# Words that make a different model of the same family and size; a
# conversion and its declared base must agree on them.
VARIANT_WORDS = {"instruct", "it", "chat", "thinking", "reasoning", "vl", "coder", "math", "embedding", "reranker", "ocr"}
CONVERSION_SUFFIX = re.compile(
    r"([-_](gguf|mlx|q\d[\w]*|iq\d\w*|[2-8]-?bit|bf16|fp16|f16|mxfp4\w*|dwq|optiq|awq|gptq|int[48]|lm))+$", re.I
)


def variant_words(name: str) -> set[str]:
    return set(re.findall(r"[a-z]+", name.lower())) & VARIANT_WORDS


def lineage_problem(repo: str, base: str) -> str | None:
    """Why a conversion's first declared base is not the model it holds."""
    held = CONVERSION_SUFFIX.sub("", repo.split("/", 1)[-1]) or repo
    declared = CONVERSION_SUFFIX.sub("", base.split("/", 1)[-1]) or base
    if _sizes(held) - _sizes(declared):
        return f"holds {held}, a resized derivative of {base}"
    if variant_words(held) != variant_words(declared):
        return f"is {sorted(variant_words(held))}, first base {base} is {sorted(variant_words(declared))}"
    return None


def restricted_reason(*names: str) -> str | None:
    for name in names:
        for pattern, reason in RESTRICTED_MODELS:
            if re.search(pattern, _normalized(name or "")):
                return reason
    return None


def identity_mismatch(name: str, family: str, repo: str, upstream: str) -> str | None:
    """Why a catalog name does not describe the repository it downloads."""
    repo_name = repo.split("/", 1)[-1]
    upstream_name = upstream.split("/", 1)[-1]
    family_key = _canonical(family)
    if not family_key or not _canonical(name).startswith(family_key):
        return f"family {family!r} is not the start of name {name!r}"
    if family_key not in _canonical(repo_name):
        return f"family {family!r} does not match repo {repo}"
    if family_key not in _canonical(upstream_name):
        return f"family {family!r} does not match upstream {upstream}"
    if not _sizes(name) <= _sizes(repo_name):
        return f"size {sorted(_sizes(name) - _sizes(repo_name))} of {name!r} is not in repo {repo}"
    if not _sizes(upstream_name) <= _sizes(name):
        return f"upstream {upstream} size {sorted(_sizes(upstream_name) - _sizes(name))} is not in name {name!r}"
    return None


def gemma_terms_error(name: str, repo: str, license_name: str, terms_required: bool) -> str | None:
    subjects = [_normalized(name), _normalized(repo.split("/", 1)[-1])]
    if not any("gemma" in subject for subject in subjects):
        return None
    if any(APACHE_GEMMA.search(subject) for subject in subjects):
        return None if license_name == "Apache-2.0" else "Gemma 4 family is Apache-2.0"
    if license_name != "Gemma" or terms_required is not True:
        return "Gemma-family repository must be license Gemma with termsRequired"
    return None


def non_llm(name: str, repo: str, upstream: str, paths: list[str]) -> bool:
    return any(NON_LLM_MODEL.search(_normalized(text)) for text in (name, repo, upstream)) \
        or any(NON_LLM_FILE.search(path) for path in paths)


def mac_catalog_errors(catalog_path: Path) -> list[str]:
    """The Mac MLX catalog has no generator; the denylist guards it here."""
    document = json.loads(catalog_path.read_text(encoding="utf-8"))
    errors = []
    for entry in document.get("entries", []):
        lineage = [entry.get("repo", ""), *(base.get("repo", "") for base in entry.get("bases", []))]
        reason = restricted_reason(*lineage)
        if reason:
            errors.append(f"Mac catalog {entry.get('repo')}: denylisted model ({reason})")
        if non_llm(lineage[0], lineage[0], lineage[-1], [f.get("path", "") for f in entry.get("files", [])]):
            errors.append(f"Mac catalog {entry.get('repo')}: image-generation or text-to-speech model is not offered")
        if len(lineage) < 2:
            errors.append(f"Mac catalog {entry.get('repo')}: no declared base")
        elif problem := lineage_problem(lineage[0], lineage[1]):
            errors.append(f"Mac catalog {entry.get('repo')}: {problem}")
    return errors


def catalog_errors(catalog_path: Path) -> list[str]:
    document = json.loads(catalog_path.read_text(encoding="utf-8"))
    errors: list[str] = []
    seen: set[str] = set()
    for entry in document.get("entries", []):
        entry_id = entry.get("id", "?")
        name, family = entry.get("name", ""), entry.get("family", "")
        repo, upstream = entry.get("repo", ""), entry.get("upstream", "")
        license_name, terms_required = entry.get("license"), entry.get("termsRequired")
        if entry_id in seen:
            errors.append(f"catalog {entry_id}: duplicate id")
        seen.add(entry_id)
        if license_name not in CATALOG_LICENSES:
            errors.append(f"catalog {entry_id}: license {license_name!r} is not allowed")
        if license_name == "Gemma" and terms_required is not True:
            errors.append(f"catalog {entry_id}: Gemma Terms require termsRequired")
        reason = restricted_reason(name, upstream, repo)
        if reason:
            errors.append(f"catalog {entry_id}: denylisted model ({reason})")
        mismatch = identity_mismatch(name, family, repo, upstream)
        if mismatch:
            errors.append(f"catalog {entry_id}: {mismatch}")
        gemma = gemma_terms_error(name, repo, license_name, terms_required)
        if gemma:
            errors.append(f"catalog {entry_id}: {gemma}")
        if non_llm(name, repo, upstream, [f.get("path", "") for f in entry.get("files", [])]):
            errors.append(f"catalog {entry_id}: image-generation or text-to-speech model is not offered (role {entry.get('role')!r})")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate FileID model license metadata")
    parser.add_argument(
        "manifest",
        nargs="?",
        default="shared/models/manifest.json",
        type=Path,
    )
    parser.add_argument(
        "--catalog",
        default=Path(__file__).resolve().parents[2] / "shared/models/catalog.json",
        type=Path,
        help="extended model catalog to check (shared/models/catalog.json)",
    )
    parser.add_argument(
        "--mac-catalog",
        default=Path(__file__).resolve().parents[2] / "platforms/apple/shared/Sources/FileIDModelInfo/Resources/model-catalog.json",
        type=Path,
        help="Mac MLX model catalog to check against the denylist",
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
    parser_mac = args.mac_catalog
    try:
        errors.extend(catalog_errors(args.catalog))
        errors.extend(mac_catalog_errors(parser_mac))
    except (ValueError, OSError, AttributeError) as error:
        errors.append(f"Model catalog: {error}")

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
