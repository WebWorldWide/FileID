#!/usr/bin/env python3
"""Release gate: shipped engine downloads must remain pinned, licensed and Hugging Face-only."""

from __future__ import annotations

import argparse
import hashlib
import os
import json
import re
import subprocess
from pathlib import Path
from urllib.parse import urlparse

APPROVED_HOSTS = {"huggingface.co", "hf.co"}
KNOWN_EXTRA_HOSTS: set[str] = set()
KNOWN_OFF_POLICY_URLS: set[str] = set()
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

EXCLUDED_SOURCE_PARTS = {
    "target", "obj", ".build", "deriveddata", "tests", "test", "benches", "examples", "scripts"
}
RAW_NETWORK_FILES = {
    "platforms/windows/src/engine/src/client.rs",
    "platforms/windows/src/engine/src/downloader.rs",
    "platforms/windows/src/engine/src/models/vlm_server.rs",
    "platforms/windows/src/engine/src/commands/prewarm.rs",
    "platforms/windows/src/engine/src/commands/bulk.rs",
    "platforms/windows/src/engine/src/commands/video_export.rs",
    "platforms/windows/src/engine/src/server.rs",
    "platforms/apple/app/Sources/FileID/Database/ThumbnailService.swift",
    "platforms/apple/shared/Sources/FileIDShared/CLIPTokenizer.swift",
    "platforms/apple/shared/Sources/FileIDModelInfo/ModelLicenseAcceptance.swift",
    "platforms/apple/shared/Sources/FileIDVLMCore/DeepAnalyze.swift",
    "platforms/apple/app/Sources/FileID/EngineClient.swift",
    "platforms/apple/app/Sources/FileID/Services/CLIPModelInstaller.swift",
    "platforms/linux/src/app/src/engine_client.rs",
    "platforms/linux/src/app/src/tabs/settings.rs",
    "platforms/windows/src/engine/src/commands/trash.rs",
    "platforms/windows/src/engine/src/models/vlm.rs",
    "platforms/windows/src/engine/src/platform.rs",
    "platforms/windows/src/engine/src/shell/mod.rs",
    "platforms/windows/src/FileID.App/Program.cs",
    "platforms/windows/src/FileID.App/Services/SafeOpen.cs",
    "platforms/windows/src/FileID.App/ViewModels/EngineClient.cs",
    "platforms/windows/src/FileID.App/Views/Settings/SettingsView.xaml.cs",
    "platforms/windows/src/FileID.App/Views/Sidebar/SidebarProcessingControl.xaml.cs",
    "platforms/windows/src/FileID.App/App.xaml.cs",
    "platforms/windows/src/FileID.App/MainWindow.xaml.cs",
    "platforms/windows/src/FileID.App/Services/FolderPickerService.cs",
    "platforms/windows/src/FileID.App/Services/WinVerifyTrustChecker.cs",
    "platforms/windows/src/engine/src/models/runtime.rs",
    "platforms/windows/src/engine/src/pipeline/deep_analyze.rs",
    "platforms/windows/src/engine/src/pipeline/doc_extract.rs",
    "platforms/windows/src/engine/src/pipeline/restructure_apply.rs",
    "platforms/windows/src/engine/src/shell/heic.rs",
    "platforms/windows/src/engine/src/shell/ocr.rs",
    "platforms/windows/src/engine/src/shell/tags.rs",
    "platforms/windows/src/engine/src/shell/thumbnail.rs",
    "platforms/windows/src/engine/src/shell/trash.rs",
    "platforms/windows/src/engine/src/shell/video.rs",
    "platforms/windows/src/engine/src/util/content_hash.rs",
    "platforms/windows/src/engine/src/util/path_safety.rs",
    "platforms/windows/src/engine/src/commands/restructure.rs",
    "platforms/windows/src/engine/src/commands/deep_scope.rs",
    "platforms/windows/src/engine/src/models/native_process.rs",
    "platforms/windows/src/FileID.App/ViewModels/EngineClient.Commands.cs",
    "platforms/windows/src/engine/src/apple/ffi.rs",
}
REVIEWED_NETWORK_SOURCE_SHA256 = {
  "platforms/windows/src/engine/src/client.rs": "e583675cc6fcbab385f66f75d2b512b392e11215a9a735ff268ab634a2eeec65",
  "platforms/windows/src/engine/src/shell/thumbnail.rs": "1ccb84569bdd7fc385bcec10d27f5631330edee8694bcf42b8ea5d5070ee784b",
  "platforms/windows/src/engine/src/models/native_process.rs": "c7edc0355b3039a6d09a8951d75454519042f7a6423f4bfc97b26bc526f9d159",
  "platforms/windows/src/engine/src/commands/deep_scope.rs": "49ab265bda91f9c72bdff23211af0fa190a2fa6ffefb92a3807a37b43e067785",
  "platforms/windows/src/engine/src/commands/video_export.rs": "95b83f52ddf1ac261daabe0157a4ba4cece7e53a814bb8398929efc07b190c3d",
  "platforms/apple/app/Sources/FileID/Database/ThumbnailService.swift": "8bc194383e36dc1bd12abc02ca52ae096f178082b9e1ae50bd965bbd146666b6",
  "platforms/apple/app/Sources/FileID/EngineClient.swift": "ba32f5ba1a7c2c501624a45ba8fba2a4c41d9c748b2adab4a56a368e62470fcd",
  "platforms/apple/app/Sources/FileID/Services/CLIPModelInstaller.swift": "0b6c28cd3b5ea50b608cb494defd87be612a26fa8f36e904da18a768501049a2",
  "platforms/apple/shared/Sources/FileIDVLMCore/DeepAnalyze.swift": "e6886afc9abe3caffbf1de191eb3e48975b53f6ae67a056f60d17387af4ad961",
  "platforms/apple/shared/Sources/FileIDShared/CLIPTokenizer.swift": "cd8639c15375f192d89756dc509dcc8308c30e70e55926baa4c237c29e4d6d50",
  "platforms/apple/shared/Sources/FileIDModelInfo/ModelLicenseAcceptance.swift": "371c99d32a06dee16f7f80a1f3841fdf3307b8bce2f9112313a225448335329a",
  "platforms/linux/src/app/src/engine_client.rs": "e69c06e2ad17b937cc8339b367e21d71af8b757332becb718873d80f7f3be8a9",
  "platforms/linux/src/app/src/tabs/settings.rs": "ee11c615f69816907ad1191b8ea7277654a82b46e675233e1f7c9bf56ab24ebb",
  "platforms/windows/src/FileID.App/App.xaml.cs": "b55546f371f14d937f929b613779dfef29eb5a84d094bdf43c7f931d9aeefd77",
  "platforms/windows/src/FileID.App/MainWindow.xaml.cs": "e32485ef2deb0f80eb647a649d0aaaf36cb4a4786a0d6483e165595b89c6065f",
  "platforms/windows/src/FileID.App/Program.cs": "5db35a6fce127071e4b7741dbe6f785196fb3ee651ce8043ca5542f269710dee",
  "platforms/windows/src/FileID.App/Services/FolderPickerService.cs": "a5323c99a0ce6de9c26f69eedc62b516a814fefb9c52c8e4e5ec48e12b3365cf",
  "platforms/windows/src/FileID.App/Services/SafeOpen.cs": "789465f21348bb6af9e1e3f35e132d007e9e8506200961eb75e4e693f999682a",
  "platforms/windows/src/FileID.App/Services/WinVerifyTrustChecker.cs": "6c94e76ce8fa5f41349b415e05ea1c49c8726c3c7f986f2d808beaef9de065df",
  "platforms/windows/src/FileID.App/ViewModels/EngineClient.Commands.cs": "76018241aed653d1746360dd303150793ac58071eed60a23a4195be8acd39b48",
  "platforms/windows/src/FileID.App/ViewModels/EngineClient.cs": "3ac6d13b920f43738ece9fd3e46a0d5a883548a0026d4afc9caeb7d078b53589",
  "platforms/windows/src/FileID.App/Views/Settings/SettingsView.xaml.cs": "109ccb62623af139267a925cac3de559dd1291a51a0dc67ab851848018bb5ac8",
  "platforms/windows/src/FileID.App/Views/Sidebar/SidebarProcessingControl.xaml.cs": "a83ca357f422a3f9bf7e8519618839de899a4a0d20d31cb641e504cd7a2b88e9",
  "platforms/windows/src/engine/src/commands/bulk.rs": "755d03aabfac6d1180fef64c99a9e298f1402aa12fc957fb139eda283c2f9870",
  "platforms/windows/src/engine/src/commands/prewarm.rs": "ece1a4dcb57e00adb721aa193681a812a00b98990cf8c298a6632ff511dd6cf9",
  "platforms/windows/src/engine/src/commands/restructure.rs": "1928d449f9078e187310355c97ec92027cf946a12934771b75b5fea35f85b00c",
  "platforms/windows/src/engine/src/commands/trash.rs": "2d6a0fc4cfd08891e8e71c96ea621e8ebc726c7444d5f823cbb40d9282963457",
  "platforms/windows/src/engine/src/downloader.rs": "6f31f9957d42c63bf4f9e625a51cbc78f78008865c5825923295dedea99446b0",
  "platforms/windows/src/engine/src/server.rs": "2ee7dcb074c432d6305e882ba021e7277fdee859145b05cb69f111538d26202c",
  "platforms/windows/src/engine/src/models/runtime.rs": "fbe9ba8ea0bf70a9853bb86adae1ca365d7e25481e2d8e9674d7ac49c0765dc9",
  "platforms/windows/src/engine/src/models/vlm.rs": "b54cceff68b1d31b3d5eb102ac7654f511c2a08e02d8faa8f94871aac46b706f",
  "platforms/windows/src/engine/src/models/vlm_server.rs": "859f765ca9a0c795c2158a05fdcada14116a73e53365594eb21f779cd0dbc7a1",
  "platforms/windows/src/engine/src/pipeline/deep_analyze.rs": "a5e0f2e939dc670ff3b53559b22c5df9768d7617071837f4bd420e60c8140cc9",
  "platforms/windows/src/engine/src/pipeline/doc_extract.rs": "b615a018b7a432860437f1a31e0c18f92a69d4ec70d00cb3de0e7821077838f2",
  "platforms/windows/src/engine/src/pipeline/restructure_apply.rs": "7100eaa0fdfd2ff2051659a5416e98ac563ccd01760c0e9506fd738a52a3e3b1",
  "platforms/windows/src/engine/src/platform.rs": "339c2e5beb9265a46db2d1d5f98b2dbc2553830cae3c991a7a2c3e643f48444b",
  "platforms/windows/src/engine/src/shell/heic.rs": "747e17bbfd709aa20558018bfa3b13c42107902837229826f79fc01695c961bb",
  "platforms/windows/src/engine/src/shell/mod.rs": "9764c853e6b3c7181bd2cfaac7b1752849d0a75cb83889390459c9acf21697f2",
  "platforms/windows/src/engine/src/shell/ocr.rs": "8360b4a7d6ca7f4bbc676be053647f42004f499f135fe0eabadac2f22d38fc84",
  "platforms/windows/src/engine/src/shell/tags.rs": "4073edaa1b335d19f83151601eff9bcc1c4c3a6f212f1a3956663259729ce0a5",
  "platforms/windows/src/engine/src/shell/trash.rs": "497e9a340919d2c3c0ad6a2bd24dcdc6d34f35ae982efc160e2dab55bc028164",
  "platforms/windows/src/engine/src/shell/video.rs": "a6bae8aaa005092408935c6bb66931fe73cd2830b84e601dac743682f63a5db1",
  "platforms/windows/src/engine/src/util/content_hash.rs": "b35c2305bb50eb24fc08fa79b45291a52c49d39ebf9326be9d96cd92942c0000",
  "platforms/windows/src/engine/src/util/path_safety.rs": "3f07d8b8f3694d5d40032b545c935ef9b18468b874cede7dbcd05151c92c9b45",
  "platforms/windows/src/engine/src/apple/ffi.rs": "b457cf17557bdefb4423f3a3ee33fe96c84850dcc7d0c08c5a853fb2b970cc56",
}
SAFE_NETWORK_CALLER_FILES = {
    "platforms/windows/src/engine/src/downloader.rs",
    "platforms/windows/src/engine/src/commands/prewarm.rs",
}
RAW_NETWORK_PATTERNS = {
    ".rs": re.compile(
        r"\b(?:reqwest\b|std::net::(?:Tcp|Udp)|tokio::net::(?:Tcp|Udp)|"
        r"TcpStream|TcpListener|UdpSocket|ClientWebSocket|"
        r"(?:std|tokio)::process::Command|Command::new|posix_spawn|"
        r"libc::(?:fork|vfork|exec[A-Za-z0-9_]*|system|socket|connect|sendto|getaddrinfo)|"
        r"extern\b|libloading|dlopen|dlsym|windows::Win32::|ort::init_from|"
        r"Pdfium::bind_to_[A-Za-z0-9_]*)"
    ),
    ".swift": re.compile(
        r"\b(?:URLSession|URLRequest|URLSessionTask|NW[A-Z][A-Za-z0-9_]*|"
        r"CFSocket[A-Za-z0-9_]*|CFStream[A-Za-z0-9_]*|CFHost[A-Za-z0-9_]*|"
        r"CFNet[A-Za-z0-9_]*|CFHTTP[A-Za-z0-9_]*|CFReadStream[A-Za-z0-9_]*|"
        r"CFWriteStream[A-Za-z0-9_]*|URLSessionWebSocketTask|Process|NSTask|"
        r"posix_spawn[A-Za-z0-9_]*|fork|exec[lvpe]*|dlopen|dlsym)\b|"
        r"(?<![A-Za-z0-9_.])system\s*\(|(?:Darwin|Glibc)\.system\s*\(|"
        r"\b[A-Z][A-Za-z0-9_.]*(?:\.init)?\s*\(\s*contentsOf\s*:"
    ),
    ".cs": re.compile(
        r"\b(?:HttpClient|HttpRequestMessage|WebClient|WebRequest|TcpClient|"
        r"TcpListener|UdpClient|ClientWebSocket|System\.Net\.Sockets|"
        r"Windows\.Networking|Process|ProcessStartInfo|DllImport|LibraryImport|NativeLibrary)\b"
    ),
}
SAFE_NETWORK_SINK_PATTERNS = {
    ".rs": re.compile(r"\b(?:download_simple|download_parallel|fetch_repo_listing)\s*\("),
    ".swift": re.compile(r"\b(?:streamingDownload|parallelStreamingDownload)\s*\("),
    ".cs": re.compile(r"$^"),
}


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


