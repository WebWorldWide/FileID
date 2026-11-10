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
    "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VideoConversionWorker.swift",
    "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VideoFrameWorker.swift",
    "platforms/windows/src/engine/src/downloader.rs",
    "platforms/windows/src/engine/src/models/vlm_server.rs",
    "platforms/windows/src/engine/src/commands/prewarm.rs",
    "platforms/windows/src/engine/src/commands/bulk.rs",
    "platforms/windows/src/engine/src/commands/video_export.rs",
    "platforms/windows/src/engine/src/server.rs",
    "platforms/apple/shared/Sources/FileIDShared/StreamingDownload.swift",
    "platforms/apple/shared/Sources/FileIDShared/TLSPinning.swift",
    "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VLMDownloader.swift",
    "platforms/apple/app/Sources/FileID/Database/ThumbnailService.swift",
    "platforms/apple/shared/Sources/FileIDShared/CLIPTokenizer.swift",
    "platforms/apple/shared/Sources/FileIDShared/ModelLicenseAcceptance.swift",
    "platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyze.swift",
    "platforms/apple/engine/Sources/FileIDEngine/Models/WordPieceTokenizer.swift",
    "platforms/apple/engine/Sources/FileIDEngine/Models/RamPlusService.swift",
    "platforms/apple/app/Sources/FileID/EngineClient.swift",
    "platforms/apple/app/Sources/FileID/Services/CLIPModelInstaller.swift",
    "platforms/apple/engine/Sources/FileIDEngine/Pipeline/DocText.swift",
    "platforms/cli/src/runtime.rs",
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
  "platforms/windows/src/engine/src/client.rs": "7fd2ec8192f167c89322230f17a3e304802a7133f445c5b8bc2ec0985e8a08f4",
  "platforms/windows/src/engine/src/shell/thumbnail.rs": "b45ae1e8953defb4d2dbc73687f61fb942d77758666824f3493cc5c7fcff6b4f",
  "platforms/windows/src/engine/src/models/native_process.rs": "495e360aab90eb0c42c78de1bdd87e0ecf87c35b5deaac01522a1a2c6a9674c7",
  "platforms/windows/src/engine/src/commands/deep_scope.rs": "fa983d18f910b15e830adc64f18040039f49d06301607c622fc0f61c38ae51f7",
  "platforms/windows/src/engine/src/commands/video_export.rs": "fae5c04d0f4bf1c2e73c48ebe472b2ea0097cee481e37843939c07bb32813e01",
  "platforms/apple/app/Sources/FileID/Database/ThumbnailService.swift": "0a564f7125845b2ed697fb64f8a2d78d2968a5652ba02d6fa47067bfb3b32a87",
  "platforms/apple/app/Sources/FileID/EngineClient.swift": "f866922044a0278268c11089481e87a9f79782d330ea8e7308fa739731358cb7",
  "platforms/apple/app/Sources/FileID/Services/CLIPModelInstaller.swift": "5ea71c5cdb780ee61ab6fb865887be963dc8c2e8b24d1eaf0583e4d792e6b0e8",
  "platforms/apple/engine/Sources/FileIDEngine/Models/RamPlusService.swift": "c21da9887868e00c68f94e9aeb594511626709a76268b63c231d34ec9493453c",
  "platforms/apple/engine/Sources/FileIDEngine/Models/WordPieceTokenizer.swift": "dc6292da096dbf4e75acf33394da13fe9811ec16067142a9582ab98fcd7b1668",
  "platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyze.swift": "f7809c1d0ad7d98de0314d4c0645dd8e0dd697cc43554a541bdd9ef4e5c6b8eb",
  "platforms/apple/engine/Sources/FileIDEngine/Pipeline/DocText.swift": "59d2fdaa2f53f55ddca503e585d16411ceac7cec376d0d74d236fa25cd9dbd28",
  "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VLMDownloader.swift": "1e18c99a52e9e4caefe6cd17431f573c99d1a81c00cb2a963d2f1eb4ee69e761",
  "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VideoConversionWorker.swift": "00fecf9ed22732df4b5410313007970bb655849f0d6c8bd3910f92f0c0aedf2a",
  "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VideoFrameWorker.swift": "ac78fb243c41ad79e39a337456a50ba72629082f3d6c8f7f76309c72c81e1f96",
  "platforms/apple/shared/Sources/FileIDShared/CLIPTokenizer.swift": "cd8639c15375f192d89756dc509dcc8308c30e70e55926baa4c237c29e4d6d50",
  "platforms/apple/shared/Sources/FileIDShared/ModelLicenseAcceptance.swift": "371c99d32a06dee16f7f80a1f3841fdf3307b8bce2f9112313a225448335329a",
  "platforms/apple/shared/Sources/FileIDShared/StreamingDownload.swift": "8220ffe36dcf6fd60c9afa9ab203e926b7a4e5a8ab2d18e7e0fcc6ce2a4b34ca",
  "platforms/apple/shared/Sources/FileIDShared/TLSPinning.swift": "36076307e429e1589a1b0d9be947c8743c92d2b4aa49298668e1070509ffead6",
  "platforms/cli/src/runtime.rs": "e86dcc98ce5482b51f23a79b59d567319c1a22d72cae535c7eaeca91e73346af",
  "platforms/linux/src/app/src/engine_client.rs": "c62d6561494089f4787c338c62840b8e5bf0734c04c1448b9b614bb517f698a2",
  "platforms/linux/src/app/src/tabs/settings.rs": "a900a39521fddd742b7a39818bfb9fa3aff10232ca7a89c1d04c539c5b9ff10f",
  "platforms/windows/src/FileID.App/App.xaml.cs": "fec87e4fc50ba035defeb6ff7517859cd3f162e3a1dd7728696637abb487b4c5",
  "platforms/windows/src/FileID.App/MainWindow.xaml.cs": "771e0a1e5a9c776faf8d5237b067c8bc4a8f1ee2b46f1a0db412bd741af2d01e",
  "platforms/windows/src/FileID.App/Program.cs": "e013f7f02ad596093072b50200555b1c63ce89f6e41dddd28a2f57e01dc28e47",
  "platforms/windows/src/FileID.App/Services/FolderPickerService.cs": "b23698d76e9f5bf37117477b9d541fb8577e54c1e1bb59a709225064ce78c7c0",
  "platforms/windows/src/FileID.App/Services/SafeOpen.cs": "48373698174ed4c43575b05e85a563fd7485207819d9737c4fed9f92e0b2dd44",
  "platforms/windows/src/FileID.App/Services/WinVerifyTrustChecker.cs": "3193b3308a99d19f10479ae9d1485c088a55880b6862161213ec8d1415c86731",
  "platforms/windows/src/FileID.App/ViewModels/EngineClient.Commands.cs": "c3f2074e3cbeb4e0fa9f495f895d8a39748ca530ab6abd854349f17a596416cc",
  "platforms/windows/src/FileID.App/ViewModels/EngineClient.cs": "c417280dcbd93d8705456b887b8eb89476f1f40dd0b86abb9ef6d827a103636e",
  "platforms/windows/src/FileID.App/Views/Settings/SettingsView.xaml.cs": "142a2d22dc127043e2e658e559d39f77fcd9101e5549aeec53d9883bc3740e21",
  "platforms/windows/src/FileID.App/Views/Sidebar/SidebarProcessingControl.xaml.cs": "71797eb7ddc4944cafa74210618a2c37deffb167b2493ad8afa78a54d8f95400",
  "platforms/windows/src/engine/src/commands/bulk.rs": "78f80ce07fca49d86c2a6a4e6c3b23285a46bbd968a1ae5f0ca55f06ca978ef6",
  "platforms/windows/src/engine/src/commands/prewarm.rs": "ea0c1ce27ccc5bfdd54ecf0c6e16425f19a60716dd81f1451c11edbdf6b508ee",
  "platforms/windows/src/engine/src/commands/restructure.rs": "a026f31dd15f33cf1b3f96190c1e68cef410f654d0ab3d697e9dde5fe0b099b8",
  "platforms/windows/src/engine/src/commands/trash.rs": "f41c2162dbb7a5473b4bd304dd8cf1b641da17e3598a6d83737fe04e507c98ac",
  "platforms/windows/src/engine/src/downloader.rs": "5195c88965e11864e11078e59051f44bdab37a8ad7ca0a9c559bde409b4d12a4",
  "platforms/windows/src/engine/src/server.rs": "8516ccaf2167e3b82ed58fe5d18ea243eb410939266cad0933b09ce48829f90b",
  "platforms/windows/src/engine/src/models/runtime.rs": "bdb502c03ff9cc22c2dd7953a8ab29dc1a812ef1a1b850c5cdc1d05c9e70b236",
  "platforms/windows/src/engine/src/models/vlm.rs": "da06866186668cb34ef298152cf4d4d2bad7fd0ede656313c43fa59d5967444f",
  "platforms/windows/src/engine/src/models/vlm_server.rs": "93ef5e06c33f8f97b44116785053ca2460028ae91bc199f6bea0eb1fad3cbf6d",
  "platforms/windows/src/engine/src/pipeline/deep_analyze.rs": "4495e266f81959e85dda6e68dc5d8bf5b2fb5eccd3ffcfa5645aaf8ede6ffd0a",
  "platforms/windows/src/engine/src/pipeline/doc_extract.rs": "be3c426f5411a60ca7a91cc9387e578432aef5e9b9d932978fd12f1be46a41bd",
  "platforms/windows/src/engine/src/pipeline/restructure_apply.rs": "9c25dd79fbf34123e32a40bf99d0aa5e90ba6b18e14d1c5c7ef2deab597886db",
  "platforms/windows/src/engine/src/platform.rs": "e2103002ec874afd8a2efbd76a7b09547ec7df010fe7159bf33b0bb60d30679e",
  "platforms/windows/src/engine/src/shell/heic.rs": "890bce8f247d2da9ddd21d544efd5b34f39ec12370c6b420cd5e579f42f00862",
  "platforms/windows/src/engine/src/shell/mod.rs": "c9a949728b1f1bf1d4db8160a72c696aee07f1f8a3a0ecd588069907cd73b3d1",
  "platforms/windows/src/engine/src/shell/ocr.rs": "30e431e99cfd34a81ba7af52d5d07712d8fce52e113c25790e12bc022bb29395",
  "platforms/windows/src/engine/src/shell/tags.rs": "5ab33e5ca6ed86bf8a7c719568443c502347ff093cb1e7d3374b7719a778fe39",
  "platforms/windows/src/engine/src/shell/trash.rs": "959c0063983706ee8503970196a2748648b3ce8176887c6aa0fe73302d797caa",
  "platforms/windows/src/engine/src/shell/video.rs": "555ab67e480a936e8f1aed95e6c1fc463405ac99626f618c01240d064d759214",
  "platforms/windows/src/engine/src/util/content_hash.rs": "dd1893ffa5e67243b1584f5d43879348ce46558b6d06ac846769a58ad0efe302",
  "platforms/windows/src/engine/src/util/path_safety.rs": "92ead87b750e0725dda4c997a8988cb43831d05542f97bcd0f6d9d1e099ad9a1",
  "platforms/windows/src/engine/src/apple/ffi.rs": "1e63b283f1610d78e6a23435930b621ae79fc4f7e1e8c960a50be7d498d8ed40",
}
SAFE_NETWORK_CALLER_FILES = {
    "platforms/windows/src/engine/src/downloader.rs",
    "platforms/windows/src/engine/src/commands/prewarm.rs",
    "platforms/cli/src/runtime.rs",
    "platforms/apple/shared/Sources/FileIDShared/StreamingDownload.swift",
    "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VLMDownloader.swift",
    "platforms/apple/app/Sources/FileID/Services/CLIPModelInstaller.swift",
    "platforms/apple/app/Sources/FileID/Services/ModelInstaller.swift",
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
    ".rs": re.compile(r"\b(?:download_file_blocking|download_simple|download_parallel)\s*\("),
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


def _swift_transport_violations(root: Path) -> list[str]:
    failures: list[str] = []
    streaming_path = root / "platforms/apple/shared/Sources/FileIDShared/StreamingDownload.swift"
    pinning_path = root / "platforms/apple/shared/Sources/FileIDShared/TLSPinning.swift"
    vlm_path = root / "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VLMDownloader.swift"
    if not all(path.is_file() for path in (streaming_path, pinning_path, vlm_path)):
        return ["Apple reviewed network transport files are missing"]
    streaming_text = streaming_path.read_text(encoding="utf-8")
    streaming = _without_comments(streaming_text)
    streaming_code = _mask_comments_and_strings(streaming_text)
    expected_inventory = {
        r"URLSession\s*\(": 3,
        r"URLRequest\s*\(": 2,
        r"\.downloadTask\s*\(": 2,
        r"\.data\s*\(": 1,
    }
    if re.search(r"\.(?:dataTask|uploadTask|streamTask|webSocketTask|bytes)\s*\(", streaming_code) \
            or "URLSession.shared" in streaming_code or any(
                len(re.findall(pattern, streaming_code)) != count
                for pattern, count in expected_inventory.items()
            ):
        failures.append(f"{streaming_path}: raw session/request inventory changed")
    guard = "guard TLSPinning.allowsExternalRequest(to: remote) else"
    for marker in ("public func streamingDownload", "public func parallelStreamingDownload"):
        function = _braced_expression(streaming, marker)
        if function is None:
            failures.append(f"{streaming_path}: {marker} not found")
            continue
        if function.count(guard) != 1 or function.index(guard) > function.index("URLSession"):
            failures.append(f"{streaming_path}: {marker} must reject the initial URL before session creation")

    pinning_text = pinning_path.read_text(encoding="utf-8")
    pinning = _without_comments(pinning_text)
    pinning_code = _mask_comments_and_strings(pinning_text)
    if re.search(
        r"URLSession\s*\(|URLSession\.shared|URLRequest\s*\(|"
        r"\.(?:dataTask|downloadTask|uploadTask|streamTask|webSocketTask|data|bytes)\s*\(",
        pinning_code,
    ):
        failures.append(f"{pinning_path}: policy module must not construct sessions or requests")
    host_match = re.search(
        r"public static let externalDownloadHosts:\s*\[String\]\s*=\s*\[(.*?)\]",
        pinning,
        re.DOTALL,
    )
    hosts = set(STRING_RE.findall(host_match.group(1))) if host_match else set()
    expected_hosts = {"huggingface.co", "*.huggingface.co", "*.hf.co"}
    if hosts != expected_hosts:
        failures.append(f"{pinning_path}: external transport hosts must be exactly {sorted(expected_hosts)}")
    redirect = _braced_expression(pinning, "public static func allowsRedirect")
    if redirect is None or "allowsExternalRequest(to: url)" not in redirect:
        failures.append(f"{pinning_path}: redirects must reuse the initial external URL policy")
    delegate_redirect = _braced_expression(pinning, "willPerformHTTPRedirection")
    if delegate_redirect is None or not all(
        value in delegate_redirect
        for value in ("TLSPinning.maxRedirects", "TLSPinning.allowsRedirect(to: request.url)")
    ):
        failures.append(f"{pinning_path}: tree-listing delegate lacks reviewed redirect enforcement")

    vlm_text = vlm_path.read_text(encoding="utf-8")
    vlm = _without_comments(vlm_text)
    vlm_code = _mask_comments_and_strings(vlm_text)
    if re.search(r"\.(?:dataTask|downloadTask|uploadTask|streamTask|webSocketTask|bytes)\s*\(", vlm_code) \
            or "URLSession.shared" in vlm_code \
            or len(re.findall(r"URLSession\s*\(", vlm_code)) != 1 \
            or len(re.findall(r"URLRequest\s*\(", vlm_code)) != 1 \
            or len(re.findall(r"\.data\s*\(", vlm_code)) != 1 \
            or _swift_url_loader_sites(vlm_code) != 1:
        failures.append(f"{vlm_path}: raw tree-listing request inventory changed")
    listing = _braced_expression(vlm, "private func listRepoFiles")
    vlm_guard = "guard TLSPinning.allowsExternalRequest(to: url) else"
    if listing is None or vlm_guard not in listing or listing.index(vlm_guard) > listing.index("URLSession"):
        failures.append(f"{vlm_path}: tree listing must reject its initial URL before session creation")
    if listing is None or "pinDelegate.redirectRejected" not in listing:
        failures.append(f"{vlm_path}: tree listing must surface blocked redirects")
    return failures


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
        "platforms/apple/shared/Sources/FileIDShared/ModelLicenseAcceptance.swift": 2,
        "platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyze.swift": 1,
        "platforms/apple/engine/Sources/FileIDEngine/Models/WordPieceTokenizer.swift": 1,
        "platforms/apple/engine/Sources/FileIDEngine/Models/RamPlusService.swift": 3,
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
    failures.extend(_swift_transport_violations(root))
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
