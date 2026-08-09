#!/usr/bin/env python3
"""Release gate: shipped engine downloads must remain Hugging Face-only."""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import subprocess
from pathlib import Path
from urllib.parse import urlparse

APPROVED_HOSTS = {"huggingface.co", "hf.co"}
KNOWN_EXTRA_HOSTS = {
    "github.com", "githubusercontent.com", "download.nvidia.com", "developer.nvidia.com"
}
KNOWN_OFF_POLICY_URLS = {
    "https://github.com/ggml-org/llama.cpp/releases/download/b9254/llama-b9254-bin-win-vulkan-x64.zip",
    "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.0/whisper-bin-x64.zip",
    "https://developer.download.nvidia.com/compute/cudnn/redist/cudnn/windows-x86_64/cudnn-windows-x86_64-9.8.0.87_cuda12-archive.zip",
    "https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-win-x64-gpu-1.22.0.zip",
    "https://github.com/ggml-org/llama.cpp/releases/download/b9254/llama-b9254-bin-win-cuda-12.4-x64.zip",
    "https://github.com/ggml-org/llama.cpp/releases/download/b9254/cudart-llama-bin-win-cuda-12.4-x64.zip",
    # CUDA math runtime for the ORT CUDA EP (CUDA 12.9 line — first with
    # native consumer-Blackwell sm_120 kernels; 12.4 ran Swin-L through
    # arch-fallback kernels at ~2 s/image). NVIDIA CDN, same host class as
    # the cuDNN archive above; mirror to HF with the other blockers.
    "https://developer.download.nvidia.com/compute/cuda/redist/cuda_cudart/windows-x86_64/cuda_cudart-windows-x86_64-12.9.79-archive.zip",
    "https://developer.download.nvidia.com/compute/cuda/redist/libcublas/windows-x86_64/libcublas-windows-x86_64-12.9.1.4-archive.zip",
    "https://developer.download.nvidia.com/compute/cuda/redist/libcufft/windows-x86_64/libcufft-windows-x86_64-11.4.1.4-archive.zip",
    "https://developer.download.nvidia.com/compute/cuda/redist/cuda_nvrtc/windows-x86_64/cuda_nvrtc-windows-x86_64-12.9.86-archive.zip",
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

EXCLUDED_SOURCE_PARTS = {
    "target", "obj", ".build", "deriveddata", "tests", "test", "benches", "examples", "scripts"
}
RAW_NETWORK_FILES = {
    "platforms/windows/src/engine/src/downloader.rs",
    "platforms/windows/src/engine/src/models/vlm_server.rs",
    "platforms/windows/src/engine/src/commands/prewarm.rs",
    "platforms/windows/src/engine/src/commands/bulk.rs",
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
    "platforms/windows/src/engine/src/models/whisper.rs",
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
    "platforms/windows/src/engine/src/shell/reveal.rs",
    "platforms/windows/src/engine/src/shell/tags.rs",
    "platforms/windows/src/engine/src/shell/thumbnail.rs",
    "platforms/windows/src/engine/src/shell/trash.rs",
    "platforms/windows/src/engine/src/shell/video.rs",
    "platforms/windows/src/engine/src/util/content_hash.rs",
    "platforms/windows/src/engine/src/util/path_safety.rs",
    "platforms/windows/src/engine/src/commands/restructure.rs",
    "platforms/windows/src/FileID.App/ViewModels/EngineClient.Commands.cs",
}
REVIEWED_NETWORK_SOURCE_SHA256 = {
    "platforms/apple/app/Sources/FileID/Database/ThumbnailService.swift": "42e7b56992f5beef2516e006aec47b2469e327d40f2b4cc37829f253e1237f10",
    "platforms/apple/engine/Sources/FileIDEngine/Models/RamPlusService.swift": "1e919213affcfbeb5fdc80068f57a92bbfb4b29a4d1ee06e5bfe20f7cb8f9349",
    "platforms/apple/engine/Sources/FileIDEngine/Models/WordPieceTokenizer.swift": "dc6292da096dbf4e75acf33394da13fe9811ec16067142a9582ab98fcd7b1668",
    "platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyze.swift": "b2ec62a2dfc3cba59b5a2a295e9587750865b6e7a617c3cf995a7546e480009b",
    "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VLMDownloader.swift": "649ae891f303751261aef7834a4a5353b0c96810018470580f6681cefef26822",
    "platforms/apple/shared/Sources/FileIDShared/CLIPTokenizer.swift": "cd8639c15375f192d89756dc509dcc8308c30e70e55926baa4c237c29e4d6d50",
    "platforms/apple/shared/Sources/FileIDShared/ModelLicenseAcceptance.swift": "bc9643b70b9bb104e13a04f0c9584c4675fef75e521abb7bd79915c0b45badc8",
    "platforms/apple/shared/Sources/FileIDShared/StreamingDownload.swift": "29cc2a712ec257b3f488f039a2afb3f6128a0a6acd03e9fc859b83d84a72ab8a",
    "platforms/apple/shared/Sources/FileIDShared/TLSPinning.swift": "3ed44d57fc25ebe197e40958d6f3bc6d7cb90a8a31b7b22dd5a76b89a46eac94",
    "platforms/windows/src/engine/src/commands/prewarm.rs": "36362dab273818418ec56b4145636403092482595ecdeaff607f90040030c493",
    "platforms/windows/src/engine/src/downloader.rs": "a3533060920f874dbc328e745edcacf58208ee9c56756834627aea56c98a08c9",
    "platforms/windows/src/engine/src/main.rs": "f115021bc50202055613c6a80825238fad4d61894ff485df6618326ff05d5094",
    "platforms/windows/src/engine/src/models/vlm_server.rs": "a603189d8b2142fe6105b30600ff82ebcbb7dbb16ad349be73171ec75f7d7e87",
    "platforms/apple/app/Sources/FileID/EngineClient.swift": "00551d7d0ce27b4306c4810f33323d1e95ffa35a25f49835cf196db2d8a4976e",
    "platforms/apple/app/Sources/FileID/Services/CLIPModelInstaller.swift": "f68d473a8a29a33b11d9f37120482f70ade3b2ba427c6a39a5b39e5f37c1c231",
    "platforms/apple/engine/Sources/FileIDEngine/Pipeline/DocText.swift": "8b5c2307fa95fbe149da38a14a48d01cb1d52299d46b23b8cd31fae4c1747f94",
    "platforms/cli/src/runtime.rs": "62af36fc5aaf77502cb633581599779adf80084e4cbc128f14e002c587e045a5",
    "platforms/cli/src/scan_models.rs": "b629b5893d9910be387d73d7e18cae1099e18d637e1c148b3ce4bb532dbf882f",
    "platforms/linux/src/app/src/engine_client.rs": "07ab3a590d6fe362cffb173c3ca99492579b4af819548df275dda12cdc5c3362",
    "platforms/linux/src/app/src/tabs/settings.rs": "a8b8be35ff2c5a496a1bd3f795ae12fb4b0b674256bcc5e6d7c75e6bafe7675f",
    "platforms/tui/src/models.rs": "bc27e7237659b63e42d2f4f8ca9d5d2a83015d849be771395654e250b0754c23",
    "platforms/tui/src/scan.rs": "3fc5136a054247f27278bd7e3050272038e828c6bfd8fcee54ddcb3e3d3a7983",
    "platforms/windows/src/engine/src/commands/trash.rs": "09f112e530d890b554ad6c1498f3a3b002bc79379a3cffa206a1f0fce6041693",
    "platforms/windows/src/engine/src/commands/bulk.rs": "61a7b9bcfba09e6f9f97c0ce8766251c8cfba601bc9d3dd4996ab1ff57627e1e",
    "platforms/windows/src/engine/src/models/vlm.rs": "b65a66a05cd29cde961265903a0791097cc1eacedab099a00eb596b1533fd161",
    "platforms/windows/src/engine/src/models/whisper.rs": "22bc6786f637487f82483732e16e43af38d54b590699d4bcd71640460f791bde",
    "platforms/windows/src/engine/src/platform.rs": "18b978061c51516a7a59e1962ebe960feb55338c0a4b7831fd8a4ae6d5a72c26",
    "platforms/windows/src/engine/src/shell/mod.rs": "82e1d3886d6b95f43eb67fa40c082f9bd880f3ad89dbf0807b478b7a67eac4c3",
    "platforms/windows/src/FileID.App/Program.cs": "9e7abdbdaa1a2245266d82e1d2e79e5dab2265872f456cfb4a1e6c3e919e83c4",
    "platforms/windows/src/FileID.App/Services/SafeOpen.cs": "976fa7c8180647d6ad7e8253ce3984df95f4532e6df25649d3981c2f60a53a94",
    "platforms/windows/src/FileID.App/ViewModels/EngineClient.cs": "0ee8b073cac92ffd45ad4138203e5bd27ab9281b38fa001f1b70f20b0ab51894",
    "platforms/windows/src/FileID.App/Views/Settings/SettingsView.xaml.cs": "3ec6de3ddd91a340b07162a7bac0beba7b412f957f70a4017c8ae42e65e6844c",
    "platforms/windows/src/FileID.App/Views/Sidebar/SidebarProcessingControl.xaml.cs": "8e5aa2c593b55bb85b53b620cafb6882e77a8632ed32abad01b7fbe7e11ba545",
    "platforms/windows/src/FileID.App/App.xaml.cs": "6ca815bd4ddf3f8fb7c6e64ad0fa31a0c106d7e35effce43f7a0796c31dd5d50",
    "platforms/windows/src/FileID.App/MainWindow.xaml.cs": "b96ca131bb349635231a07552fb0805b74b0a0622bf39abb7cf4ca77c59177ac",
    "platforms/windows/src/FileID.App/Services/FolderPickerService.cs": "288109b87c67f9789e989cd15a60fc6bb317b4b6eb154bcddbaa5ff52618d828",
    "platforms/windows/src/FileID.App/Services/WinVerifyTrustChecker.cs": "c50846c16a67365d48caa6e6206f4aa291a384ac93b17a1d85923fdc5449f117",
    "platforms/windows/src/engine/src/models/runtime.rs": "ded8e1cb12c34b1942b763cbc492a6b7e51be17d2f0d27bbf1b179126b13bdc1",
    "platforms/windows/src/engine/src/pipeline/deep_analyze.rs": "fc8e0926380abed1d811339124385a92eb2c2f1f2e4fe984ae3a6e5f6878dcf1",
    "platforms/windows/src/engine/src/pipeline/doc_extract.rs": "f5da0d296c5c7fd4ec48905125b0f527edc5da65e9407885305ae212d4a9b89e",
    "platforms/windows/src/engine/src/pipeline/restructure_apply.rs": "d95a97bc3e652fbafb68df88873ca9eead3fe567ead4e8515d70d56b11e5b740",
    "platforms/windows/src/engine/src/shell/heic.rs": "25a63774cab3f18e55fe5eaa73f46e3fb119831e446fc76e4d48a2d3190b3071",
    "platforms/windows/src/engine/src/shell/ocr.rs": "0f00992631b59d6bc1840490172c3346b0588752aa8d8fc66fa94e85ac8b27a7",
    "platforms/windows/src/engine/src/shell/reveal.rs": "99fffa994961644a9695812f9388599f233742ff85105c107e6a76dafa30591b",
    "platforms/windows/src/engine/src/shell/tags.rs": "a3b9ec505ae51429cf855ce3e7ff2afcf57fe9741f3c18bab62cd731d7aae9e2",
    "platforms/windows/src/engine/src/shell/thumbnail.rs": "9845c74529266b4e981e1d9b8e1e3e8baebe8a8629edaf6c1173081bb6d951e3",
    "platforms/windows/src/engine/src/shell/trash.rs": "46864e2622b9883d793f77afb826b2297b4c6fec1b8e618dc80cf34e2f9865e8",
    "platforms/windows/src/engine/src/shell/video.rs": "ad14b8ffa397be744c544dd69a014ed9905cde33ed3ce02a56f8e1727bfdd044",
    "platforms/windows/src/engine/src/util/content_hash.rs": "4b7317c9de3702200252178f1e2a781914151b5bc4c5d670d289ed8771e58d39",
    "platforms/windows/src/engine/src/util/path_safety.rs": "5b9b528f24aa322804d4a6153721ee03e2f3b7898ecfa0918cfbd1c63f1f6b8a",
    "platforms/windows/src/engine/src/commands/restructure.rs": "4d9b918a2ad49227d6a908a701e17877196822adfc5a2a63b8de77ac4c8335a7",
    "platforms/windows/src/FileID.App/ViewModels/EngineClient.Commands.cs": "83876e904f49c2e2e16dda087db648f3de1b0df8ac01c81fb2f237fd48610180",
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
        if any(part.lower() in EXCLUDED_SOURCE_PARTS for part in relative_parts):
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
        "platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyze.swift": 2,
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


def analyze(registry: Path, downloader: Path) -> tuple[list[str], set[str], set[str]]:
    failures: list[str] = []
    registry_text = _without_comments(
        _without_rust_test_code(registry.read_text(encoding="utf-8"))
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
        "      - name: Enforce Hugging Face-only runtime egress before publication\n"
        "        if: steps.mode.outputs.publish == 'true'\n"
        "        shell: pwsh\n"
        "        run: python ../../shared/scripts/check_runtime_egress.py\n"
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