def _mask_comments_and_strings(text: str) -> str:
    output = list(text)
    index = 0
    block_depth = 0
    while index < len(text):
        if block_depth:
            if text.startswith("/*", index):
                output[index:index + 2] = "  "
                block_depth += 1
                index += 2
            elif text.startswith("*/", index):
                output[index:index + 2] = "  "
                block_depth -= 1
                index += 2
            else:
                if text[index] != "\n":
                    output[index] = " "
                index += 1
            continue
        if text.startswith("//", index):
            end = text.find("\n", index)
            if end < 0:
                end = len(text)
            for offset in range(index, end):
                output[offset] = " "
            index = end
            continue
        if text.startswith("/*", index):
            output[index:index + 2] = "  "
            block_depth = 1
            index += 2
            continue
        raw = re.match(r"(?:br|r)(#*)\"", text[index:])
        if raw:
            delimiter = '"' + raw.group(1)
            end = text.find(delimiter, index + raw.end())
            end = len(text) if end < 0 else end + len(delimiter)
            for offset in range(index, end):
                if text[offset] != "\n":
                    output[offset] = " "
            index = end
            continue
        character = re.match(r"'(?:\\(?:.|u\{[0-9A-Fa-f_]+\})|[^\\'\n])'", text[index:])
        if character:
            end = index + character.end()
            for offset in range(index, end):
                output[offset] = " "
            index = end
            continue
        swift_extended = re.match(r"(#+)\"", text[index:])
        if swift_extended:
            delimiter = '"' + swift_extended.group(1)
            end = text.find(delimiter, index + swift_extended.end())
            end = len(text) if end < 0 else end + len(delimiter)
            for offset in range(index, end):
                if text[offset] != "\n":
                    output[offset] = " "
            index = end
            continue
        if text.startswith('"""', index):
            end = text.find('"""', index + 3)
            end = len(text) if end < 0 else end + 3
            for offset in range(index, end):
                if text[offset] != "\n":
                    output[offset] = " "
            index = end
            continue
        if text[index] == '"' or text.startswith('@"', index):
            start = index
            if text.startswith('@"', index):
                index += 2
                while index < len(text):
                    if text.startswith('""', index):
                        index += 2
                    elif text[index] == '"':
                        index += 1
                        break
                    else:
                        index += 1
            else:
                index += 1
                escaped = False
                while index < len(text):
                    char = text[index]
                    index += 1
                    if escaped:
                        escaped = False
                    elif char == "\\":
                        escaped = True
                    elif char == '"':
                        break
            for offset in range(start, index):
                if text[offset] != "\n":
                    output[offset] = " "
            continue
        index += 1
    return "".join(output)


