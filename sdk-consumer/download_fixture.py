"""Real external Cargo consumption through a private loopback transport mapping.

Only the temporary SDK's build entrypoint and binding change. The production
loader, public SDK, types and real static implementation are copied unchanged.
This tests distribution mechanics, not GitHub Release publication or TLS.
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import tarfile
import tempfile
import threading
import time

HEAVY = {"moli-core", "moli-renderer-v8", "moli-fetch", "moli-stealth-net", "moli-cookie-jar", "v8", "stylo", "btls-sys"}


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def run_fixture(consumer: Path, target: str, original: Path) -> None:
    root = Path(__file__).resolve().parent.parent
    evidence = consumer / "evidence" / "downloads"
    evidence.mkdir(parents=True, exist_ok=True)
    reports = []
    requests = []
    guard = threading.Lock()
    manifest = json.loads((original / "manifest.json").read_text(encoding="utf-8"))
    with tempfile.TemporaryDirectory(prefix="sdk-download-", dir=consumer.parent) as directory:
        work = Path(directory)
        sdk_root = work / "sdk-source"
        sdk_root.mkdir()
        # Minimal workspace metadata only; no implementation crate is present.
        (sdk_root / "Cargo.toml").write_text('[workspace]\nmembers = ["moli-sdk", "moli-sdk-types"]\nresolver = "3"\n[workspace.package]\nlicense = "MIT"\n[workspace.lints]\n', encoding="utf-8")
        for name in ("moli-sdk", "moli-sdk-types"):
            shutil.copytree(root / name, sdk_root / name, ignore=shutil.ignore_patterns("target"))
        project = work / "consumer"
        shutil.copytree(consumer, project, ignore=shutil.ignore_patterns("target", "evidence", "__pycache__", "sdk-rejections-*"))
        cargo_manifest = (project / "Cargo.toml").read_text(encoding="utf-8")
        cargo_manifest, count = re.subn(r'^moli-sdk\s*=.*$', 'moli-sdk = { path = ' + json.dumps((sdk_root / "moli-sdk").as_posix()) + ' }', cargo_manifest, flags=re.MULTILINE)
        if count != 1:
            raise RuntimeError("expected exactly one SDK dependency in external consumer")
        (project / "Cargo.toml").write_text(cargo_manifest, encoding="utf-8")
        if manifest["profile"] != "release":
            raise RuntimeError("download fixture requires a real optimized release implementation")
        # Unpublished local builds cannot satisfy production provenance. Construct
        # an explicitly synthetic fixture manifest, never rewrite the real one.
        # Native archives and every inventoried file remain byte-for-byte real.
        fixture_manifest = json.dumps(manifest | {"dirty": False, "local": False}, sort_keys=True).encode()
        manifest_sha = hashlib.sha256(fixture_manifest).hexdigest()
        (evidence / "original-manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
        (evidence / "synthetic-fixture-manifest.json").write_bytes(fixture_manifest)
        good = work / "real.tar.gz"
        with tarfile.open(good, "w:gz", compresslevel=1) as archive:
            for path in sorted(original.rglob("*")):
                if path.is_file():
                    relative = path.relative_to(original).as_posix()
                    if relative == "manifest.json":
                        item = tarfile.TarInfo(relative)
                        item.size = len(fixture_manifest)
                        archive.addfile(item, io.BytesIO(fixture_manifest))
                    else:
                        archive.add(path, arcname=relative, recursive=False)
        bad = work / "unsafe.tar.gz"
        with tarfile.open(bad, "w:gz") as archive:
            item = tarfile.TarInfo("../escaped")
            item.size = 1
            archive.addfile(item, io.BytesIO(b"x"))
        truncated = work / "truncated.tar.gz"
        with good.open("rb") as source, truncated.open("wb") as output:
            remaining = good.stat().st_size // 2
            while remaining:
                block = source.read(min(1024 * 1024, remaining))
                output.write(block)
                remaining -= len(block)
        state = {"mode": "good", "archive": good}

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_GET(self):
                mode, archive = state["mode"], state["archive"]
                record = {"path": self.path, "mode": mode, "bytes_sent": 0, "time": time.time()}
                with guard:
                    requests.append(record)
                try:
                    if self.path != "/artifact":
                        self.send_error(404)
                        record["status"] = 404
                        return
                    if mode == "http-failure":
                        self.send_error(503)
                        record["status"] = 503
                        return
                    record["status"] = 200
                    self.send_response(200)
                    self.send_header("Content-Length", str(archive.stat().st_size))
                    self.end_headers()
                    with archive.open("rb") as source:
                        while block := source.read(256 * 1024):
                            if mode == "wrong-digest" and record["bytes_sent"] == 0:
                                block = bytes([block[0] ^ 1]) + block[1:]
                            self.wfile.write(block)
                            record["bytes_sent"] += len(block)
                            if mode == "interrupt":
                                self.wfile.flush()
                                self.connection.shutdown(socket.SHUT_RDWR)
                                self.connection.close()
                                return
                except (BrokenPipeError, ConnectionResetError, OSError) as error:
                    record["transport_error"] = str(error)
                finally:
                    with guard:
                        (evidence / "requests.json").write_text(json.dumps(requests, indent=2), encoding="utf-8")

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        fixture_url = f"http://127.0.0.1:{server.server_port}/artifact"
        entrypoint = '''mod build_loader;
fn main() {
    build_loader::main_with_fetch(|url| {
        assert!(url.starts_with("https://github.com/moli-fixture/artifacts/releases/download/fixture/"));
        eprintln!("fixture transport mapping: {url} -> FIXTURE_URL");
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(900)))
            .build().into();
        Ok(agent.get("FIXTURE_URL").call()?.into_body())
    });
}
'''.replace("FIXTURE_URL", fixture_url)
        (sdk_root / "moli-sdk/build.rs").write_text(entrypoint, encoding="utf-8")
        (evidence / "transport-boundary.json").write_text(json.dumps({
            "mapping": fixture_url, "production_loader_sha256": digest(root / "moli-sdk/build_loader.rs"),
            "fixture_loader_sha256": digest(sdk_root / "moli-sdk/build_loader.rs"),
            "real_manifest_sha256": digest(original / "manifest.json"),
            "synthetic_manifest_sha256": manifest_sha,
            "synthetic_metadata": {"dirty": False, "local": False},
            "provenance_note": "Only fixture archive manifest provenance flags are synthetic; original manifest is retained as evidence. Native files and their digests are unchanged. Not release provenance verification.",
            "archive_sha256": digest(good),
            "scope": "Private copied build entrypoint maps fixed GitHub URL to loopback HTTP; no production environment override, TLS bypass, retry or FFI replacement. Does not prove Release publication or production TLS."
        }, indent=2), encoding="utf-8")
        env = os.environ.copy()
        for name in ("MOLI_SDK_ARTIFACT_DIR", "MOLI_SDK_OFFLINE", "CARGO_NET_OFFLINE", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"):
            env.pop(name, None)
        for name in ("http_proxy", "https_proxy", "all_proxy", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"):
            env[name] = ""
        if target.endswith("-pc-windows-msvc"):
            env["RUSTFLAGS"] = "-C target-feature=+crt-static"

        def bind(archive: Path) -> str:
            checksum = digest(archive)
            binding = {"schema": 1, "repository": "moli-fixture/artifacts", "release": "fixture", "targets": {
                target: {"sha256": checksum, "manifest_sha256": manifest_sha,
                         "asset": "artifact.tar.gz", "implementation_revision": manifest["implementation_revision"]}}}
            (sdk_root / "moli-sdk/artifacts.json").write_text(json.dumps(binding), encoding="utf-8")
            return checksum

        def cargo(label: str, cache: Path, success: bool, *, offline=False, slot="main", runtime=False, error=None):
            command = ["cargo", "run" if runtime else "check", "--offline", "--target", target, "--message-format=json"]
            if runtime:
                command += ["--", "benchmark"]
            case_env = env | {"MOLI_SDK_CACHE_DIR": str(cache), "MOLI_SDK_OFFLINE": str(offline).lower(), "CARGO_TARGET_DIR": str(work / f"target-{slot}")}
            result = subprocess.run(command, cwd=project, env=case_env, capture_output=True, text=True)
            (evidence / f"{label}.stdout.log").write_text(result.stdout, encoding="utf-8")
            (evidence / f"{label}.stderr.log").write_text(result.stderr, encoding="utf-8")
            compiled = []
            for line in result.stdout.splitlines():
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if event.get("reason") == "compiler-artifact":
                    if event["target"]["name"].replace("_", "-") in HEAVY:
                        raise RuntimeError(f"implementation compiled in {label}: {event['package_id']}")
                    if not event["fresh"]:
                        compiled.append(event["package_id"])
            with guard:
                reports.append({"scenario": label, "command": command, "exit_code": result.returncode,
                                "expected_success": success, "compiled_packages": compiled})
                (evidence / "results.json").write_text(json.dumps(reports, indent=2), encoding="utf-8")
            if (result.returncode == 0) != success or (error and error not in result.stderr):
                raise RuntimeError(f"unexpected {label} result: {result.stderr}")
            if not success and "Moli SDK artifact error:" not in result.stderr:
                raise RuntimeError(f"{label} did not reach artifact loader")

        try:
            checksum = bind(good)
            cache = work / "success-cache"
            cargo("first-download-link-run", cache, True, runtime=True)
            if len(requests) != 1 or requests[0]["bytes_sent"] != good.stat().st_size:
                raise RuntimeError("first build did not stream exactly one complete real archive")
            before = len(requests)
            cargo("offline-cache-link-run", cache, True, offline=True, runtime=True)
            cargo("offline-empty-cache", work / "empty-cache", False, offline=True, error="offline cache miss")
            if len(requests) != before:
                raise RuntimeError("offline mode made a network request")
            for mode in ("http-failure", "interrupt", "wrong-digest"):
                state["mode"] = mode
                cache = work / mode
                before = len(requests)
                cargo(mode, cache, False, error="SHA-256 mismatch" if mode == "wrong-digest" else None)
                entry = cache / target / checksum
                if len(requests) != before + 1 or (entry / "artifact.tar.gz").exists() or (entry / "package").exists():
                    raise RuntimeError(f"{mode}: retries or reusable failed cache")
                if mode != "http-failure" and not (entry / "download.partial").exists():
                    raise RuntimeError(f"{mode}: partial stream was not exercised")
                state["mode"] = "good"
                if mode == "interrupt":
                    # Independent target directories avoid Cargo's build lock masking the OS cache lock.
                    before = len(requests)
                    with ThreadPoolExecutor(max_workers=2) as workers:
                        futures = [workers.submit(cargo, f"concurrent-recovery-{i}", cache, True, slot=str(i)) for i in range(2)]
                        for future in futures:
                            future.result()
                    if len(requests) != before + 1:
                        raise RuntimeError("concurrent recovery must fetch exactly once")
                    cargo("concurrent-recovery-link-run", cache, True, runtime=True)
                else:
                    cargo(f"manual-rerun-{mode}", cache, True, runtime=True)
            for label, archive, expected_error in (("bound-truncated-archive", truncated, None), ("unsafe-extraction", bad, "unsafe artifact path")):
                bad_sha = bind(archive)
                state.update(mode="good", archive=archive)
                cache = work / label
                cargo(label, cache, False, error=expected_error)
                if (cache / target / bad_sha / "package").exists():
                    raise RuntimeError(f"{label}: failed extraction published a package")
                before = len(requests)
                cargo(label + "-offline-reject", cache, False, offline=True, error=expected_error)
                if len(requests) != before:
                    raise RuntimeError("failed extraction offline rerun fetched")
                if (cache / target / bad_sha / "escaped").exists():
                    raise RuntimeError("archive escaped staging directory")
            bind(good)
            state.update(mode="good", archive=good)
            cargo("final-real-control", work / "success-cache", True, offline=True, runtime=True)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()
            (evidence / "requests.json").write_text(json.dumps(requests, indent=2), encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--consumer", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--artifact-dir", type=Path, required=True)
    args = parser.parse_args()
    run_fixture(args.consumer.resolve(), args.target, args.artifact_dir.resolve())


if __name__ == "__main__":
    main()
