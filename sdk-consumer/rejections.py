"""通过真实仓库外 Cargo 消费者验证受控产物损坏及目标配置拒绝路径。"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import mmap
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading

from download_fixture import run_fixture


HEAVY = {"moli-core", "moli-renderer-v8", "moli-fetch", "moli-stealth-net", "moli-cookie-jar", "v8", "stylo", "btls-sys"}


def link_or_copy(source: str, destination: str) -> str:
    # 清单将被改写，不能硬链接到原始产物；其他文件始终只读。
    if Path(source).name == "manifest.json":
        return shutil.copyfile(source, destination)
    try:
        os.link(source, destination)
    except OSError:
        shutil.copyfile(source, destination)
    return destination


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--consumer", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--cache-dir", type=Path, required=True)
    parser.add_argument("--artifact-dir", type=Path)
    args = parser.parse_args()
    consumer = args.consumer.resolve()
    root = Path(__file__).resolve().parent.parent
    binding = json.loads((root / "moli-sdk/artifacts.json").read_text(encoding="utf-8"))
    bound = binding["targets"].get(args.target)
    if args.artifact_dir:
        original = args.artifact_dir.resolve()
    elif bound:
        original = args.cache_dir.resolve() / args.target / bound["sha256"] / "package"
    else:
        parser.error("a real local artifact or verified bound cache is required")
    original_manifest = json.loads((original / "manifest.json").read_text(encoding="utf-8"))
    evidence = consumer / "evidence" / "rejections"
    evidence.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    for name in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "MOLI_SDK_ARTIFACT_DIR"):
        env.pop(name, None)
    env.update(CARGO_NET_OFFLINE="true", MOLI_SDK_OFFLINE="true", CARGO_TARGET_DIR=str(consumer / "target"))
    if args.target.endswith("-pc-windows-msvc"):
        env["RUSTFLAGS"] = "-C target-feature=+crt-static"
    command = ["cargo", "check", "--offline", "--target", args.target, "--message-format=json"]
    reports = []
    report_lock = threading.Lock()

    def check(label: str, overrides: dict[str, str], success: bool, cargo_command: list[str] | None = None) -> None:
        result = subprocess.run(cargo_command or command, cwd=consumer, env=env | overrides, capture_output=True, text=True)
        (evidence / f"{label}.stdout.log").write_text(result.stdout, encoding="utf-8")
        (evidence / f"{label}.stderr.log").write_text(result.stderr, encoding="utf-8")
        compiled = []
        for line in result.stdout.splitlines():
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if event.get("reason") == "compiler-artifact" and not event["fresh"]:
                compiled.append(event["package_id"])
                if event["target"]["name"].replace("_", "-") in HEAVY:
                    raise RuntimeError(f"source fallback compiled in {label}: {event['package_id']}")
        with report_lock:
            reports.append({"scenario": label, "exit_code": result.returncode, "expected_success": success, "compiled_packages": compiled})
            (evidence / "results.json").write_text(json.dumps(reports, indent=2), encoding="utf-8")
        if (result.returncode == 0) != success:
            raise RuntimeError(f"unexpected Cargo result for {label}: {result.stderr}")
        print(f"{label}: {'accepted' if success else 'rejected'}", flush=True)

    with tempfile.TemporaryDirectory(prefix="sdk-rejections-", dir=consumer) as temporary:
        temporary = Path(temporary)
        package = temporary / "package"
        shutil.copytree(original, package, copy_function=link_or_copy)
        local = {"MOLI_SDK_ARTIFACT_DIR": str(package)}
        check("valid-local-control", local, True)
        for field, wrong in (("abi", "deliberately-incompatible-abi"), ("target", "wrong-target"), ("crt", "incompatible-crt")):
            changed = original_manifest | {field: wrong}
            (package / "manifest.json").write_text(json.dumps(changed), encoding="utf-8")
            check(f"wrong-{field}", local, False)
        library = original_manifest["libraries"][0]["file"]
        changed = json.loads(json.dumps(original_manifest))
        changed["files"][library] = "0" * 64
        (package / "manifest.json").write_text(json.dumps(changed), encoding="utf-8")
        check("wrong-file-digest", local, False)
        (package / "manifest.json").write_text(json.dumps(original_manifest), encoding="utf-8")
        truncated = package / library
        truncated.unlink()  # 删除本测试的硬链接，不修改原始静态库。
        with (original / library).open("rb") as source:
            truncated.write_bytes(source.read(1024))
        check("truncated-library", local, False)
        truncated.unlink()
        link_or_copy(str(original / library), str(truncated))
        if args.target.endswith("-pc-windows-msvc"):
            check("missing-host-static-crt", local | {"RUSTFLAGS": ""}, False)
        check("restored-local-control", local, True)
        shadow = package / "lib" / ("kernel32.lib" if args.target.endswith("-pc-windows-msvc") else "libc.a")
        shadow.write_bytes(b"!<arch>\n")
        check("unlisted-system-library-shadow", local, False)
        shadow.unlink()
        # 在真实实现中只替换等长 ABI 身份，保留代码、符号和调用约定；
        # 清单仍声明当前 ABI 且完整性正确，拒绝必须来自运行时握手。
        implementation = next(item["file"] for item in original_manifest["libraries"] if item["name"] == "moli_sdk_ffi")
        native = package / implementation
        native.unlink()
        shutil.copyfile(original / implementation, native)
        identity = original_manifest["abi"].encode()
        incompatible = identity[:-1] + (b"X" if identity[-1:] != b"X" else b"Y")
        with native.open("r+b") as stream, mmap.mmap(stream.fileno(), 0) as contents:
            offset = contents.find(identity)
            if offset < 0:
                raise RuntimeError("real implementation ABI identity is not available for the runtime mismatch fixture")
            while offset >= 0:
                contents[offset:offset + len(identity)] = incompatible
                offset = contents.find(identity, offset + len(identity))
            contents.flush()
        changed = json.loads(json.dumps(original_manifest))
        with native.open("rb") as stream:
            changed["files"][implementation] = hashlib.file_digest(stream, "sha256").hexdigest()
        (package / "manifest.json").write_text(json.dumps(changed), encoding="utf-8")
        check("runtime-rejects-mismatched-implementation", local, True,
              ["cargo", "run", "--offline", "--target", args.target, "--message-format=json", "--", "reject-runtime-abi"])
        native.unlink()
        link_or_copy(str(original / implementation), str(native))
        (package / "manifest.json").write_text(json.dumps(original_manifest), encoding="utf-8")
        notice = "notices/LICENSE-MIT"
        notice_path = package / notice
        altered_notice = notice_path.read_bytes() + b"\nSDK integrity fixture\n"
        notice_path.unlink()
        notice_path.write_bytes(altered_notice)
        changed = json.loads(json.dumps(original_manifest))
        changed["files"][notice] = hashlib.sha256(altered_notice).hexdigest()
        (package / "manifest.json").write_text(json.dumps(changed), encoding="utf-8")
        check("explicit-local-trust-new-manifest", local, True)
        if bound:
            empty = temporary / "empty-cache"
            check("offline-empty-cache", {"MOLI_SDK_CACHE_DIR": str(empty)}, False)
            corrupted = temporary / "corrupt-cache" / args.target / bound["sha256"]
            corrupted.mkdir(parents=True)
            (corrupted / "artifact.tar.gz").write_bytes(b"truncated-gzip")
            check("corrupt-bound-archive", {"MOLI_SDK_CACHE_DIR": str(temporary / "corrupt-cache")}, False)
            forged = temporary / "forged-manifest-cache" / args.target / bound["sha256"]
            forged.mkdir(parents=True)
            archive = args.cache_dir.resolve() / args.target / bound["sha256"] / "artifact.tar.gz"
            link_or_copy(str(archive), str(forged / "artifact.tar.gz"))
            shutil.copytree(package, forged / "package", copy_function=link_or_copy)
            check("default-rejects-self-consistent-unbound-manifest", {"MOLI_SDK_CACHE_DIR": str(temporary / "forged-manifest-cache")}, False)
            recovering = temporary / "recover-cache" / args.target / bound["sha256"]
            (recovering / "unpack.partial").mkdir(parents=True)
            (recovering / "unpack.partial" / "manifest.json").write_text("interrupted unpack", encoding="utf-8")
            link_or_copy(str(archive), str(recovering / "artifact.tar.gz"))
            with ThreadPoolExecutor(max_workers=2) as workers:
                cases = [
                    workers.submit(check, f"partial-unpack-concurrent-{index}", {
                        "MOLI_SDK_CACHE_DIR": str(temporary / "recover-cache"),
                        # 避免 Cargo 的 target 目录锁掩盖 SDK 缓存并发。
                        "CARGO_TARGET_DIR": str(temporary / f"target-{index}"),
                    }, True)
                    for index in range(2)
                ]
                for case in cases:
                    case.result()
        else:
            print("No final binding yet: bound-cache rejection scenarios remain unverified.", flush=True)
    run_fixture(consumer, args.target, original)
    final_source = {"MOLI_SDK_ARTIFACT_DIR": str(original)} if args.artifact_dir else {"MOLI_SDK_CACHE_DIR": str(args.cache_dir.resolve())}
    check("original-control-after-fixtures", final_source, True,
          ["cargo", "run", "--offline", "--target", args.target, "--message-format=json", "--", "benchmark"])


if __name__ == "__main__":
    main()