def _without_rust_test_code(text: str) -> str:
    output = list(text)
    structure = _mask_comments_and_strings(text)
    cursor = 0
    while True:
        marker = structure.find("#[cfg(test)]", cursor)
        if marker < 0:
            break
        opening = structure.find("{", marker)
        semicolon = structure.find(";", marker)
        if opening < 0 or 0 <= semicolon < opening:
            end = semicolon + 1 if semicolon >= 0 else marker + len("#[cfg(test)]")
        else:
            depth = 0
            end = len(structure)
            for index in range(opening, len(structure)):
                if structure[index] == "{":
                    depth += 1
                elif structure[index] == "}":
                    depth -= 1
                    if depth == 0:
                        end = index + 1
                        break
        for index in range(marker, end):
            if output[index] != "\n":
                output[index] = " "
        cursor = end
    return "".join(output)


def _source_code(path: Path, text: str) -> str:
    if path.suffix.lower() == ".rs":
        text = _without_rust_test_code(text)
    return _mask_comments_and_strings(text)


def _swift_url_loader_sites(code: str) -> int:
    loader_types = {"Data", "NSData", "String", "NSString"}
    aliases = [
        (alias, target.rsplit(".", 1)[-1])
        for alias, target in re.findall(
            r"\btypealias\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"
            r"([A-Za-z_][A-Za-z0-9_.]*)\b",
            code,
        )
    ]
    changed = True
    while changed:
        changed = False
        for alias, target in aliases:
            if target in loader_types and alias not in loader_types:
                loader_types.add(alias)
                changed = True
    return sum(
        len(re.findall(rf"\b{re.escape(loader)}(?:\.init)?\s*\(\s*contentsOf\s*:", code))
        for loader in loader_types
    )


