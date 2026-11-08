#!/usr/bin/env python3
"""Build shared/models/catalog.json — the extended local-model catalog.

Discovers llama.cpp (GGUF) and MLX repositories from trusted publishers on
Hugging Face, traces each to its upstream base model, keeps only upstream
licenses FileID may offer (Apache-2.0 / MIT, plus opt-in Gemma terms),
rejects denylisted models and any repository whose own id or config
disagrees with the base model its card declares,
dedupes re-quantizations of the same base model, and pins an immutable
revision with per-file SHA256 and sizes for every artifact.

Developer-time tool: it queries huggingface.co. The app itself never runs it;
the app's only egress remains user-initiated downloads of pinned files.

Every entry starts with status "candidate". Only `fileid-bench` promotes an
entry to "verified" after it loads and answers on real hardware.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_model_license_policy import (  # noqa: E402
    catalog_errors,
    gemma_terms_error,
    identity_mismatch,
    non_llm,
    restricted_reason,
    _sizes,
)

API = "https://huggingface.co/api"

# Publishers whose conversions FileID trusts, best first. Official upstream
# releases win over community re-quantizations of the same base model.
PUBLISHERS = [
    "ggml-org", "Qwen", "google", "mistralai", "ibm-granite", "HuggingFaceTB", "microsoft",
    "allenai", "lmstudio-community", "unsloth", "bartowski", "mlx-community",
]
# Upstream organisations whose base models may be offered. LiquidAI (LFM
# license) and meta-llama (Llama license) publish only restricted weights.
UPSTREAM_ORGS = {
    "Qwen", "google", "mistralai", "ibm-granite", "HuggingFaceTB", "microsoft", "allenai",
    "OpenGVLab", "deepseek-ai", "zai-org", "THUDM", "nvidia", "openai",
    "BAAI", "nomic-ai", "intfloat", "Snowflake", "jinaai", "mixedbread-ai", "ggerganov",
    "openbmb", "moonshotai", "baidu", "tencent", "PaddlePaddle", "rednote-hilab", "dots-studio",
    "lightonai", "stepfun-ai", "XiaomiMiMo", "tiiuae", "CohereLabs",
}
ALLOWED_LICENSES = {"apache-2.0": "Apache-2.0", "mit": "MIT", "gemma": "Gemma"}
# A card's license_name, or a license_link to one of these licenses,
# overrides an apache-2.0 or mit license field.
RESTRICTED_LICENSE_NAME = re.compile(r"qwen|tongyi|llama|lfm|apple|amlr|research|non-?commercial|cc-by-nc", re.I)
RESTRICTED_LICENSE_LINK = re.compile(r"llama|lfm|qwen2(\.5)?-(vl-)?72b|research|non-?commercial", re.I)
# Cards whose license_name/link is a reviewed copy-paste error. InternVL3
# 1B-14B cards link the Qwen2.5-72B license, but their README releases the
# weights under MIT and their language parts are Apache-2.0 Qwen2.5 0.5B-14B.
REVIEWED_LICENSE_REFERENCES = {
    "OpenGVLab/InternVL3-1B-Instruct", "OpenGVLab/InternVL3-2B-Instruct",
    "OpenGVLab/InternVL3-8B-Instruct", "OpenGVLab/InternVL3-14B-Instruct",
}
# Image generators and text-to-speech models: no FileID role consumes them.
NON_LLM_ARCHITECTURES = {
    "qwen_image", "wan", "flux", "sd3", "stable_diffusion", "hunyuan_video", "qwen3_tts", "voxcpm", "voxcpm2", "kokoro",
}
NON_LLM_PIPELINES = {
    "text-to-image", "image-to-image", "text-to-video", "image-to-video", "text-to-speech", "text-to-audio",
    "unconditional-image-generation",
}
NON_LLM_LIBRARIES = {"diffusers", "mflux"}
# Reviewed roles that override what the repository's files suggest, keyed by
# conversion repository or upstream. FrogNano's GGUF ships the projector
# inherited from Qwen3.5-4B, but its card says image and video input are
# unsupported. The gemma-3n `-lm` build holds only language-model tensors.
ROLE_OVERRIDES = {
    "microsoft/FrogNano-4B-2609": "text",
    "mlx-community/gemma-3n-E2B-it-lm-4bit": "text",
}
# Reviewed base models for repositories whose card declares the wrong one
# (a -pt/base checkpoint, a backbone, a private training checkpoint or a
# pre-rename organisation). Each conversion's README names the release it was
# converted from; the upstream's license and lineage are still checked.
# smollm-360M-instruct-add-basics has the same safetensors SHA256 as
# SmolLM-360M-Instruct (v0.2); the 1.7B sibling's checkpoint is private.
UPSTREAM_OVERRIDES = {
    "mlx-community/Qwen2.5-0.5B-Instruct-4bit": "Qwen/Qwen2.5-0.5B-Instruct",
    "mlx-community/Qwen2.5-1.5B-Instruct-4bit": "Qwen/Qwen2.5-1.5B-Instruct",
    "mlx-community/Qwen2.5-7B-Instruct-4bit": "Qwen/Qwen2.5-7B-Instruct",
    "mlx-community/Qwen2.5-14B-Instruct-4bit": "Qwen/Qwen2.5-14B-Instruct",
    "mlx-community/Qwen2.5-32B-Instruct-4bit": "Qwen/Qwen2.5-32B-Instruct",
    "mlx-community/gemma-3-4b-it-8bit": "google/gemma-3-4b-it",
    "mlx-community/gemma-3-12b-it-4bit": "google/gemma-3-12b-it",
    "mlx-community/olmOCR-2-7B-1025-mlx-8bit": "allenai/olmOCR-2-7B-1025",
    "mlx-community/PaddleOCR-VL-1.5-4bit": "PaddlePaddle/PaddleOCR-VL-1.5",
    "HuggingFaceTB/smollm-360M-instruct-v0.2-Q8_0-GGUF": "HuggingFaceTB/SmolLM-360M-Instruct",
    "HuggingFaceTB/smollm-1.7B-instruct-v0.2-Q8_0-GGUF": "HuggingFaceTB/SmolLM-1.7B-Instruct",
    "unsloth/GLM-4-32B-0414-GGUF": "zai-org/GLM-4-32B-0414",
}
# Words that make a different model of the same family and size; a
# conversion and its declared base must agree on them.
VARIANT_WORDS = {"instruct", "it", "chat", "thinking", "reasoning", "vl", "coder", "math", "embedding", "reranker", "ocr"}
CONVERSION_SUFFIX = re.compile(
    r"([-_](gguf|mlx|q\d[\w]*|iq\d\w*|[2-8]-?bit|bf16|fp16|f16|mxfp4\w*|dwq|optiq|awq|gptq|int[48]|lm))+$", re.I
)
EXCLUDE = re.compile(
    r"abliterat|uncensor|nsfw|heretic|lora-|-lora|roleplay|merge|test|-pt-|base-gguf|-base$|-base-|draft|mtp|speculative"
    r"|imatrix|bitnet|madlad|coder|safeguard|phone|guard-|-guard|embedding-gemma-test",
    re.I,
)
# Files that ship beside the weights but are not a standalone model.
AUXILIARY_FILE = re.compile(r"^(mtp|dspark|dflash|draft|eagle)-|imatrix|-mtp\b", re.I)
# Below ~3 bits per weight, quality drops too far to offer.
LOW_BIT = re.compile(r"IQ1|IQ2|TQ1|TQ2|Q2_K|Q2K|\bq2\b", re.I)
SPEECH_NAME = re.compile(r"voxtral|whisper|asr|audio|speech", re.I)
REASONING_NAME = re.compile(r"thinking|reasoning|r1-distill", re.I)
GGUF_QUANT_PREFERENCE = ["Q4_K_M", "Q4_K_S", "Q4_0", "IQ4_XS", "Q5_K_M", "Q8_0", "BF16", "F16"]
MMPROJ_PREFERENCE = ["f16", "F16", "bf16", "BF16", "Q8_0", "q8_0", "f32"]

ROLE_BY_PIPELINE = {
    "image-text-to-text": "vision",
    "any-to-any": "vision",
    "text-generation": "text",
    "feature-extraction": "embedding",
    "sentence-similarity": "embedding",
    "text-ranking": "reranker",
    "text-classification": "reranker",
    "automatic-speech-recognition": "speech",
}


CACHE: Path | None = None
CALLS = 0


def get(path: str, retries: int = 8):
    global CALLS
    url = path if path.startswith("http") else f"{API}/{path}"
    cached = CACHE / (hashlib.sha256(url.encode()).hexdigest() + ".json") if CACHE else None
    if cached and cached.exists():
        return json.loads(cached.read_text())
    for attempt in range(retries):
        try:
            CALLS += 1
            if CALLS % 100 == 0:
                print(f"... {CALLS} API calls", file=sys.stderr, flush=True)
            with urllib.request.urlopen(url, timeout=90) as response:
                data = json.load(response)
                if cached:
                    cached.write_text(json.dumps(data))
                return data
        except urllib.error.HTTPError as error:
            if error.code in (401, 403, 404):
                return None
            if attempt == retries - 1:
                raise
            if error.code == 429:
                time.sleep(int(error.headers.get("Retry-After") or 60))
                continue
        except (urllib.error.URLError, TimeoutError):
            if attempt == retries - 1:
                raise
        time.sleep(2 ** attempt)
    return None


def small_file_sha256(repo: str, sha: str, path: str, limit: int = 20_000_000) -> tuple[str, int] | None:
    """SHA256 of a non-LFS file at a pinned revision (configs, tokenizers)."""
    url = f"https://huggingface.co/{repo}/resolve/{sha}/{urllib.parse.quote(path)}"
    cached = CACHE / ("blob-" + hashlib.sha256(url.encode()).hexdigest() + ".json") if CACHE else None
    if cached and cached.exists():
        value = json.loads(cached.read_text())
        return value["sha256"], value["bytes"]
    for attempt in range(6):
        try:
            with urllib.request.urlopen(url, timeout=90) as response:
                data = response.read(limit + 1)
            if len(data) > limit:
                return None
            result = (hashlib.sha256(data).hexdigest(), len(data))
            if cached:
                cached.write_text(json.dumps({"sha256": result[0], "bytes": result[1]}))
            return result
        except urllib.error.HTTPError as error:
            if error.code in (401, 403, 404) or attempt == 5:
                return None
            time.sleep(int(error.headers.get("Retry-After") or 30) if error.code == 429 else 2 ** attempt)
        except (urllib.error.URLError, TimeoutError):
            if attempt == 5:
                return None
            time.sleep(2 ** attempt)
    return None


def license_of(info: dict) -> str | None:
    card = info.get("cardData") or {}
    license_value = card.get("license")
    if isinstance(license_value, list):
        license_value = license_value[0] if license_value else None
    if not license_value:
        license_value = next((t.split(":", 1)[1] for t in info.get("tags", []) if t.startswith("license:")), None)
    return license_value.lower() if isinstance(license_value, str) else None


def base_model_of(info: dict) -> str | None:
    tags = info.get("tags", [])
    for prefix in ("base_model:quantized:", "base_model:finetune:", "base_model:adapter:", "base_model:"):
        for tag in tags:
            if tag.startswith(prefix) and "/" in tag[len(prefix):]:
                candidate = tag[len(prefix):]
                if not candidate.startswith(("quantized:", "finetune:", "adapter:", "merge:")):
                    return candidate
    card = info.get("cardData") or {}
    base = card.get("base_model")
    if isinstance(base, list):
        base = base[0] if base else None
    return base if isinstance(base, str) else None


def restricted_license_reference(info: dict) -> bool:
    card = info.get("cardData") or {}
    restricted = RESTRICTED_LICENSE_NAME.search(str(card.get("license_name") or "")) \
        or RESTRICTED_LICENSE_LINK.search(str(card.get("license_link") or ""))
    return bool(restricted) and info.get("id") not in REVIEWED_LICENSE_REFERENCES


def license_chain(chain: list[tuple[str, dict]]) -> list[tuple[str, dict]]:
    """Hops whose license references are checked: a reviewed repository
    covers its own pretraining ancestors."""
    for index, (repo, _) in enumerate(chain):
        if repo in REVIEWED_LICENSE_REFERENCES:
            return chain[: index + 1]
    return chain


def lineage(repo: str, cache: dict, info: dict | None = None) -> list[tuple[str, dict]]:
    """The repo followed by its base_model ancestors (at most 4 hops). An
    ancestor Hugging Face cannot resolve keeps an empty info. A moved
    repository is named by the id Hugging Face redirects it to."""
    chain: list[tuple[str, dict]] = []
    current, current_info = repo, info
    while current and len(chain) < 5 and all(current != seen for seen, _ in chain):
        if current_info is None:
            current_info = cache.get(current)
            if current_info is None:
                current_info = get(f"models/{current}") or {}
                cache[current] = current_info
            current = current_info.get("id") or current
        chain.append((current, current_info))
        current, current_info = UPSTREAM_OVERRIDES.get(current) or base_model_of(current_info), None
    return chain


def model_name(repo: str) -> str:
    """The model a repository holds, read from its own id: publisher prefix
    (`bartowski/mistralai_X-GGUF`) and conversion suffixes removed."""
    name = repo.split("/", 1)[1]
    owner, _, rest = name.partition("_")
    if rest and owner in UPSTREAM_ORGS:
        name = rest
    return CONVERSION_SUFFIX.sub("", name) or name


def family_of(name: str) -> str:
    return re.split(r"[-_]\d", name, maxsplit=1)[0]


def variant_words(name: str) -> set[str]:
    return set(re.findall(r"[a-z]+", name.lower())) & VARIANT_WORDS


def architecture_family(info: dict) -> str | None:
    model_type = (info.get("config") or {}).get("model_type") or ""
    match = re.match(r"[a-z]+", model_type.lower())
    return match.group(0) if match else None


def not_language_model(info: dict, base_repo: str, base_info: dict) -> bool:
    for item in (info, base_info):
        architecture = (item.get("config") or {}).get("model_type") or (item.get("gguf") or {}).get("architecture")
        if architecture in NON_LLM_ARCHITECTURES or item.get("pipeline_tag") in NON_LLM_PIPELINES \
                or item.get("library_name") in NON_LLM_LIBRARIES:
            return True
    return non_llm(model_name(base_repo), info["id"], base_repo, [])


def upstream_of(chain: list[tuple[str, dict]]) -> tuple[str, dict]:
    """The original release a conversion was made from: the nearest ancestor
    owned by an upstream organisation, skipping community conversions."""
    for repo, info in chain[1:]:
        if repo.split("/", 1)[0] in UPSTREAM_ORGS:
            return repo, info
    return chain[1] if len(chain) > 1 else chain[0]


def identity_problem(chain: list[tuple[str, dict]], upstream: str, upstream_info: dict) -> str | None:
    """Why the repository's own files and id disagree with its declared base.
    Cards copy base_model and license from unrelated models, so the repo id
    and config decide what the model is."""
    repo, info = chain[0]
    hops = [name for name, _ in chain]
    through_upstream = chain[: hops.index(upstream) + 1] if upstream in hops else chain
    if not upstream_info or any(not hop_info for _, hop_info in through_upstream):
        return "lineage up to the upstream release contains a repository Hugging Face cannot resolve"
    held = model_name(repo)
    declared = model_name(upstream) if upstream != repo else held
    mismatch = identity_mismatch(declared, family_of(declared), repo, upstream)
    if mismatch:
        return mismatch
    if not re.sub(r"[^a-z0-9]", "", held.lower()).startswith(re.sub(r"[^a-z0-9]", "", family_of(declared).lower())):
        return f"repo {repo} holds {held}, a derivative named after its base {upstream}"
    if _sizes(held) - _sizes(declared):
        return f"repo {repo} holds {held}, a resized derivative of {upstream}"
    if variant_words(held) != variant_words(declared):
        return f"repo {repo} is {sorted(variant_words(held))}, declared base {upstream} is {sorted(variant_words(declared))}"
    held_architecture, declared_architecture = architecture_family(info), architecture_family(upstream_info)
    if held_architecture and declared_architecture and held_architecture != declared_architecture:
        return f"repo architecture {held_architecture} differs from base {declared_architecture}"
    return None


def params_billion(name: str) -> float | None:
    match = re.search(r"(?<![\d.])(\d+(?:\.\d+)?)\s*([BbMm])(?![a-z])", name.replace("_", "-"))
    if not match:
        return None
    value = float(match.group(1))
    return value / 1000 if match.group(2) in "Mm" else value


def active_billion(name: str) -> float | None:
    match = re.search(r"-A(\d+(?:\.\d+)?)B", name)
    return float(match.group(1)) if match else None


def pick(files: list[dict], preference: list[str], predicate) -> dict | None:
    """First file matching the preference order; None when nothing preferred
    matches, so unknown or low-bit quantizations are never chosen by default."""
    candidates = [f for f in files if predicate(f["path"])]
    for key in preference:
        for f in candidates:
            if key.lower() in Path(f["path"]).name.lower():
                return f
    return None


def gguf_files(repo: str, sha: str) -> list[dict] | None:
    tree = get(f"models/{repo}/tree/{sha}?recursive=true")
    if not tree:
        return None
    files = []
    for item in tree:
        lfs = item.get("lfs") or {}
        if item.get("type") == "file" and item["path"].lower().endswith(".gguf") and lfs.get("oid"):
            files.append({"path": item["path"], "sha256": lfs["oid"], "bytes": lfs.get("size") or item.get("size")})
    return files


def is_split(path: str) -> bool:
    return bool(re.search(r"-\d{5}-of-\d{5}\.gguf$", path))


def mlx_quant_rank(repo: str) -> int:
    """Among one publisher's MLX builds of a model, prefer 4-bit, then larger
    quantizations, then unrecognised ones, then full-precision weights."""
    order = ["4bit", "5bit", "6bit", "8bit", "3bit"]
    name = repo.lower()
    if re.search(r"bf16|fp16", name):
        return len(order) + 1
    return next((index for index, quant in enumerate(order) if quant in name), len(order))


def tier_for(resident_gb: float) -> str:
    for limit, tier in [(3, "tiny"), (6, "small"), (12, "medium"), (24, "large"), (48, "xlarge")]:
        if resident_gb <= limit:
            return tier
    return "workstation"


def build_gguf_entry(info: dict, base_repo: str, base_info: dict, license_key: str) -> dict | None:
    repo, sha = info["id"], info["sha"]
    files = gguf_files(repo, sha)
    if not files:
        return None
    weights = [
        f for f in files
        if "mmproj" not in f["path"].lower() and not is_split(f["path"])
        and not AUXILIARY_FILE.search(Path(f["path"]).name) and not LOW_BIT.search(Path(f["path"]).name)
    ]
    model = pick(weights, GGUF_QUANT_PREFERENCE, lambda p: True)
    if not model:
        return None
    projector = pick(files, MMPROJ_PREFERENCE, lambda p: "mmproj" in p.lower())
    pipeline = base_info.get("pipeline_tag") or info.get("pipeline_tag")
    role = ROLE_BY_PIPELINE.get(pipeline or "", "text")
    if SPEECH_NAME.search(base_repo) or (pipeline or "").startswith("audio") or pipeline == "automatic-speech-recognition":
        role = "speech"
    elif projector and role == "text":
        role = "vision"
    if role == "vision" and (not projector or REASONING_NAME.search(base_repo) or REASONING_NAME.search(repo)):
        return None
    artifacts = [{"role": "model", **model}]
    if projector:
        artifacts.append({"role": "projector", **projector})
    total = sum(a["bytes"] for a in artifacts)
    quant = next(q for q in GGUF_QUANT_PREFERENCE if q.lower() in Path(model["path"]).name.lower())
    resident_gb = total / 1e9 * 1.15 + (0.6 if role in ("vision", "text") else 0.1)
    return finish_entry(info, base_repo, base_info, license_key, "gguf", role, quant, artifacts, total, resident_gb, sha)


def build_mlx_entry(info: dict, base_repo: str, base_info: dict, license_key: str) -> dict | None:
    repo, sha = info["id"], info["sha"]
    tree = get(f"models/{repo}/tree/{sha}?recursive=true")
    if not tree:
        return None
    artifacts = []
    for item in tree:
        if item.get("type") != "file":
            continue
        if item["path"].startswith(".") or item["path"].lower().endswith((".md", ".png", ".jpg", ".gitattributes")):
            continue
        lfs = item.get("lfs") or {}
        if lfs.get("oid"):
            artifacts.append({"role": "file", "path": item["path"], "sha256": lfs["oid"], "bytes": lfs.get("size") or item.get("size")})
            continue
        pinned = small_file_sha256(repo, sha, item["path"])
        if not pinned:
            return None
        artifacts.append({"role": "file", "path": item["path"], "sha256": pinned[0], "bytes": pinned[1]})
    if not any(a["path"].endswith(".safetensors") for a in artifacts):
        return None
    pipeline = base_info.get("pipeline_tag") or info.get("pipeline_tag")
    role = "speech" if SPEECH_NAME.search(base_repo) else ROLE_BY_PIPELINE.get(pipeline or "", "text")
    if role == "vision" and (REASONING_NAME.search(base_repo) or REASONING_NAME.search(repo)):
        return None
    if re.search(r"[123]bit|optiq-[12]", repo, re.I) and not re.search(r"[4-8]bit", repo, re.I):
        return None
    total = sum(a["bytes"] or 0 for a in artifacts)
    quant = next((q for q in ["3bit", "4bit", "5bit", "6bit", "8bit", "bf16", "fp16"] if q in repo.lower()), "other")
    resident_gb = total / 1e9 * 1.15 + 0.6
    return finish_entry(info, base_repo, base_info, license_key, "mlx", role, quant, artifacts, total, resident_gb, sha)


def finish_entry(info, base_repo, base_info, license_key, fmt, role, quant, artifacts, total, resident_gb, sha) -> dict:
    repo = info["id"]
    name = model_name(base_repo)
    return {
        "id": re.sub(r"[^a-z0-9]+", "_", f"{name}_{fmt}_{quant}".lower()).strip("_"),
        "name": name,
        "family": family_of(name),
        "upstream": base_repo,
        "repo": repo,
        "revision": sha,
        "format": fmt,
        "role": role,
        "quantization": quant,
        "license": ALLOWED_LICENSES[license_key],
        "termsRequired": license_key == "gemma",
        "parametersBillion": params_billion(name),
        "activeParametersBillion": active_billion(name),
        "downloadBytes": total,
        "estimatedResidentGB": round(resident_gb, 2),
        "tier": tier_for(resident_gb),
        "platforms": ["macos"] if fmt == "mlx" else ["windows", "linux", "macos"],
        "files": [
            {**a, "url": f"https://huggingface.co/{repo}/resolve/{sha}/{urllib.parse.quote(a['path'])}"}
            for a in artifacts
        ],
        "status": "candidate",
        "upstreamCreatedAt": (base_info.get("createdAt") or "")[:10],
    }


def discover(authors: list[str], formats: set[str], limit: int) -> list[dict]:
    seen = []
    for author in authors:
        listing = get(f"models?author={author}&limit={limit}&full=true&cardData=true&sort=downloads") or []
        for info in listing:
            tags = info.get("tags", [])
            fmt = "gguf" if ("gguf" in tags or info["id"].lower().endswith("gguf")) else ("mlx" if "mlx" in tags else None)
            if fmt in formats and not EXCLUDE.search(info["id"]):
                info["_format"] = fmt
                seen.append(info)
    return seen


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--output", type=Path, default=Path("shared/models/catalog.json"))
    parser.add_argument("--formats", default="gguf,mlx")
    parser.add_argument("--limit", type=int, default=400, help="listing size per publisher")
    parser.add_argument("--max-resident-gb", type=float, default=80.0)
    parser.add_argument("--cache", type=Path, help="directory caching Hugging Face API responses")
    parser.add_argument("--verbose", action="store_true", help="print every rejected repository and why")
    args = parser.parse_args()
    global CACHE
    if args.cache:
        args.cache.mkdir(parents=True, exist_ok=True)
        CACHE = args.cache

    formats = set(args.formats.split(","))
    upstream_cache: dict = {}
    chosen: dict[tuple[str, str, str], tuple[int, dict]] = {}
    rejected: dict[str, int] = {}
    def reject(reason: str, repo: str, detail: str = "") -> None:
        rejected[reason] = rejected.get(reason, 0) + 1
        if args.verbose:
            print(f"reject {repo}: {reason} {detail}".rstrip(), file=sys.stderr)

    for info in discover(PUBLISHERS, formats, args.limit):
        chain = lineage(info["id"], upstream_cache, info=info)
        # The model is the original release this conversion was made from;
        # deeper ancestors are its backbone. Every declared upstream license
        # must be allowed, and the conversion may not claim a different one.
        base_repo, base_info = upstream_of(chain)
        if base_repo == info["id"] and model_name(base_repo) != base_repo.split("/", 1)[1]:
            original = f"{base_repo.split('/', 1)[0]}/{model_name(base_repo)}"
            original_info = upstream_cache.setdefault(original, get(f"models/{original}") or {})
            if original_info:
                base_repo, base_info = original, original_info
        org = chain[-1][0].split("/", 1)[0]
        upstream_licenses = {license_of(item) for _, item in chain[1:]} - {None}
        license_key = license_of(base_info) or license_of(chain[-1][1]) or license_of(info)
        if org not in UPSTREAM_ORGS or base_repo.split("/", 1)[0] not in UPSTREAM_ORGS | set(PUBLISHERS):
            reject("untrusted upstream", info["id"])
            continue
        restricted = restricted_reason(info["id"], base_repo, *(repo for repo, _ in chain))
        if restricted:
            reject("denylisted", info["id"], restricted)
            continue
        if (license_key not in ALLOWED_LICENSES or not upstream_licenses <= set(ALLOWED_LICENSES)
                or license_of(info) not in (None, license_key, "other")
                or any(restricted_license_reference(item) for _, item in license_chain(chain))
                or (base_info and restricted_license_reference(base_info))):
            reject("license", info["id"])
            continue
        gemma = gemma_terms_error(model_name(base_repo), info["id"], ALLOWED_LICENSES[license_key], license_key == "gemma")
        if gemma:
            reject("license", info["id"], gemma)
            continue
        problem = identity_problem(chain, base_repo, base_info)
        if problem:
            reject("identity mismatch", info["id"], problem)
            continue
        if EXCLUDE.search(base_repo):
            reject("excluded variant", info["id"])
            continue
        if not_language_model(info, base_repo, base_info):
            reject("not a language model", info["id"])
            continue
        rank = (PUBLISHERS.index(info["id"].split("/", 1)[0]), mlx_quant_rank(info["id"]) if info["_format"] == "mlx" else 0)
        key = (base_repo, info["_format"])
        if key in chosen and chosen[key][0] <= rank:
            continue
        builder = build_gguf_entry if info["_format"] == "gguf" else build_mlx_entry
        entry = builder(info, base_repo, base_info, license_key)
        if not entry or entry["estimatedResidentGB"] > args.max_resident_gb:
            reject("no usable artifact or too large", info["id"])
            continue
        entry["role"] = ROLE_OVERRIDES.get(info["id"]) or ROLE_OVERRIDES.get(base_repo, entry["role"])
        chosen[key] = (rank, entry)

    entries = sorted((entry for _, entry in chosen.values()), key=lambda e: (e["role"], e["format"], e["estimatedResidentGB"], e["id"]))
    ids = set()
    for entry in entries:
        while entry["id"] in ids:
            entry["id"] += "_alt"
        ids.add(entry["id"])
    catalog = {
        "comment": "Generated by shared/scripts/build_model_catalog.py. Extended, opt-in local models; the curated defaults stay in manifest.json. Every file is pinned to an immutable revision and SHA256. status=candidate until fileid-bench verifies the entry on real hardware.",
        "schemaVersion": 1,
        "generatedAt": time.strftime("%Y-%m-%d"),
        "licensePolicy": {"allowed": sorted(set(ALLOWED_LICENSES.values())), "termsRequired": ["Gemma"]},
        "entries": entries,
    }
    # The release gate's checks run on the output before it replaces the
    # catalog, so a bad entry fails the build instead of shipping.
    with tempfile.TemporaryDirectory() as temporary:
        candidate = Path(temporary) / "catalog.json"
        candidate.write_text(json.dumps(catalog, indent=1, sort_keys=False) + "\n")
        errors = catalog_errors(candidate)
        if errors:
            for error in errors:
                print(f"ERROR: {error}", file=sys.stderr)
            return 1
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(candidate.read_text())
    by_role: dict[str, int] = {}
    for entry in entries:
        by_role[f"{entry['format']}/{entry['role']}"] = by_role.get(f"{entry['format']}/{entry['role']}", 0) + 1
    print(f"wrote {len(entries)} entries to {args.output}: {by_role}; rejected {rejected}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
