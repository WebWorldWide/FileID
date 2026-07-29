from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


ORT_API_VERSION = 22
GET_ERROR_MESSAGE_INDEX = 2
RELEASE_STATUS_INDEX = 93
GET_AVAILABLE_PROVIDERS_INDEX = 125
RELEASE_AVAILABLE_PROVIDERS_INDEX = 126


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def function_pointer(base: int, index: int) -> int:
    pointer_size = ctypes.sizeof(ctypes.c_void_p)
    value = ctypes.c_void_p.from_address(base + index * pointer_size).value
    if value is None:
        raise RuntimeError(f"ORT API function slot {index} is null")
    return value


def status_message(api: int, status: int) -> str:
    get_error_message = ctypes.WINFUNCTYPE(
        ctypes.c_char_p, ctypes.c_void_p
    )(function_pointer(api, GET_ERROR_MESSAGE_INDEX))
    release_status = ctypes.WINFUNCTYPE(
        None, ctypes.c_void_p
    )(function_pointer(api, RELEASE_STATUS_INDEX))
    try:
        raw = get_error_message(status)
        return raw.decode("utf-8", errors="replace") if raw else "unknown ORT error"
    finally:
        release_status(status)


def query_providers(dll_path: Path) -> dict[str, Any]:
    dll_directory = None
    if hasattr(os, "add_dll_directory"):
        dll_directory = os.add_dll_directory(str(dll_path.parent))
    try:
        runtime = ctypes.WinDLL(str(dll_path))
        runtime.OrtGetApiBase.argtypes = []
        runtime.OrtGetApiBase.restype = ctypes.c_void_p
        base = runtime.OrtGetApiBase()
        if not base:
            raise RuntimeError("OrtGetApiBase returned null")

        get_api = ctypes.WINFUNCTYPE(
            ctypes.c_void_p, ctypes.c_uint32
        )(function_pointer(base, 0))
        get_version = ctypes.WINFUNCTYPE(
            ctypes.c_char_p
        )(function_pointer(base, 1))
        api = get_api(ORT_API_VERSION)
        if not api:
            raise RuntimeError(
                f"ORT API version {ORT_API_VERSION} is not supported by this runtime"
            )

        get_available = ctypes.WINFUNCTYPE(
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.POINTER(ctypes.c_char_p)),
            ctypes.POINTER(ctypes.c_int),
        )(function_pointer(api, GET_AVAILABLE_PROVIDERS_INDEX))
        release_available = ctypes.WINFUNCTYPE(
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_char_p),
            ctypes.c_int,
        )(function_pointer(api, RELEASE_AVAILABLE_PROVIDERS_INDEX))

        providers_pointer = ctypes.POINTER(ctypes.c_char_p)()
        provider_count = ctypes.c_int()
        status = get_available(
            ctypes.byref(providers_pointer),
            ctypes.byref(provider_count),
        )
        if status:
            raise RuntimeError(status_message(api, status))

        providers: list[str] = []
        try:
            providers = [
                providers_pointer[index].decode("utf-8", errors="replace")
                for index in range(provider_count.value)
            ]
        finally:
            release_status = release_available(
                providers_pointer,
                provider_count.value,
            )
            if release_status:
                raise RuntimeError(status_message(api, release_status))

        raw_version = get_version()
        return {
            "runtimeVersion": (
                raw_version.decode("utf-8", errors="replace")
                if raw_version
                else None
            ),
            "providers": providers,
        }
    finally:
        if dll_directory is not None:
            dll_directory.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Query the bundled ONNX Runtime provider table without third-party Python packages."
    )
    parser.add_argument("--dll", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--allowed-python-root", type=Path)
    parser.add_argument("--require-provider", action="append", default=[])
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    dll_path = args.dll.resolve(strict=True)
    python_paths = [
        str(Path(entry).resolve())
        for entry in sys.path
        if entry
    ]
    python_path_contained = True
    if args.allowed_python_root is not None:
        allowed_python_root = args.allowed_python_root.resolve(strict=True)
        allowed_normalized = os.path.normcase(str(allowed_python_root))
        python_path_contained = all(
            os.path.commonpath(
                [allowed_normalized, os.path.normcase(entry)]
            )
            == allowed_normalized
            for entry in python_paths
        )
    else:
        allowed_python_root = None
    _ = (sqlite3.sqlite_version, subprocess.PIPE, tempfile.gettempdir())
    started_at = utc_now()
    result: dict[str, Any] = {
        "schemaVersion": 1,
        "startedAt": started_at,
        "finishedAt": None,
        "dll": str(dll_path),
        "dllSha256": sha256_file(dll_path),
        "apiVersion": ORT_API_VERSION,
        "python": {
            "executable": sys.executable,
            "version": sys.version,
            "sysPath": python_paths,
            "allowedRoot": (
                str(allowed_python_root)
                if allowed_python_root is not None
                else None
            ),
        },
        "runtimeVersion": None,
        "compiledProviders": [],
        "requiredProviders": list(args.require_provider),
        "providers": [],
        "checks": {
            "dllLoaded": False,
            "apiVersionSupported": False,
            "pythonPathContained": python_path_contained,
            "runtimeVersionMatches": False,
            "providersReported": False,
            "cpuProviderAvailable": False,
            "requiredProvidersAvailable": False,
        },
        "error": None,
        "result": "RED",
    }
    try:
        provider_result = query_providers(dll_path)
        providers = provider_result["providers"]
        result["runtimeVersion"] = provider_result["runtimeVersion"]
        result["compiledProviders"] = providers
        result["providers"] = providers
        result["checks"] = {
            "dllLoaded": True,
            "apiVersionSupported": True,
            "pythonPathContained": python_path_contained,
            "runtimeVersionMatches": str(
                provider_result["runtimeVersion"]
            ).startswith("1.22."),
            "providersReported": bool(providers),
            "cpuProviderAvailable": "CPUExecutionProvider" in providers,
            "requiredProvidersAvailable": all(
                provider in providers
                for provider in args.require_provider
            ),
        }
        if all(result["checks"].values()):
            result["result"] = "GREEN"
    except (OSError, RuntimeError, ValueError) as error:
        result["error"] = f"{type(error).__name__}: {error}"
    finally:
        result["finishedAt"] = utc_now()
        write_json(args.output, result)
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["result"] == "GREEN" else 1


if __name__ == "__main__":
    raise SystemExit(main())