def _production_sources(root: Path) -> tuple[list[Path], list[str]]:
    platforms = root / "platforms"
    if not platforms.is_dir():
        return [], ["platforms: production source tree is missing"]
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", "platforms"],
            check=True,
            capture_output=True,
        )
        candidates = [root / os.fsdecode(raw) for raw in result.stdout.split(b"\0") if raw]
    except (OSError, subprocess.CalledProcessError):
        candidates = list(platforms.rglob("*"))
    files = []
    for path in candidates:
        if not path.is_file() or path.suffix.lower() not in RAW_NETWORK_PATTERNS:
            continue
        relative_parts = path.relative_to(root).parts
        if any(part.lower() in EXCLUDED_SOURCE_PARTS for part in relative_parts) or path.stem.endswith(("_test", "_tests")):
            continue
        files.append(path)
    # A symlink to another production source in the tree (the Swift engine's
    # MLX code compiled into FileIDAppleVLM) is checked once, at its target's
    # path, where reviewed digests apply. Any other symlink is checked as a file.
    real = {path.resolve() for path in files if not path.is_symlink()}
    files = [path for path in files if not (path.is_symlink() and path.resolve() in real)]
    return sorted(set(files)), []


def _rust_external_transport_violations(root: Path) -> list[str]:
    path = root / "platforms/windows/src/engine/src/downloader.rs"
    if not path.is_file():
        return [f"{path}: reviewed external transport is missing"]
    code = _source_code(path, path.read_text(encoding="utf-8"))
    request_sites = re.findall(
        r"(?:\b[A-Za-z_][A-Za-z0-9_]*\s*\.|\breqwest::Client::)\s*"
        r"(?:get|head|post|request|put|delete|patch|execute)\s*\(",
        code,
    )
    if code.count("reqwest::Client::builder") != 2 \
            or "reqwest::Client::new" in code \
            or "reqwest::ClientBuilder" in code \
            or len(request_sites) != 4:
        return [f"{path}: raw client/request inventory differs from the reviewed transport"]
    return []


