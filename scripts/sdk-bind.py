#!/usr/bin/env python3
"""Derive SDK Release bindings from real complete archives; never synthesize digests."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import shutil
import sys
import tarfile

TARGETS = {
    "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl",
}


def sha(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def inspect(path: Path) -> tuple[dict, str]:
    files = {}
    manifest_bytes = None
    total = 0
    with tarfile.open(path, "r:gz") as archive:
        for entry in archive:
            name = entry.name
            relative = PurePosixPath(name)
            if not entry.isfile() or relative.is_absolute() or ".." in relative.parts or any(c in name for c in "\\:\r\n") or name in files:
                raise RuntimeError(f"unsafe/duplicate archive entry {name!r} in {path}")
            total += entry.size
            if total > 24 * 1024**3:
                raise RuntimeError(f"oversized archive {path}")
            stream = archive.extractfile(entry)
            if stream is None:
                raise RuntimeError(f"unreadable archive entry {name}")
            if name == "manifest.json":
                if entry.size > 4 * 1024**2:
                    raise RuntimeError("manifest exceeds 4 MiB")
                manifest_bytes = stream.read()
                files[name] = hashlib.sha256(manifest_bytes).hexdigest()
            else:
                files[name] = hashlib.file_digest(stream, "sha256").hexdigest()
    if manifest_bytes is None:
        raise RuntimeError(f"manifest.json missing in {path}")
    manifest = json.loads(manifest_bytes)
    manifest_sha = files.pop("manifest.json")
    if manifest.get("schema") != 1 or not isinstance(manifest.get("files"), dict) or manifest["files"] != files:
        raise RuntimeError(f"archive file inventory/integrity mismatch in {path}")
    if manifest.get("target") not in TARGETS or not manifest.get("abi") or not re.fullmatch(r"[0-9a-f]{40}", manifest.get("implementation_revision", "")):
        raise RuntimeError(f"invalid target/ABI/implementation identity in {path}")
    expected_crt = "static" if "windows" in manifest["target"] else "system"
    if manifest.get("crt") != expected_crt:
        raise RuntimeError(f"wrong CRT contract in {path}")
    return manifest, manifest_sha


def bind(args: argparse.Namespace) -> None:
    directory = Path(args.artifacts)
    targets = {}
    identities = set()
    for archive in sorted(directory.rglob("moli-sdk-*.tar.gz")):
        if archive.name.endswith("-symbols.tar.gz"):
            continue
        manifest, manifest_sha = inspect(archive)
        if manifest.get("local") is not False or manifest.get("dirty") is not False or manifest.get("profile") != "release":
            raise RuntimeError(f"only clean optimized release artifacts can be bound: {archive}")
        target = manifest["target"]
        if target in targets:
            raise RuntimeError(f"more than one implementation archive for {target}")
        libraries = manifest.get("libraries", [])
        if not libraries or not any(lib.get("name") == "moli_sdk_ffi" for lib in libraries):
            raise RuntimeError(f"missing implementation archive in {archive}")
        for library in libraries:
            if library.get("file") not in manifest["files"]:
                raise RuntimeError(f"unverified link input in {archive}")
        symbols = archive.with_name(archive.name.removesuffix(".tar.gz") + "-symbols.tar.gz")
        symbols_manifest, _ = inspect(symbols)
        for key in ("abi", "target", "crt", "implementation_revision"):
            if symbols_manifest[key] != manifest[key]:
                raise RuntimeError(f"symbols identity mismatch for {archive}: {key}")
        archive_sha = sha(archive)
        if symbols_manifest.get("artifact_sha256") != archive_sha:
            raise RuntimeError(f"symbols do not belong to {archive}")
        identities.add((manifest["implementation_revision"], manifest["abi"]))
        targets[target] = {"asset": archive.name, "sha256": archive_sha, "manifest_sha256": manifest_sha, "implementation_revision": manifest["implementation_revision"], "symbols": {"asset": symbols.name, "sha256": sha(symbols)}}
    if set(targets) != TARGETS:
        raise RuntimeError(f"six-platform binding requires all real archives; missing {sorted(TARGETS - targets.keys())}")
    if len(identities) != 1:
        raise RuntimeError(f"mixed implementation revision/ABI: {identities}")
    abi_file = Path(args.abi)
    if abi_file.read_text(encoding="utf-8").strip() != next(iter(identities))[1]:
        raise RuntimeError("archive ABI does not match this SDK source")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repository):
        raise RuntimeError("repository must be owner/repository")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", args.release) or args.release == "latest":
        raise RuntimeError("release must be an explicit immutable tag identifier, not latest")
    binding = {"schema": 1, "repository": args.repository, "release": args.release, "targets": targets}
    Path(args.output).write_text(json.dumps(binding, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"Bound {len(targets)} verified archives from implementation {next(iter(identities))[0]}; this does not create or attest existence of a Release.")


def seed(args: argparse.Namespace) -> None:
    binding = json.loads(Path(args.binding).read_text(encoding="utf-8"))
    entry = binding["targets"][args.target]
    matches = list(Path(args.artifacts).rglob(entry["asset"]))
    if len(matches) != 1:
        raise RuntimeError(f"expected exactly one archive {entry['asset']}, found {matches}")
    source = matches[0]
    if sha(source) != entry["sha256"]:
        raise RuntimeError(f"archive differs from SDK binding: {source}")
    manifest, manifest_sha = inspect(source)
    if manifest["target"] != args.target or manifest["implementation_revision"] != entry["implementation_revision"] or manifest_sha != entry["manifest_sha256"]:
        raise RuntimeError("manifest differs from SDK binding")
    destination = Path(args.cache) / args.target / entry["sha256"]
    destination.mkdir(parents=True, exist_ok=True)
    output = destination / "artifact.tar.gz"
    if output.exists():
        if sha(output) != entry["sha256"]:
            raise RuntimeError(f"refusing to replace corrupt existing cache {output}")
        return
    partial = destination / "seed.partial"
    with source.open("rb") as reader, partial.open("xb") as writer:
        shutil.copyfileobj(reader, writer, length=1024 * 1024)
    if sha(partial) != entry["sha256"]:
        raise RuntimeError("archive changed while copying to cache")
    partial.replace(output)
    print(f"Seeded bound offline cache at {output}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    generate = commands.add_parser("bind")
    generate.add_argument("--artifacts", required=True)
    generate.add_argument("--repository", required=True)
    generate.add_argument("--release", required=True)
    generate.add_argument("--abi", default="moli-sdk/abi.txt")
    generate.add_argument("--output", default="moli-sdk/artifacts.json")
    populate = commands.add_parser("seed")
    populate.add_argument("--binding", default="moli-sdk/artifacts.json")
    populate.add_argument("--artifacts", required=True)
    populate.add_argument("--target", required=True, choices=sorted(TARGETS))
    populate.add_argument("--cache", required=True)
    args = parser.parse_args()
    try:
        (bind if args.command == "bind" else seed)(args)
    except (OSError, ValueError, KeyError, RuntimeError, tarfile.TarError) as error:
        print(f"SDK binding failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
