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


def measure(project: Path, env: dict[str, str], command: list[str], label: str, *, cwd: Path | None = None) -> dict:
    start = time.perf_counter()
    result = subprocess.run(command, cwd=cwd or project, env=env, text=True, capture_output=True)
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
    measurement = {"label": label, "command": command, "seconds": elapsed, "returncode": result.returncode, "units": units,
                   "cwd": str(cwd or project), "rustup_toolchain": env.get("RUSTUP_TOOLCHAIN"), "target_dir": env.get("CARGO_TARGET_DIR")}
    with (project / "measurements.jsonl").open("a", encoding="utf-8") as evidence:
        evidence.write(json.dumps(measurement) + "\n")
    print(f"{label}: {elapsed:.3f}s, exit={result.returncode}", flush=True)
    if result.returncode:
        raise RuntimeError(f"{label} failed:\n{result.stderr}")
    return measurement


def cargo(project: Path, env: dict[str, str], args: list[str], label: str) -> dict:
    return measure(project, env, ["cargo", *args], label)


def implementation_provenance(metadata: dict, target: str, artifact_dir: Path | None, cache_dir: Path) -> dict:
    if artifact_dir:
        package = artifact_dir.resolve()
    else:
        sdk = next(item for item in metadata["packages"] if item["name"] == "moli-sdk")
        binding = json.loads(Path(sdk["manifest_path"]).with_name("artifacts.json").read_text(encoding="utf-8"))
        package = cache_dir.resolve() / target / binding["targets"][target]["sha256"] / "package"
    # first-debug has already validated this actual archive through the loader.
    raw = (package / "manifest.json").read_bytes()
    manifest = json.loads(raw)
    toolchain = manifest["rustc"]
    fields = dict(line.split(": ", 1) for line in toolchain.splitlines() if ": " in line)
    if fields.get("release") != "1.96.1" or fields.get("host") != target:
        raise RuntimeError(f"cross-version proof requires a native Rust 1.96.1 implementation for {target}:\n{toolchain}")
    return {
        "revision": manifest["implementation_revision"],
        "manifest_sha256": hashlib.sha256(raw).hexdigest(),
        "toolchain": toolchain,
    }


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
    measure(destination, env, ["rustc", "-vV"], "baseline-rustc")
    toolchain = (destination / "baseline-rustc.stdout.log").read_text(encoding="utf-8")
    fields = dict(line.split(": ", 1) for line in toolchain.splitlines() if ": " in line)
    if fields.get("release") != "1.96.1" or fields.get("host") != args.target:
        raise RuntimeError(f"baseline consumer requires native Rust 1.96.1 for {args.target}:\n{toolchain}")
    provenance = {"sdk_revision": args.sdk_revision, "repository": args.repository, "target": args.target, "toolchain": toolchain, "local_override": str(args.artifact_dir) if args.artifact_dir else None, "initial_target_cache": "empty", "cargo_download_cache": "inherited", "sdk_cache": str(args.cache_dir.resolve()), "manifest_sha256": hashlib.sha256(manifest.encode()).hexdigest()}
    (destination / "provenance.json").write_text(json.dumps(provenance, indent=2), encoding="utf-8")
    base = ["build", "--target", args.target, "--timings", "--message-format=json"]
    cargo(destination, env, base, "first-debug")
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--filter-platform", args.target], cwd=destination, env=env, text=True))
    forbidden = sorted({package["name"] for package in metadata["packages"]} & HEAVY_PACKAGES)
    if forbidden:
        raise RuntimeError(f"source implementation leaked into consumer graph: {forbidden}")
    (destination / "dependency-graph.json").write_text(json.dumps(metadata, indent=2), encoding="utf-8")
    provenance["implementation"] = implementation_provenance(metadata, args.target, args.artifact_dir, args.cache_dir)
    (destination / "provenance.json").write_text(json.dumps(provenance, indent=2), encoding="utf-8")
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

    # An empty debug target forces a fresh link against the same bound implementation.
    # Do not turn this into cargo check or reuse the baseline host executable.
    cross_root = destination / "cross-rust-1.98.1"
    if cross_root.exists():
        raise RuntimeError("cross-version target must start empty")
    cross_env = offline | {
        "RUSTUP_TOOLCHAIN": f"1.98.1-{args.target}",
        "CARGO_TARGET_DIR": str(cross_root / "target"),
        "MOLI_SDK_EVIDENCE_DIR": str(destination / "evidence" / "rust-1.98.1"),
    }
    measure(destination, cross_env, ["rustc", "-vV"], "cross-rust-1.98.1-rustc")
    cross_toolchain = (destination / "cross-rust-1.98.1-rustc.stdout.log").read_text(encoding="utf-8")
    fields = dict(line.split(": ", 1) for line in cross_toolchain.splitlines() if ": " in line)
    if fields.get("release") != "1.98.1" or fields.get("host") != args.target:
        raise RuntimeError(f"cross-version consumer requires native Rust 1.98.1 for {args.target}:\n{cross_toolchain}")
    cross_provenance = provenance | {
        "toolchain": cross_toolchain,
        "rustup_toolchain": cross_env["RUSTUP_TOOLCHAIN"],
        "target_dir": cross_env["CARGO_TARGET_DIR"],
        "profile": "debug",
        "compatibility_scope": "Rust 1.96.1 implementation consumed by Rust 1.98.1 host only",
    }
    (destination / "cross-rust-1.98.1-provenance.json").write_text(json.dumps(cross_provenance, indent=2), encoding="utf-8")
    cargo(destination, cross_env, [*base, "--offline", "--locked"], "cross-rust-1.98.1-debug")
    measure(destination, cross_env, ["cargo", "run", "--manifest-path", str(destination / "Cargo.toml"), "--target", args.target, "--offline", "--locked", "--", "all"], "cross-rust-1.98.1-runtime-all", cwd=cross_root)
    shutil.copytree(cross_root / "target" / "cargo-timings", destination / "evidence" / "rust-1.98.1" / "cargo-timings")


if __name__ == "__main__":
    main()