def _rust_loopback_violations(root: Path) -> list[str]:
    path = root / "platforms/windows/src/engine/src/models/vlm_server.rs"
    if not path.is_file():
        return [f"{path}: reviewed loopback transport is missing"]
    raw_text = path.read_text(encoding="utf-8")
    text = _without_comments(raw_text)
    code = _source_code(path, raw_text)
    request_sites = re.findall(
        r"(?:\b[A-Za-z_][A-Za-z0-9_]*\s*\.|\breqwest::Client::)\s*"
        r"(?:get|head|post|request|put|delete|patch|execute)\s*\(",
        code,
    )
    if code.count("reqwest::Client::builder") != 1 \
            or "reqwest::Client::new" in code \
            or code.count("reqwest::ClientBuilder") != 1 \
            or len(request_sites) != 2:
        return [f"{path}: loopback raw client/request inventory changed"]
    required = (
        '.arg("--host")',
        '.arg("127.0.0.1")',
        'format!("http://127.0.0.1:{port}")',
        'std::net::TcpListener::bind("127.0.0.1:0")',
    )
    normalized = _normalized(text)
    wrapper = _braced_expression(code, "fn build_loopback_client()")
    builder = _braced_expression(code, "fn build_loopback_client_with")
    reviewed_builder = (
        "builder.no_proxy().redirect(reqwest::redirect::Policy::none())"
        ".timeout(Duration::from_secs(300)).build()"
    )
    wrapper_compact = re.sub(r"\s+", "", wrapper or "")
    builder_compact = re.sub(r"\s+", "", builder or "")
    contract_changed = any(_normalized(value) not in normalized for value in required) \
        or "build_loopback_client_with(reqwest::Client::builder())" not in wrapper_compact \
        or reviewed_builder not in builder_compact \
        or builder_compact.count(".no_proxy()") != 1 \
        or builder_compact.count(".redirect(reqwest::redirect::Policy::none())") != 1
    return [f"{path}: loopback transport differs from the reviewed no-proxy contract"] \
        if contract_changed else []


def source_boundary_violations(root: Path) -> list[str]:
    root = root.resolve()
    sources, failures = _production_sources(root)
    for relative, expected_digest in REVIEWED_NETWORK_SOURCE_SHA256.items():
        path = root / relative
        if not path.is_file():
            failures.append(f"{relative}: reviewed network-capable source is missing")
            continue
        normalized = path.read_bytes().replace(b"\r\n", b"\n")
        if hashlib.sha256(normalized).hexdigest() != expected_digest:
            failures.append(f"{relative}: reviewed network-capable source digest changed")
    for path in sources:
        relative = path.relative_to(root).as_posix()
        if relative in REVIEWED_NETWORK_SOURCE_SHA256:
            continue
        text = path.read_text(encoding="utf-8")
        raw_pattern = RAW_NETWORK_PATTERNS[path.suffix.lower()]
        sink_pattern = SAFE_NETWORK_SINK_PATTERNS[path.suffix.lower()]
        if not raw_pattern.search(text) \
                and not sink_pattern.search(text) \
                and not (path.suffix.lower() == ".swift" and "contentsOf" in text):
            continue
        code = _source_code(path, text)
        raw_network_present = raw_pattern.search(code) is not None
        if path.suffix.lower() == ".swift" and _swift_url_loader_sites(code) > 0:
            raw_network_present = True
        if raw_network_present and relative not in RAW_NETWORK_FILES:
            failures.append(f"{relative}: raw network API is outside a reviewed transport module")
        if sink_pattern.search(code) and relative not in SAFE_NETWORK_CALLER_FILES:
            failures.append(f"{relative}: network download sink is outside the reviewed caller inventory")
    local_data_inventory = {
        "platforms/apple/app/Sources/FileID/Database/ThumbnailService.swift": 1,
        "platforms/apple/shared/Sources/FileIDShared/CLIPTokenizer.swift": 2,
        "platforms/apple/shared/Sources/FileIDModelInfo/ModelLicenseAcceptance.swift": 2,
        "platforms/apple/shared/Sources/FileIDVLMCore/DeepAnalyze.swift": 1,
    }
    for relative, expected_count in local_data_inventory.items():
        path = root / relative
        if not path.is_file():
            failures.append(f"{relative}: reviewed local Foundation URL-loader file is missing")
            continue
        code = _mask_comments_and_strings(path.read_text(encoding="utf-8"))
        other_raw = re.search(
            r"\b(?:URLSession|URLRequest|URLSessionTask|NWConnection|NWListener|CFSocket|CFStream)\b",
            code,
        )
        if _swift_url_loader_sites(code) != expected_count or other_raw:
            failures.append(f"{relative}: local Foundation URL-loader inventory changed")
    failures.extend(_rust_external_transport_violations(root))
    failures.extend(_rust_loopback_violations(root))
    for relative in (
        "platforms/windows/src/engine/src/commands/prewarm.rs",
        "platforms/windows/src/engine/src/server.rs",
    ):
        plumbing_path = root / relative
        if not plumbing_path.is_file():
            continue
        plumbing = _source_code(plumbing_path, plumbing_path.read_text(encoding="utf-8"))
        if re.search(
            r"reqwest::(?:Client::(?:builder|new|get|head|post|request|put|delete|patch|execute)|ClientBuilder)|\.\s*(?:get|head|post|request|put|delete|patch|execute)\s*\(",
            plumbing,
        ):
            failures.append(
                f"{plumbing_path}: HTTP client plumbing may call reviewed downloader entry points only"
            )
    return failures


def policy_source_wiring_violations(policy_workflow: Path) -> list[str]:
    text = policy_workflow.read_text(encoding="utf-8")
    failures: list[str] = []
    trigger = text.partition("permissions:")[0]
    expected_trigger = (
        "on:\n"
        "  push:\n"
        "    branches: [main]\n"
        "  pull_request:\n"
        "  workflow_dispatch:\n"
    )
    if expected_trigger not in trigger \
            or any(key in trigger for key in ("paths:", "paths-ignore:", "branches-ignore:")):
        failures.append(f"{policy_workflow}: repository policy must run unconditionally on push and pull_request")
    if re.search(r"(?m)^\s+(?:if|continue-on-error|defaults|shell)\s*:", text):
        failures.append(
            f"{policy_workflow}: policy jobs and enforcement steps must not be conditional or failure-masking"
        )
    test_step = "        run: python shared/scripts/test_check_runtime_egress.py\n"
    blocker_step = "        run: python shared/scripts/check_runtime_egress.py --known-blockers\n"
    if text.count(test_step) != 1 or text.count(blocker_step) != 1:
        failures.append(f"{policy_workflow}: runtime egress tests and known-blocker audit must each run once")
    return failures


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


