#!/usr/bin/env python3
"""Release gate: shipped engine downloads must remain Hugging Face-only."""

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
    "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VideoConversionWorker.swift",
    "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VideoFrameWorker.swift",
    "platforms/windows/src/engine/src/downloader.rs",
    "platforms/windows/src/engine/src/models/vlm_server.rs",
    "platforms/windows/src/engine/src/commands/prewarm.rs",
    "platforms/windows/src/engine/src/commands/bulk.rs",
    "platforms/windows/src/engine/src/commands/video_export.rs",
    "platforms/windows/src/engine/src/main.rs",
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
    "platforms/cli/src/scan_models.rs",
    "platforms/linux/src/app/src/engine_client.rs",
    "platforms/linux/src/app/src/tabs/settings.rs",
    "platforms/tui/src/models.rs",
    "platforms/tui/src/scan.rs",
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
}
REVIEWED_NETWORK_SOURCE_SHA256 = {
  "platforms/windows/src/engine/src/shell/thumbnail.rs": "f88fbb2de5867c2084e043e43cb5fb85e4a80891496984224a243553fd4ef2eb",
  "platforms/windows/src/engine/src/models/native_process.rs": "ab6b887dc2f1500bb80b510f333ddb0fc110a5e6234b6f57502330e67fd21ad3",
  "platforms/windows/src/engine/src/commands/deep_scope.rs": "4dca8812ea40edb2a39925693db2d8496d264b59557d42ecbf216aaff221e59a",
  "platforms/windows/src/engine/src/commands/video_export.rs": "95af5b5609582a784c0234b2cfc5121fee4ca5a5cf6873b9fdf864db795131a9",
  "platforms/apple/app/Sources/FileID/Database/ThumbnailService.swift": "d4e484a6e72adfcce2758f8d120ff3aff381cb82b19c564a1dcae68e2a76bf57",
  "platforms/apple/app/Sources/FileID/EngineClient.swift": "714bfcdec96fd727e32a6ef2a934b348291969da4d99e3674f0101fb02024b08",
  "platforms/apple/app/Sources/FileID/Services/CLIPModelInstaller.swift": "a5b875ceb9c2e827778884c0f41dc1833eab00d1ee01d114ed89899556ce3482",
  "platforms/apple/engine/Sources/FileIDEngine/Models/RamPlusService.swift": "576cd0cde651ba3154c833ce24b414c5dd9b0c85d5992783b9b42e608c35d819",
  "platforms/apple/engine/Sources/FileIDEngine/Models/WordPieceTokenizer.swift": "dc6292da096dbf4e75acf33394da13fe9811ec16067142a9582ab98fcd7b1668",
  "platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyze.swift": "effafa41f6016555ee292fc0c42ce683742769f5b3dcadd9a3a5b5857408215d",
  "platforms/apple/engine/Sources/FileIDEngine/Pipeline/DocText.swift": "b35d92123749d4509347e9474d388674976bf649d549f9261267cecbb6740158",
  "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VLMDownloader.swift": "fcf725de54cd9aa946129672ac3d3f802e03b1f7bdad1a84311df52b6844fafd",
  "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VideoConversionWorker.swift": "508d495b00b199a354cda9d0c9f29c94b5f7408432e372ab0e494eec059b4f1f",
  "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VideoFrameWorker.swift": "e2102256f509eaa3ced33306cf301effe190b1031027912afcc409f521691e28",
  "platforms/apple/shared/Sources/FileIDShared/CLIPTokenizer.swift": "cd8639c15375f192d89756dc509dcc8308c30e70e55926baa4c237c29e4d6d50",
  "platforms/apple/shared/Sources/FileIDShared/ModelLicenseAcceptance.swift": "371c99d32a06dee16f7f80a1f3841fdf3307b8bce2f9112313a225448335329a",
  "platforms/apple/shared/Sources/FileIDShared/StreamingDownload.swift": "a2d6d7bea0e6d024080d84fb708390c64b60cf3ae991d2e7dba7e61dc56010fb",
  "platforms/apple/shared/Sources/FileIDShared/TLSPinning.swift": "36076307e429e1589a1b0d9be947c8743c92d2b4aa49298668e1070509ffead6",
  "platforms/cli/src/runtime.rs": "62af36fc5aaf77502cb633581599779adf80084e4cbc128f14e002c587e045a5",
  "platforms/cli/src/scan_models.rs": "a359191e9124c156bfbb28a445088c1fbd743eb0319a24aeba60cc7c9e7b5318",
  "platforms/linux/src/app/src/engine_client.rs": "f339fb80c3ab97c5fc3d8c40724e2627c890af6ede5b6816832912298cdeaf79",
  "platforms/linux/src/app/src/tabs/settings.rs": "de591bae80fb4025944b4084a0118758041087b1126b74bf148e5b62c3e0a7b2",
  "platforms/tui/src/models.rs": "bc27e7237659b63e42d2f4f8ca9d5d2a83015d849be771395654e250b0754c23",
  "platforms/tui/src/scan.rs": "9a1131f508dab0ceaebf8934188d99a18da38da5d1f147d57fec8b97fe1431f6",
  "platforms/windows/src/FileID.App/App.xaml.cs": "fec87e4fc50ba035defeb6ff7517859cd3f162e3a1dd7728696637abb487b4c5",
  "platforms/windows/src/FileID.App/MainWindow.xaml.cs": "2406f5eff09b8e7d1472b8791f00ddd6b6168b250d56507ef04570a05b38307d",
  "platforms/windows/src/FileID.App/Program.cs": "e013f7f02ad596093072b50200555b1c63ce89f6e41dddd28a2f57e01dc28e47",
  "platforms/windows/src/FileID.App/Services/FolderPickerService.cs": "ba8cbf1070795fb5fb4a2ea5fdaa3f7828c834e9b16783d6c864e70a915e8c79",
  "platforms/windows/src/FileID.App/Services/SafeOpen.cs": "48373698174ed4c43575b05e85a563fd7485207819d9737c4fed9f92e0b2dd44",
  "platforms/windows/src/FileID.App/Services/WinVerifyTrustChecker.cs": "c50846c16a67365d48caa6e6206f4aa291a384ac93b17a1d85923fdc5449f117",
  "platforms/windows/src/FileID.App/ViewModels/EngineClient.Commands.cs": "7a98a1cac07b0089ea8e80a692bb8ddae8750ff422e506f643fe54e0191f2fea",
  "platforms/windows/src/FileID.App/ViewModels/EngineClient.cs": "41a9af4acae4d9e39b00ff51e9cf16629277299304faafd9e474bd041c67e7ee",
  "platforms/windows/src/FileID.App/Views/Settings/SettingsView.xaml.cs": "d126da2898769a375613c9d53e89e908cc2dd3c0c8d700db6d592374e1b5d5fe",
  "platforms/windows/src/FileID.App/Views/Sidebar/SidebarProcessingControl.xaml.cs": "f06fa0c923507f380f64a1b0aedb6951466cf1b33441ecd6ac99fa1c6cf663ca",
  "platforms/windows/src/engine/src/commands/bulk.rs": "acd9f70c831539d27340e94574caac8578eb8efe963d3515d722987788434811",
  "platforms/windows/src/engine/src/commands/prewarm.rs": "d421167edf8b636c7682c5034a6d260981a702e89b2b924573cfa1e5790aecd2",
  "platforms/windows/src/engine/src/commands/restructure.rs": "a4d353780e6e012e0b2fb89d2830f9be14ff72c9cde3a2af9cd6a5a7e7af6500",
  "platforms/windows/src/engine/src/commands/trash.rs": "e9c7043e2d89d721cd05b29b675cb7e9fe92da0ae1e7aea839694b499476edbe",
  "platforms/windows/src/engine/src/downloader.rs": "85da28fe6478033c761fdc7c756279cb819730adcb7be6355ca5a34025a2b9d8",
  "platforms/windows/src/engine/src/main.rs": "4ba09d79f2422cc251c7884550d5a3eff767b3951028478588524def9da84cdc",
  "platforms/windows/src/engine/src/models/runtime.rs": "1f72535377b16ccbd07104406448353ba4cac5b87bc8d2035f3d1f7611809d4e",
  "platforms/windows/src/engine/src/models/vlm.rs": "2ce0b8372ef35f3ba545df750fb62e981a5808fd08a12f284e1fe9d84f484fe7",
  "platforms/windows/src/engine/src/models/vlm_server.rs": "40e260d05d96072d0d48222a21d6d5e8a34d1e6d11650d9254b9c17352a50267",
  "platforms/windows/src/engine/src/pipeline/deep_analyze.rs": "9c50f760d9d372179a3ad1df6e76a41a7d806b0b3102cab8b27198c9baa2071b",
  "platforms/windows/src/engine/src/pipeline/doc_extract.rs": "680e893b176cd64462a1f29cd990353309f1b58b066a0c81593c1a9ac6ef46f3",
  "platforms/windows/src/engine/src/pipeline/restructure_apply.rs": "f2df458226b36379c7368d67f254475c5a20f2b3c7006db902a91f54874a2bbf",
  "platforms/windows/src/engine/src/platform.rs": "c48653f7bd5e0c0670ca8757b2581b37f4971d7533ffb442dadb3260871ee025",
  "platforms/windows/src/engine/src/shell/heic.rs": "9e21d1bb73da451e67f410aa18dcf034316bc3b88ca4f2c732c32c06fe1171cf",
  "platforms/windows/src/engine/src/shell/mod.rs": "5b681d96c81ec33cf4c3206b3ff73db3bd7d9536d5e3ad430f21d69b688c789d",
  "platforms/windows/src/engine/src/shell/ocr.rs": "5983864f1b2f13c48d4df884ac210e69df7ce781cb69ca132ff44a7f8098259e",
  "platforms/windows/src/engine/src/shell/tags.rs": "e3173acbbbae07939168102cbcb8a082e48e15bb92b0c82bd0eb4ffec527816a",
  "platforms/windows/src/engine/src/shell/trash.rs": "81e1cf3e11a4d3a839fa439e014199655f8cdd91c3a09f79f1139e21b21acc99",
  "platforms/windows/src/engine/src/shell/video.rs": "96d06fc7aa090f568732acb2c04585d66f2be5fe8994fef7bfbc48a85f8799e0",
  "platforms/windows/src/engine/src/util/content_hash.rs": "7ee089593e70e9bcf6dd3dfe53fb16ee01eae06b561ba21409396240ebe65588",
  "platforms/windows/src/engine/src/util/path_safety.rs": "75fc778eb1d98ed35adbfe52c1d74fa375b53b536c78f0591a82b8e63d8d45f9",
}
SAFE_NETWORK_CALLER_FILES = {
    "platforms/windows/src/engine/src/downloader.rs",
    "platforms/windows/src/engine/src/commands/prewarm.rs",
    "platforms/cli/src/runtime.rs",
    "platforms/apple/shared/Sources/FileIDShared/StreamingDownload.swift",
    "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VLMDownloader.swift",
    "platforms/apple/app/Sources/FileID/Services/ArcFaceModelInstaller.swift",
    "platforms/apple/app/Sources/FileID/Services/BGEModelInstaller.swift",
    "platforms/apple/app/Sources/FileID/Services/CLIPModelInstaller.swift",
    "platforms/apple/app/Sources/FileID/Services/RamPlusModelInstaller.swift",
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
        "platforms/windows/src/engine/src/main.rs",
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
CATALOG_URL_RE = re.compile(r"^https://huggingface\.co/([A-Za-z0-9._-]+/[A-Za-z0-9._-]+)/resolve/([0-9a-f]{40})/([^?#]+)$")


def catalog_for(registry: Path) -> Path:
    """The catalog beside the registry's repository, else this script's."""
    relative = Path("shared") / "models" / "catalog.json"
    for parent in registry.resolve().parents:
        if (parent / relative).is_file():
            return parent / relative
    return Path(__file__).resolve().parents[2] / relative


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


def analyze(registry: Path, downloader: Path) -> tuple[list[str], set[str], set[str]]:
    failures: list[str] = []
    registry_text = _without_comments(
        _without_rust_test_code(registry.read_text(encoding="utf-8"))
    )
    urls: set[str] = set()
    blocks, block_failures = _file_entry_blocks(registry_text)
    failures.extend(f"{registry}: {failure}" for failure in block_failures)
    catalog_constructions = 0
    for index, block in enumerate(blocks, 1):
        matches = re.findall(r'\burl:\s*"(https?://[^"\\]+)"', block)
        # The one reviewed catalog construction takes its URL from
        # shared/models/catalog.json, whose URLs are verified below.
        if not matches and CATALOG_FILE_ENTRY_URL in _normalized(block):
            catalog_constructions += 1
            continue
        if len(matches) != 1:
            failures.append(
                f"{registry}: FileEntry construction {index} must contain exactly one literal http(s) url field"
            )
            continue
        urls.add(matches[0])
    if not blocks or not urls:
        failures.append(f"{registry}: no production FileEntry URL constructions found")
    if catalog_constructions > 1:
        failures.append(f"{registry}: only one catalog-backed FileEntry construction is reviewed")
    if catalog_constructions:
        catalog_failures, catalog_urls = catalog_urls_checked(catalog_for(registry))
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
        print("Runtime egress release gate passed: registry and redirects are Hugging Face-only.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
