#!/usr/bin/env python3
"""Release gate: shipped engine downloads must remain Hugging Face-only."""

from __future__ import annotations

import argparse
import re
from pathlib import Path
from urllib.parse import urlparse

APPROVED_HOSTS = {"huggingface.co", "hf.co"}
KNOWN_EXTRA_HOSTS = {
    "github.com", "githubusercontent.com", "download.nvidia.com", "developer.nvidia.com"
}
KNOWN_OFF_POLICY_URLS = {
    "https://github.com/ggml-org/llama.cpp/releases/download/b9254/llama-b9254-bin-win-vulkan-x64.zip",
    "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.0/whisper-bin-x64.zip",
    "https://developer.download.nvidia.com/compute/cudnn/redist/cudnn/windows-x86_64/cudnn-windows-x86_64-9.5.1.17_cuda12-archive.zip",
    "https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-win-x64-gpu-1.22.0.zip",
    "https://github.com/ggml-org/llama.cpp/releases/download/b9254/llama-b9254-bin-win-cuda-12.4-x64.zip",
    "https://github.com/ggml-org/llama.cpp/releases/download/b9254/cudart-llama-bin-win-cuda-12.4-x64.zip",
}
ALLOWLIST_RE = re.compile(
    r"const ALLOWED_DOWNLOAD_HOSTS:\s*&\[&str\]\s*=\s*&\[(.*?)\];", re.DOTALL
)
STRING_RE = re.compile(r'"([^"]+)"')
BLOCK_COMMENT_RE = re.compile(r"/\*.*?\*/", re.DOTALL)
EXPECTED_INITIAL_PREDICATE = """
fn download_url_allowed(url: &str) -> bool {
    match reqwest::Url::parse(url) {
        Ok(u) => {
            u.scheme() == "https"
                && u.host_str().is_some_and(|h| {
                    ALLOWED_DOWNLOAD_HOSTS
                        .iter()
                        .any(|d| h == *d || h.ends_with(&format!(".{d}")))
                })
        }
        Err(_) => false,
    }
}
"""
EXPECTED_INITIAL_GUARD = """
if !download_url_allowed(&request.url) {
    let host = reqwest::Url::parse(&request.url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "<unparseable>".to_string());
    anyhow::bail!("refusing to download: host '{host}' is not on the https egress allowlist");
}
"""
EXPECTED_REDIRECT_POLICY = """
let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {
    if attempt.previous().len() >= 10 {
        return attempt.stop();
    }
    if attempt.url().scheme() != "https" {
        return attempt.stop();
    }
    match attempt.url().host_str() {
        Some(h)
            if ALLOWED_DOWNLOAD_HOSTS
                .iter()
                .any(|d| h == *d || h.ends_with(&format!(".{d}"))) =>
        {
            attempt.follow()
        }
        _ => attempt.stop(),
    }
});
"""


def _approved(host: str | None) -> bool:
    if host is None:
        return False
    host = host.lower().rstrip(".")
    return any(host == approved or host.endswith(f".{approved}") for approved in APPROVED_HOSTS)


def _without_comments(text: str) -> str:
    text = BLOCK_COMMENT_RE.sub("", text)
    return "\n".join(
        line for line in text.splitlines()
        if not line.lstrip().startswith("//")
    )


