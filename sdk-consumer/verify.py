"""在仓库外用精确 Git revision 验证 SDK，不继承实现仓库的 Cargo 配置。"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time


HEAVY_PACKAGES = {
    "moli-core", "moli-renderer-v8", "moli-fetch", "moli-stealth-net",
    "moli-cookie-jar", "v8", "stylo", "btls-sys", "aws-lc-sys",
}


def cargo(project: Path, env: dict[str, str], args: list[str], label: str) -> dict:
    start = time.perf_counter()
    result = subprocess.run(["cargo", *args], cwd=project, env=env, text=True, capture_output=True)
    elapsed = time.perf_counter() - start
    (project / f"{label}.stdout.log").write_text(result.stdout, encoding="utf-8")
    (project / f"{label}.stderr.log").write_text(result.stderr, encoding="utf-8")
    units = []
    for line in result.stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(event, dict) and event.get("reason") == "compiler-artifact":
            units.append({"package_id": event["package_id"], "target": event["target"]["name"], "fresh": event["fresh"]})
    measurement = {"label": label, "command": ["cargo", *args], "seconds": elapsed, "returncode": result.returncode, "units": units}
    with (project / "measurements.jsonl").open("a", encoding="utf-8") as evidence:
        evidence.write(json.dumps(measurement) + "\n")
    print(f"{label}: {elapsed:.3f}s, exit={result.returncode}", flush=True)
    if result.returncode:
        raise RuntimeError(f"{label} failed:\n{result.stderr}")
    return measurement


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk-revision", required=True)
    parser.add_argument("--repository", default="https://github.com/Stravia-AI/moli-stealth")
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--artifact-dir", type=Path)
    parser.add_argument("--cache-dir", type=Path, required=True)
    args = parser.parse_args()
    if len(args.sdk_revision) != 40 or any(c not in "0123456789abcdef" for c in args.sdk_revision):
        parser.error("--sdk-revision must be an exact lowercase 40-character commit")
    template = Path(__file__).resolve().parent
    destination = args.destination.resolve()
    if destination.exists():
        parser.error("destination must not exist; evidence and existing files are never overwritten")
    if template.parent == destination or template.parent in destination.parents:
        parser.error("destination must be outside the implementation repository")
    shutil.copytree(template, destination, ignore=shutil.ignore_patterns("target", "Cargo.lock", "*.log", "__pycache__"))
    manifest = (destination / "Cargo.toml").read_text(encoding="utf-8")
    manifest = manifest.replace('moli-sdk = { path = "../moli-sdk" }', f'moli-sdk = {{ git = {json.dumps(args.repository)}, rev = "{args.sdk_revision}" }}')
    (destination / "Cargo.toml").write_text(manifest, encoding="utf-8")
    env = os.environ.copy()
    env.pop("CARGO_ENCODED_RUSTFLAGS", None)
    env.pop("RUSTFLAGS", None)
    env.pop("MOLI_SDK_ARTIFACT_DIR", None)
    env["CARGO_TARGET_DIR"] = str(destination / "target")
    env["MOLI_SDK_CACHE_DIR"] = str(args.cache_dir.resolve())
    env["MOLI_SDK_EVIDENCE_DIR"] = str(destination / "evidence")
    if args.artifact_dir:
        env["MOLI_SDK_ARTIFACT_DIR"] = str(args.artifact_dir.resolve())
    if args.target.endswith("-pc-windows-msvc"):
        env["RUSTFLAGS"] = "-C target-feature=+crt-static"
    toolchain = subprocess.check_output(["rustc", "-vV"], cwd=destination, env=env, text=True)
    provenance = {"sdk_revision": args.sdk_revision, "repository": args.repository, "target": args.target, "toolchain": toolchain, "local_override": str(args.artifact_dir) if args.artifact_dir else None, "initial_target_cache": "empty", "cargo_download_cache": "inherited", "sdk_cache": str(args.cache_dir.resolve()), "manifest_sha256": hashlib.sha256(manifest.encode()).hexdigest()}
    (destination / "provenance.json").write_text(json.dumps(provenance, indent=2), encoding="utf-8")
    base = ["build", "--target", args.target, "--timings", "--message-format=json"]
    cargo(destination, env, base, "first-debug")
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--filter-platform", args.target], cwd=destination, env=env, text=True))
    forbidden = sorted({package["name"] for package in metadata["packages"]} & HEAVY_PACKAGES)
    if forbidden:
        raise RuntimeError(f"source implementation leaked into consumer graph: {forbidden}")
    (destination / "dependency-graph.json").write_text(json.dumps(metadata, indent=2), encoding="utf-8")
    cargo(destination, env, base, "cached-debug")
    offline = env | {"CARGO_NET_OFFLINE": "true"}
    cargo(destination, offline, [*base, "--offline"], "offline-debug")
    main_rs = destination / "src" / "main.rs"
    original_code = main_rs.read_text(encoding="utf-8")
    marker = '"browser-http-cookie benchmark workload passed"'
    if marker not in original_code:
        raise RuntimeError("host output marker missing; cannot measure a real code change")
    main_rs.write_text(original_code.replace(marker, '"browser-http-cookie benchmark workload passed; host edit"', 1), encoding="utf-8")
    increment = cargo(destination, offline, [*base, "--offline"], "host-only-debug")
    rebuilt = [unit for unit in increment["units"] if not unit["fresh"]]
    if any(unit["target"] != "moli-sdk-consumer" for unit in rebuilt):
        raise RuntimeError(f"host-only edit rebuilt a dependency: {rebuilt}")
    cargo(destination, env, [*base, "--release"], "release-switch")
    shutil.copytree(destination / "target" / "cargo-timings", destination / "evidence" / "cargo-timings")
    cargo(destination, env, ["run", "--release", "--target", args.target, "--", "all"], "runtime-all")
    cargo(destination, env, ["test", "--target", args.target], "consumer-tests")
    rejection_command = [
        sys.executable, str(template / "rejections.py"), "--consumer", str(destination),
        "--target", args.target, "--cache-dir", str(args.cache_dir.resolve()),
    ]
    if args.artifact_dir:
        rejection_command.extend(["--artifact-dir", str(args.artifact_dir.resolve())])
    subprocess.run(rejection_command, cwd=destination, env=env, check=True)


if __name__ == "__main__":
    main()