def _braced_expression_structural(
    text: str, marker: str, suffix: str = "", start_at: int = 0
) -> tuple[str | None, int]:
    structure = _mask_comments_and_strings(text)
    start = structure.find(marker, start_at)
    if start < 0:
        return None, start_at
    opening = structure.find("{", start + len(marker))
    if opening < 0:
        return None, start + len(marker)
    depth = 0
    for index in range(opening, len(structure)):
        if structure[index] == "{":
            depth += 1
        elif structure[index] == "}":
            depth -= 1
            if depth == 0:
                end = index + 1
                if suffix:
                    suffix_index = structure.find(suffix, end)
                    if suffix_index < 0:
                        return None, end
                    end = suffix_index + len(suffix)
                return text[start:end], end
    return None, len(text)


def _file_entry_blocks(text: str) -> tuple[list[str], list[str]]:
    failures: list[str] = []
    blocks: list[str] = []
    structure = _mask_comments_and_strings(text)
    cursor = 0
    pattern = re.compile(r"\b(?:FileEntry|ModelFile)\s*\{")
    while match := pattern.search(structure, cursor):
        prefix = structure[max(0, match.start() - 12):match.start()]
        cursor = match.end()
        if re.search(r"struct\s+$", prefix):
            continue
        marker = "ModelFile" if structure[match.start():].startswith("ModelFile") else "FileEntry"
        block, cursor = _braced_expression_structural(text, marker, start_at=match.start())
        if block is None:
            failures.append("unterminated FileEntry construction")
            continue
        blocks.append(block)
    return blocks, failures


CATALOG_FILE_ENTRY_URL = _normalized("url: file.url.clone()")
MANIFEST_FILE_ENTRY_URL = _normalized("url: artifact.url()")
MANIFEST_URL_BUILDER = 'format!("https://huggingface.co/{}/resolve/{}/{}", self.repo, self.revision, self.path)'
CATALOG_URL_RE = re.compile(r"^https://huggingface\.co/([A-Za-z0-9._-]+/[A-Za-z0-9._-]+)/resolve/([0-9a-f]{40})/([^?#]+)$")
NAME_RE = re.compile(r"[A-Za-z0-9._-]+")
SHA256_RE = re.compile(r"[0-9a-f]{64}")
REVISION_RE = re.compile(r"[0-9a-f]{40}")
MANIFEST_SCHEMA_VERSION = 2
PERMITTED_LICENSES = {"Apache-2.0": False, "MIT": False, "Gemma": True}
MANIFEST_KEYS = {"comment", "schemaVersion", "licensePolicies", "artifacts", "vlmRepos"}
ARTIFACT_KEYS = {"id", "kind", "platforms", "repo", "revision", "path", "sha256", "approxBytes", "license"}
VLM_REPO_KEYS = {"repo", "kind", "platforms", "revision", "approxBytes", "license"}
POLICY_KEYS = {"spdx", "licenseUrl", "termsRequired", "reviewedAt"}


def _shared_file_for(registry: Path, name: str) -> Path:
    relative = Path("shared") / "models" / name
    for parent in registry.resolve().parents:
        if (parent / relative).is_file():
            return parent / relative
    return Path(__file__).resolve().parents[2] / relative


def catalog_for(registry: Path) -> Path:
    """The catalog beside the registry's repository, else this script's."""
    return _shared_file_for(registry, "catalog.json")


def manifest_for(registry: Path) -> Path:
    """The curated manifest beside the registry's repository, else this script's."""
    return _shared_file_for(registry, "manifest.json")


