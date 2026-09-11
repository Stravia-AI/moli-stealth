"""对同机同工具链的源码和 SDK 集成记录真实 Cargo 编译单元与墙钟时间。"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess


loader = importlib.util.spec_from_file_location("consumer_verify", Path(__file__).with_name("verify.py"))
verify = importlib.util.module_from_spec(loader)
loader.loader.exec_module(verify)


def measure(project: Path, target: str, env: dict[str, str]) -> None:
    rustc = subprocess.check_output(["rustc", "-vV"], cwd=project, env=env, text=True)
    (project / "measurement-environment.json").write_text(json.dumps({"rustc": rustc, "target": target, "cargo_home": env.get("CARGO_HOME", "default"), "target_cache": "empty", "dependency_download_cache": "shared, inherited; download effects remain in first-build logs", "sdk_artifact_mode": "explicit local" if env.get("MOLI_SDK_ARTIFACT_DIR") else "fixed binding", "workload": "binary HTTP echo, repeated response cookies applied to next request, real-layout browser DCL navigation, isolated JS, rendered HTML, explicit cleanup"}, indent=2), encoding="utf-8")
    build = ["build", "--target", target, "--timings", "--message-format=json"]
    verify.cargo(project, env, build, "first-debug")
    verify.cargo(project, env, build, "cached-debug")
    main = project / "src" / "main.rs"
    original_code = main.read_text(encoding="utf-8")
    marker = '"browser-http-cookie benchmark workload passed"'
    if marker not in original_code:
        raise RuntimeError("host output marker missing; cannot measure a real code change")
    # 改变宿主可见输出，必须重新生成代码和链接；不是只有注释变化。
    main.write_text(original_code.replace(marker, '"browser-http-cookie benchmark workload passed; host edit"', 1), encoding="utf-8")
    verify.cargo(project, env, build, "host-only-debug")
    verify.cargo(project, env, [*build, "--release"], "release-switch")
    verify.cargo(project, env, ["run", "--release", "--target", target, "--", "benchmark"], "same-workload")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-checkout", type=Path, required=True, help="fixed implementation revision checkout")
    parser.add_argument("--sdk-revision", required=True)
    parser.add_argument("--repository", default="https://github.com/Stravia-AI/moli-stealth")
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--artifact-dir", type=Path)
    parser.add_argument("--cache-dir", type=Path, required=True)
    parser.add_argument("--order", choices=["source-first", "sdk-first"], required=True)
    args = parser.parse_args()
    if len(args.sdk_revision) != 40 or any(c not in "0123456789abcdef" for c in args.sdk_revision):
        parser.error("--sdk-revision must be an exact lowercase 40-character commit")
    template = Path(__file__).resolve().parent
    destination = args.destination.resolve()
    if destination.exists() or template.parent == destination or template.parent in destination.parents:
        parser.error("destination must be a new directory outside implementation repository")
    destination.mkdir(parents=True)
    source = args.source_checkout.resolve()
    source_revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=source, text=True).strip()
    source_status = subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=no"], cwd=source, text=True)
    if source_status:
        parser.error("source checkout must have no tracked changes")
    (destination / "comparison.json").write_text(json.dumps({"source_revision": source_revision, "sdk_revision": args.sdk_revision, "target": args.target, "order": args.order, "constraint": "same machine and toolchain, separate empty target directories, shared inherited dependency download cache; includes final-link cost and any download in first build"}, indent=2), encoding="utf-8")
    env = os.environ.copy()
    for name in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "MOLI_SDK_ARTIFACT_DIR"):
        env.pop(name, None)
    if args.target.endswith("-pc-windows-msvc"):
        env["RUSTFLAGS"] = "-C target-feature=+crt-static"
    env["MOLI_SDK_CACHE_DIR"] = str(args.cache_dir.resolve())
    if args.artifact_dir:
        env["MOLI_SDK_ARTIFACT_DIR"] = str(args.artifact_dir.resolve())
    projects = {}
    original_manifest = (template / "Cargo.toml").read_text(encoding="utf-8")
    for mode in ("source", "sdk"):
        project = destination / mode
        shutil.copytree(template, project, ignore=shutil.ignore_patterns("target", "Cargo.lock", "*.log", "__pycache__"))
        if mode == "source":
            dependencies = "\n".join(f'{name} = {{ path = {json.dumps((source / name).as_posix())} }}' for name in ("moli-core", "moli-stealth-net", "moli-cookie-jar"))
            manifest = original_manifest.replace('moli-sdk = { path = "../moli-sdk" }', dependencies)
            shutil.copyfile(template / "source-baseline.rs", project / "src" / "main.rs")
        else:
            manifest = original_manifest.replace('moli-sdk = { path = "../moli-sdk" }', f'moli-sdk = {{ git = {json.dumps(args.repository)}, rev = "{args.sdk_revision}" }}')
        (project / "Cargo.toml").write_text(manifest, encoding="utf-8")
        projects[mode] = project
    order = ("source", "sdk") if args.order == "source-first" else ("sdk", "source")
    for mode in order:
        project = projects[mode]
        measure(project, args.target, env | {"CARGO_TARGET_DIR": str(project / "target")})


if __name__ == "__main__":
    main()