def _normalized(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip()


def _braced_expression(text: str, marker: str, suffix: str = "") -> str | None:
    start = text.find(marker)
    if start < 0:
        return None
    opening = text.find("{", start + len(marker))
    if opening < 0:
        return None
    depth = 0
    quote: str | None = None
    escaped = False
    for index in range(opening, len(text)):
        char = text[index]
        if quote is not None:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            continue
        if char in {'"', "'"}:
            quote = char
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                end = index + 1
                if suffix:
                    suffix_index = text.find(suffix, end)
                    if suffix_index < 0:
                        return None
                    end = suffix_index + len(suffix)
                return text[start:end]
    return None


def _file_entry_blocks(text: str) -> tuple[list[str], list[str]]:
    failures: list[str] = []
    blocks: list[str] = []
    cursor = 0
    pattern = re.compile(r"\b(?:FileEntry|ModelFile)\s*\{")
    while match := pattern.search(text, cursor):
        prefix = text[max(0, match.start() - 12):match.start()]
        cursor = match.end()
        if re.search(r"struct\s+$", prefix):
            continue
        marker = "ModelFile" if text[match.start():].startswith("ModelFile") else "FileEntry"
        block = _braced_expression(text[match.start():], marker)
        if block is None:
            failures.append("unterminated FileEntry construction")
            continue
        blocks.append(block)
        cursor = match.start() + len(block)
    return blocks, failures


def analyze(registry: Path, downloader: Path) -> tuple[list[str], set[str], set[str]]:
    failures: list[str] = []
    registry_text = _without_comments(
        registry.read_text(encoding="utf-8").split("#[cfg(test)]", 1)[0]
    )
    urls: set[str] = set()
    blocks, block_failures = _file_entry_blocks(registry_text)
    failures.extend(f"{registry}: {failure}" for failure in block_failures)
    for index, block in enumerate(blocks, 1):
        matches = re.findall(r'\burl:\s*"(https?://[^"\\]+)"', block)
        if len(matches) != 1:
            failures.append(
                f"{registry}: FileEntry construction {index} must contain exactly one literal http(s) url field"
            )
            continue
        urls.add(matches[0])
    if not blocks or not urls:
        failures.append(f"{registry}: no production FileEntry URL constructions found")

    downloader_text = _without_comments(downloader.read_text(encoding="utf-8"))
    allowlists = list(ALLOWLIST_RE.finditer(downloader_text))
    hosts: set[str] = set()
    if len(allowlists) != 1:
        failures.append(
            f"{downloader}: expected exactly one ALLOWED_DOWNLOAD_HOSTS definition; found {len(allowlists)}"
        )
    else:
        hosts = set(STRING_RE.findall(allowlists[0].group(1)))

    predicate = _braced_expression(downloader_text, "fn download_url_allowed")
    if predicate is None or _normalized(predicate) != _normalized(EXPECTED_INITIAL_PREDICATE):
        failures.append(f"{downloader}: initial URL predicate differs from the reviewed fail-closed contract")
    redirect = _braced_expression(
        downloader_text, "let redirect_policy = reqwest::redirect::Policy::custom", suffix=");"
    )
    if redirect is None or _normalized(redirect) != _normalized(EXPECTED_REDIRECT_POLICY):
        failures.append(f"{downloader}: redirect policy differs from the reviewed fail-closed contract")
    if "reqwest::redirect::Policy::limited" in downloader_text:
        failures.append(f"{downloader}: unrestricted limited redirect policy is forbidden")
    guard = _normalized(EXPECTED_INITIAL_GUARD)
    for function_name, first_request in (
        ("download_simple", "client.get(&request.url)"),
        ("download_parallel", "client.head(&request.url)"),
    ):
        function = _braced_expression(downloader_text, f"pub async fn {function_name}")
        if function is None:
            failures.append(f"{downloader}: {function_name} entry point not found")
            continue
        opening = function.find("{")
        body = _normalized(function[opening + 1:-1]) if opening >= 0 else ""
        if body.count(guard) != 1 or not body.startswith(guard):
            failures.append(
                f"{downloader}: {function_name} must begin with the reviewed initial URL rejection guard"
            )
            continue
        if first_request in function and function.index("if !download_url_allowed(&request.url)") > function.index(first_request):
            failures.append(f"{downloader}: {function_name} performs network I/O before URL rejection")
    guard_count = downloader_text.count("if !download_url_allowed(&request.url)")
    if guard_count != 2:
        failures.append(f"{downloader}: initial URL guard must appear only in both download entry points")
    return failures, urls, hosts


def violations(registry: Path, downloader: Path) -> list[str]:
    failures, urls, hosts = analyze(registry, downloader)
    for url in sorted(urls):
        parsed = urlparse(url)
        if parsed.scheme != "https" or not _approved(parsed.hostname):
            failures.append(f"{registry}: non-Hugging-Face runtime URL: {url}")
    if hosts != APPROVED_HOSTS:
        failures.append(
            f"{downloader}: download host allowlist must be exactly {sorted(APPROVED_HOSTS)}; got {sorted(hosts)}"
        )
    return failures


def known_blocker_violations(registry: Path, downloader: Path) -> list[str]:
    failures, urls, hosts = analyze(registry, downloader)
    off_policy = {url for url in urls if not _approved(urlparse(url).hostname)}
    if off_policy != KNOWN_OFF_POLICY_URLS:
        failures.append(
            "off-policy runtime URL baseline changed; mirror removals require updating the reviewed baseline, "
            f"additions are forbidden: got {sorted(off_policy)}"
        )
    extras = hosts - APPROVED_HOSTS
    if extras != KNOWN_EXTRA_HOSTS or not APPROVED_HOSTS.issubset(hosts):
        failures.append(
            f"runtime host baseline changed: approved={sorted(APPROVED_HOSTS)}, extras={sorted(extras)}"
        )
    return failures


def release_wiring_violations(release_workflow: Path) -> list[str]:
    text = release_workflow.read_text(encoding="utf-8")
    step = (
        "      - name: Enforce Hugging Face-only runtime egress before publication\n"
        "        if: steps.mode.outputs.publish == 'true'\n"
        "        shell: pwsh\n"
        "        run: python ../../shared/scripts/check_runtime_egress.py\n"
    )
    if text.count(step) != 1:
        return [f"{release_workflow}: exact output-gated runtime egress step is missing or duplicated"]
    if text.index(step) > text.index("      - name: Stage release-ready assets"):
        return [f"{release_workflow}: runtime egress gate must run before release asset staging"]
    return []


def main() -> int:
    parser = argparse.ArgumentParser()
    root = Path(__file__).resolve().parents[2]
    parser.add_argument(
        "--registry", type=Path,
        default=root / "platforms" / "windows" / "src" / "engine" / "src" / "models" / "registry.rs",
    )
    parser.add_argument(
        "--downloader", type=Path,
        default=root / "platforms" / "windows" / "src" / "engine" / "src" / "downloader.rs",
    )
    parser.add_argument(
        "--release-workflow", type=Path,
        default=root / ".github" / "workflows" / "release.yml",
    )
    parser.add_argument(
        "--known-blockers", action="store_true",
        help="CI audit mode: permit only the exact reviewed off-policy baseline while mirrors are pending",
    )
    args = parser.parse_args()
    check = known_blocker_violations if args.known_blockers else violations
    failures = check(args.registry, args.downloader)
    failures.extend(release_wiring_violations(args.release_workflow))
    if failures:
        print("Runtime egress release gate failed:")
        for failure in failures:
            print(f"  {failure}")
        return 1
    if args.known_blockers:
        print("Runtime egress audit passed: no additions beyond the reviewed release-blocking baseline.")
    else:
        print("Runtime egress release gate passed: registry and redirects are Hugging Face-only.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