def catalog_urls_checked(catalog: Path) -> tuple[list[str], set[str]]:
    """Every catalog download must be an immutable Hugging Face file URL in
    the entry's own repository and revision, with a SHA256 pin."""
    failures: list[str] = []
    urls: set[str] = set()
    try:
        document = json.loads(catalog.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        return [f"{catalog}: unreadable model catalog ({error})"], urls
    for entry in document.get("entries", []):
        for item in entry.get("files", []):
            url = item.get("url", "")
            match = CATALOG_URL_RE.match(url)
            if not match or match.group(1) != entry.get("repo") or match.group(2) != entry.get("revision") or ".." in match.group(3):
                failures.append(f"{catalog}: {entry.get('id')}: catalog URL is not an immutable file in its own repository: {url}")
                continue
            if not re.fullmatch(r"[0-9a-f]{64}", str(item.get("sha256") or "")):
                failures.append(f"{catalog}: {entry.get('id')}: catalog file is not SHA256-pinned: {url}")
                continue
            urls.add(url)
    return failures, urls


def _repo_ok(value: object) -> bool:
    if not isinstance(value, str):
        return False
    parts = value.split("/")
    return len(parts) == 2 and all(NAME_RE.fullmatch(part) and part not in {".", ".."} for part in parts)


def _path_ok(value: object) -> bool:
    if not isinstance(value, str):
        return False
    return all(NAME_RE.fullmatch(part) and part not in {".", ".."} for part in value.split("/"))


def _platforms_ok(value: object) -> bool:
    return isinstance(value, list) and bool(value) and all(isinstance(item, str) and item for item in value)


def manifest_urls_checked(manifest: Path) -> tuple[list[str], set[str]]:
    """Every curated download must be a Hugging Face file at an immutable
    commit revision, with a SHA256 pin and a permitted license. A malformed
    document fails closed."""
    failures: list[str] = []
    urls: set[str] = set()
    try:
        document = json.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        return [f"{manifest}: unreadable model manifest ({error})"], urls
    if not isinstance(document, dict) or set(document) != MANIFEST_KEYS:
        return [f"{manifest}: model manifest must have exactly the keys {sorted(MANIFEST_KEYS)}"], urls
    if document["schemaVersion"] != MANIFEST_SCHEMA_VERSION or isinstance(document["schemaVersion"], bool):
        return [f"{manifest}: model manifest schemaVersion must be {MANIFEST_SCHEMA_VERSION}"], urls
    policies = document["licensePolicies"]
    artifacts = document["artifacts"]
    repos = document["vlmRepos"]
    if not isinstance(policies, dict) or not isinstance(artifacts, list) or not isinstance(repos, list) or not artifacts:
        return [f"{manifest}: model manifest licensePolicies, artifacts and vlmRepos are malformed"], urls
    for key, policy in policies.items():
        if not isinstance(policy, dict) or set(policy) != POLICY_KEYS or not isinstance(policy["termsRequired"], bool):
            failures.append(f"{manifest}: license policy {key!r} is malformed")

    def license_failure(name: str, license_key: object) -> str | None:
        if license_key not in PERMITTED_LICENSES:
            return f"{manifest}: {name}: license {license_key!r} is not permitted"
        policy = policies.get(license_key)
        if not isinstance(policy, dict) or policy.get("termsRequired") is not PERMITTED_LICENSES[license_key]:
            return f"{manifest}: {name}: license {license_key!r} lacks its reviewed policy"
        return None

    def size_ok(value: object) -> bool:
        return isinstance(value, int) and not isinstance(value, bool) and value > 0

    seen_ids: set[str] = set()
    for index, artifact in enumerate(artifacts):
        if not isinstance(artifact, dict) or set(artifact) != ARTIFACT_KEYS:
            failures.append(f"{manifest}: artifact {index} must have exactly the keys {sorted(ARTIFACT_KEYS)}")
            continue
        name = artifact["id"]
        if not isinstance(name, str) or not name or name in seen_ids:
            failures.append(f"{manifest}: artifact {index} has a missing or duplicate id")
            continue
        seen_ids.add(name)
        revision = artifact["revision"]
        if not _repo_ok(artifact["repo"]) or not _path_ok(artifact["path"]) \
                or not isinstance(revision, str) or not NAME_RE.fullmatch(revision) \
                or not isinstance(artifact["kind"], str) or not artifact["kind"] \
                or not _platforms_ok(artifact["platforms"]) or not size_ok(artifact["approxBytes"]):
            failures.append(f"{manifest}: {name}: repo, revision, path, kind, platforms or approxBytes is malformed")
            continue
        url = f"https://huggingface.co/{artifact['repo']}/resolve/{revision}/{artifact['path']}"
        if not isinstance(artifact["sha256"], str) or not SHA256_RE.fullmatch(artifact["sha256"]):
            failures.append(f"{manifest}: {name}: manifest file is not SHA256-pinned: {url}")
            continue
        problem = license_failure(name, artifact["license"])
        if problem:
            failures.append(problem)
            continue
        if not REVISION_RE.fullmatch(revision):
            failures.append(f"{manifest}: {name}: manifest URL is not pinned to an immutable revision: {url}")
            continue
        urls.add(url)
    seen_repos: set[str] = set()
    for index, repo in enumerate(repos):
        if not isinstance(repo, dict) or set(repo) != VLM_REPO_KEYS:
            failures.append(f"{manifest}: vlmRepo {index} must have exactly the keys {sorted(VLM_REPO_KEYS)}")
            continue
        name = repo["repo"]
        if not _repo_ok(name) or name in seen_repos:
            failures.append(f"{manifest}: vlmRepo {index} has a malformed or duplicate repo")
            continue
        seen_repos.add(name)
        if not isinstance(repo["revision"], str) or not REVISION_RE.fullmatch(repo["revision"]):
            failures.append(f"{manifest}: {name}: VLM repo is not pinned to an immutable revision")
            continue
        if not isinstance(repo["kind"], str) or not repo["kind"] \
                or not _platforms_ok(repo["platforms"]) or not size_ok(repo["approxBytes"]):
            failures.append(f"{manifest}: {name}: kind, platforms or approxBytes is malformed")
            continue
        problem = license_failure(name, repo["license"])
        if problem:
            failures.append(problem)
    return failures, urls


def _manifest_builder_failures(builder: Path) -> list[str]:
    if not builder.is_file():
        return [f"{builder}: reviewed manifest URL builder is missing"]
    code = _without_rust_test_code(_without_comments(builder.read_text(encoding="utf-8")))
    if _normalized(code).count(_normalized(MANIFEST_URL_BUILDER)) != 1 or len(re.findall(r"https?://", code)) != 1:
        return [f"{builder}: manifest URL builder differs from the reviewed Hugging Face resolve URL"]
    return []


def analyze(
    registry: Path, downloader: Path, manifest: Path | None = None, catalog: Path | None = None,
) -> tuple[list[str], set[str], set[str]]:
    failures: list[str] = []
    registry_text = _without_comments(
        _without_rust_test_code(registry.read_text(encoding="utf-8"))
    )
    urls: set[str] = set()
    blocks, block_failures = _file_entry_blocks(registry_text)
    failures.extend(f"{registry}: {failure}" for failure in block_failures)
    constructions = {MANIFEST_FILE_ENTRY_URL: 0, CATALOG_FILE_ENTRY_URL: 0}
    for index, block in enumerate(blocks, 1):
        normalized = _normalized(block)
        url_fields = re.findall(r"\burl\s*:", _mask_comments_and_strings(block))
        source = next((source for source in constructions if source in normalized), None)
        if len(url_fields) != 1 or source is None or re.search(r"https?://", block) \
                or normalized.count(source.removeprefix("url: ")) != 1:
            failures.append(
                f"{registry}: FileEntry construction {index} must take its URL from the model manifest or catalog"
            )
            continue
        constructions[source] += 1
    if not constructions[MANIFEST_FILE_ENTRY_URL]:
        failures.append(f"{registry}: no production FileEntry URL constructions found")
    if constructions[MANIFEST_FILE_ENTRY_URL] > 1:
        failures.append(f"{registry}: only one manifest-backed FileEntry construction is reviewed")
    if constructions[CATALOG_FILE_ENTRY_URL] > 1:
        failures.append(f"{registry}: only one catalog-backed FileEntry construction is reviewed")
    if constructions[MANIFEST_FILE_ENTRY_URL]:
        failures.extend(_manifest_builder_failures(registry.with_name("manifest.rs")))
        manifest_failures, manifest_urls = manifest_urls_checked(manifest or manifest_for(registry))
        failures.extend(manifest_failures)
        urls |= manifest_urls
    if constructions[CATALOG_FILE_ENTRY_URL]:
        catalog_failures, catalog_urls = catalog_urls_checked(catalog or catalog_for(registry))
        failures.extend(catalog_failures)
        urls |= catalog_urls

    downloader_text = _without_comments(downloader.read_text(encoding="utf-8"))
    downloader_structure = _mask_comments_and_strings(downloader_text)
    allowlists = list(ALLOWLIST_RE.finditer(downloader_structure))
    hosts: set[str] = set()
    if len(allowlists) != 1:
        failures.append(
            f"{downloader}: expected exactly one ALLOWED_DOWNLOAD_HOSTS definition; found {len(allowlists)}"
        )
    else:
        match = allowlists[0]
        hosts = set(STRING_RE.findall(downloader_text[match.start(1):match.end(1)]))

    predicate, _ = _braced_expression_structural(downloader_text, "fn download_url_allowed")
    if predicate is None or _normalized(predicate) != _normalized(EXPECTED_INITIAL_PREDICATE):
        failures.append(f"{downloader}: initial URL predicate differs from the reviewed fail-closed contract")
    redirect, _ = _braced_expression_structural(
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
        function, _ = _braced_expression_structural(
            downloader_text, f"pub async fn {function_name}"
        )
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
    guard_count = downloader_structure.count("if !download_url_allowed(&request.url)")
    if guard_count != 2:
        failures.append(f"{downloader}: initial URL guard must appear only in both download entry points")
    return failures, urls, hosts


def violations(registry: Path, downloader: Path, manifest: Path | None = None) -> list[str]:
    failures, urls, hosts = analyze(registry, downloader, manifest)
    for url in sorted(urls):
        parsed = urlparse(url)
        if parsed.scheme != "https" or not _approved(parsed.hostname):
            failures.append(f"{registry}: non-Hugging-Face runtime URL: {url}")
    if hosts != APPROVED_HOSTS:
        failures.append(
            f"{downloader}: download host allowlist must be exactly {sorted(APPROVED_HOSTS)}; got {sorted(hosts)}"
        )
    return failures


def known_blocker_violations(registry: Path, downloader: Path, manifest: Path | None = None) -> list[str]:
    failures, urls, hosts = analyze(registry, downloader, manifest)
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
        "      - name: Enforce reviewed runtime egress before publication\n"
        "        if: steps.mode.outputs.publish == 'true'\n"
        "        shell: pwsh\n"
        "        run: python ../../shared/scripts/check_runtime_egress.py --known-blockers\n"
        "\n"
        "      - name: Download signed/non-Windows CLI/TUI bundles\n"
    )
    if text.count(step) != 1 or re.search(r"(?m)^\s+continue-on-error\s*:", text):
        return [f"{release_workflow}: exact blocking output-gated runtime egress step is missing or duplicated"]
    if text.index(step) > text.index("      - name: Stage release-ready assets"):
        return [f"{release_workflow}: runtime egress gate must run before release asset staging"]
    return []


def main() -> int:
    parser = argparse.ArgumentParser()
    root = Path(__file__).resolve().parents[2]
    parser.add_argument("--root", type=Path, default=root)
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
        "--policy-workflow", type=Path,
        default=root / ".github" / "workflows" / "policy.yml",
    )
    parser.add_argument(
        "--known-blockers", action="store_true",
        help="CI audit mode: permit only the exact reviewed off-policy baseline while mirrors are pending",
    )
    args = parser.parse_args()
    check = known_blocker_violations if args.known_blockers else violations
    failures = check(args.registry, args.downloader)
    failures.extend(source_boundary_violations(args.root))
    failures.extend(policy_source_wiring_violations(args.policy_workflow))
    failures.extend(release_wiring_violations(args.release_workflow))
    if failures:
        print("Runtime egress release gate failed:")
        for failure in failures:
            print(f"  {failure}")
        return 1
    if args.known_blockers:
        print("Runtime egress audit passed: no additions beyond the reviewed release-blocking baseline.")
    else:
        print("Runtime egress release gate passed: manifest, catalog and redirects are Hugging Face-only and pinned.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
