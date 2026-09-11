#!/usr/bin/env python3
"""Record actual native host imports and reject non-system dynamic SDK dependencies."""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import subprocess


def inspect(command: list[str]) -> str:
    return subprocess.check_output(command, text=True, stderr=subprocess.STDOUT)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--consumer", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    windows = "windows" in args.target
    binary = args.consumer / "target" / args.target / "release" / ("moli-sdk-consumer.exe" if windows else "moli-sdk-consumer")
    report = {"target": args.target, "binary": str(binary)}
    if windows:
        report["imports"] = inspect(["llvm-readobj", "--coff-imports", "--file-headers", str(binary)])
        names = re.findall(r"Name:\s+(\S+\.dll)", report["imports"], re.I)
        if any(re.match(r"(?:msvcr|msvcp|vcruntime|ucrtbase|api-ms-win-crt)", name, re.I) for name in names):
            raise RuntimeError(f"dynamic MSVC CRT import: {names}")
        expected = "IMAGE_FILE_MACHINE_AMD64" if args.target.startswith("x86_64") else "IMAGE_FILE_MACHINE_ARM64"
        if expected not in report["imports"]:
            raise RuntimeError("consumer machine does not match native validation target")
    else:
        report["headers"] = inspect(["readelf", "-h", str(binary)])
        report["dynamic"] = inspect(["readelf", "-d", str(binary)])
        report["versions"] = inspect(["readelf", "--version-info", str(binary)])
        names = re.findall(r"\(NEEDED\).*\[(.*?)\]", report["dynamic"])
        allowed = (r"(?:lib(?:c|m|dl|pthread|rt|util|resolv)\.so\.\d+|libgcc_s\.so\.1|ld-linux[^/]*\.so(?:\.\d+)?)" if args.target.endswith("-gnu") else r"(?:libc\.musl-(?:x86_64|aarch64)\.so\.1|libgcc_s\.so\.1)")
        if any(not re.fullmatch(allowed, name) for name in names):
            raise RuntimeError(f"non-system dynamic dependency in SDK consumer: {names}")
        if args.target.endswith("-musl") and "GLIBC_" in report["versions"]:
            raise RuntimeError("glibc symbol version in musl consumer")
        versions = [tuple(map(int, version.split("."))) for version in re.findall(r"GLIBC_(\d+\.\d+(?:\.\d+)?)", report["versions"])]
        if versions and max(versions) > (2, 36):
            raise RuntimeError(f"consumer exceeds Debian 12 glibc baseline: {max(versions)}")
        expected = "Advanced Micro Devices X86-64" if args.target.startswith("x86_64") else "AArch64"
        if expected not in report["headers"]:
            raise RuntimeError("consumer machine does not match native validation target")
    report["dynamic_libraries"] = names
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"Verified native imports: {names}")


if __name__ == "__main__":
    main()
