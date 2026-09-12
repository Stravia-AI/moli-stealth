#!/usr/bin/env python3
"""Build real native SDK archives, collect static closure, and split debugging material."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parent.parent
TARGETS = (
    "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl",
)
LINUX_SYSTEM = {"c", "m", "dl", "pthread", "rt", "util", "resolv", "gcc_s"}
WINDOWS_SYSTEM = set("advapi32 bcrypt crypt32 dbghelp dnsapi gdi32 iphlpapi kernel32 msimg32 ncrypt netapi32 ntdll ole32 oleaut32 opengl32 powrprof psapi rpcrt4 secur32 setupapi shell32 shlwapi synchronization user32 userenv uuid version winhttp winmm winspool ws2_32 wsock32 wtsapi32 dwrite d2d1 dxgi d3d11 windowsapp libcmt libvcruntime libucrt libcpmt legacy_stdio_definitions oldnames".split())


def run(command: list[str], *, env: dict[str, str] | None = None, echo: bool = True) -> str:
    print("+ " + shlex.join(map(str, command)), flush=True)
    result = subprocess.run(command, cwd=ROOT, env=env, text=True, encoding="utf-8", stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    if echo or result.returncode:
        print(result.stdout, end="", flush=True)
    if result.returncode:
        raise RuntimeError(f"command failed ({result.returncode}): {shlex.join(command)}")
    return result.stdout


def sha(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def tool(name: str) -> str:
    found = shutil.which(name)
    if not found:
        raise RuntimeError(f"required native packaging tool not found: {name}")
    return found


def archive(directory: Path, destination: Path) -> None:
    # Only regular files, no top-level directory and no symlinks: matches build.rs.
    with tarfile.open(destination, "w:gz", format=tarfile.USTAR_FORMAT) as output:
        for path in sorted(directory.rglob("*")):
            if path.is_symlink():
                raise RuntimeError(f"symlink in package: {path}")
            if path.is_file():
                info = output.gettarinfo(str(path), arcname=path.relative_to(directory).as_posix())
                info.uid = info.gid = info.mtime = 0
                info.uname = info.gname = ""
                info.mode = 0o644
                with path.open("rb") as stream:
                    output.addfile(info, stream)


def verify_machine(path: Path, target: str) -> None:
    with path.open("rb") as stream:
        if stream.read(8) != b"!<arch>\n":
            raise RuntimeError(f"not a normal static archive: {path}")
    output = run([tool("llvm-readobj"), "--file-headers", str(path)], echo=False)
    machines = re.findall(r"Machine:\s+(\S+)", output)
    wanted = ("IMAGE_FILE_MACHINE_AMD64" if target.startswith("x86_64") else "IMAGE_FILE_MACHINE_ARM64") if "windows" in target else ("EM_X86_64" if target.startswith("x86_64") else "EM_AARCH64")
    if not machines or any(machine != wanted for machine in machines):
        raise RuntimeError(f"wrong or uninspectable architecture in {path}: expected {wanted}, found {set(machines)}")
    if "windows" in target:
        directives = run([tool("llvm-readobj"), "--coff-directives", str(path)], echo=False)
        if re.search(r"DEFAULTLIB:[\"']?(?:MSVCRTD?|MSVCPRTD?|UCRTD?|VCRUNTIMED?|LIBCMTD|LIBCPMTD|LIBUCRTD|LIBVCRUNTIMED)(?:\.lib)?(?:[\"'\s]|$)", directives, re.I):
            raise RuntimeError(f"dynamic/debug CRT directive in {path}")


def coff_has_index_metadata(path: Path) -> bool:
    # MSVC 的这些节含有非 relocation 形式的索引。llvm-strip 不重映射
    # 它们；保留整个成员才能同时保全索引、校验和与链接语义。
    with path.open("rb") as source:
        header = source.read(56)
        if header[:4] == b"\0\0\xff\xff":
            if len(header) != 56 or int.from_bytes(header[4:6], "little") < 2:
                raise RuntimeError(f"unsupported COFF object header: {path}")
            count = int.from_bytes(header[44:48], "little")
            source.seek(56)
        else:
            if len(header) < 20:
                raise RuntimeError(f"truncated COFF object header: {path}")
            count = int.from_bytes(header[2:4], "little")
            source.seek(20 + int.from_bytes(header[16:18], "little"))
        for _ in range(count):
            section = source.read(40)
            if len(section) != 40:
                raise RuntimeError(f"truncated COFF section table: {path}")
            if section[:8].rstrip(b"\0") in {b".voltbl", b".chks64"}:
                return True
    return False


def transform_coff_archive(path: Path, target: str, renames: dict[str, str] | None = None, affected_members: set[str] | None = None) -> None:
    # Rust 的 COFF staticlib 包含 short import object。它们没有调试节，
    # llvm-strip 却拒绝这种格式；只剥离机器码对象，导入对象原样归档。
    # 用独立目录容纳同名成员，但保留 DLL 导入成员的 basename：
    # MSVC 根据成员名分组 .idata，改名会使导入表在启动时失效。
    with tempfile.TemporaryDirectory(prefix="coff-strip-", dir=path.parent) as temporary:
        temporary = Path(temporary)
        encoded_renames = {old.encode("ascii"): new.encode("ascii") for old, new in (renames or {}).items()}
        members = []
        native = []
        machine_objects = 0
        names = b""
        total_size = path.stat().st_size
        with path.open("rb") as source:
            if source.read(8) != b"!<arch>\n":
                raise RuntimeError(f"not a regular COFF archive: {path}")
            while header := source.read(60):
                if len(header) != 60 or header[58:60] != b"`\n":
                    raise RuntimeError(f"invalid COFF archive member header: {path}")
                size = int(header[48:58])
                position = source.tell()
                if size < 0 or position + size > total_size:
                    raise RuntimeError(f"truncated COFF archive member: {path}")
                name = header[:16].rstrip(b" ")
                if name == b"//":
                    names = source.read(size)
                    source.seek(position + size + size % 2)
                    continue
                if name in {b"/", b"/SYM64/"}:
                    source.seek(position + size + size % 2)
                    continue
                data_position = position
                if name.startswith(b"#1/"):
                    name_size = int(name[3:])
                    if not 0 <= name_size <= size:
                        raise RuntimeError(f"invalid archive extended name: {path}")
                    name = source.read(name_size).rstrip(b"\0")
                    data_position += name_size
                elif name.startswith(b"/"):
                    offset = int(name[1:])
                    if not 0 <= offset < len(names):
                        raise RuntimeError(f"invalid archive long name: {path}")
                    name = re.split(b"\0|/\n", names[offset:], maxsplit=1)[0]
                else:
                    name = name.rstrip(b"/")
                basename = name.replace(b"\\", b"/").rsplit(b"/", 1)[-1].decode("utf-8")
                if not basename or basename in {".", ".."} or ":" in basename:
                    raise RuntimeError(f"invalid COFF member filename: {path}")
                length = position + size - data_position
                source.seek(data_position)
                prefix = source.read(min(20, length))
                source.seek(data_position)
                directory = temporary / f"member-{len(members):05d}"
                directory.mkdir()
                member = directory / basename
                with member.open("wb") as output:
                    remaining = length
                    while remaining:
                        chunk = source.read(min(1024 * 1024, remaining))
                        if not chunk:
                            raise RuntimeError(f"truncated object in {path}")
                        output.write(chunk)
                        remaining -= len(chunk)
                members.append(member)
                if prefix[:6] == b"\0\0\xff\xff\0\0":
                    if len(prefix) != 20 or 20 + int.from_bytes(prefix[12:16], "little") != length:
                        raise RuntimeError(f"invalid COFF short import object in {path}")
                else:
                    machine_objects += 1
                    if renames is not None:
                        rename_coff_runtime(member, encoded_renames, affected_members is None or basename in affected_members)
                    elif not coff_has_index_metadata(member):
                        native.append(member)
                source.seek(position + size + size % 2)
        if not machine_objects:
            raise RuntimeError(f"COFF implementation archive has no machine-code objects: {path}")
        native_arguments = temporary / "native.rsp"
        native_arguments.write_text("\n".join(f'"{member.as_posix()}"' for member in native), encoding="utf-8")
        if native:
            run([tool("llvm-strip"), "--strip-debug", f"@{native_arguments}"])
        all_arguments = temporary / "members.rsp"
        all_arguments.write_text("\n".join(f'"{member.as_posix()}"' for member in members), encoding="utf-8")
        rebuilt = temporary / path.name
        run([tool("llvm-ar"), "--format=coff", "--rsp-quoting=posix", "qcsD", str(rebuilt), f"@{all_arguments}"])
        verify_machine(rebuilt, target)
        os.replace(rebuilt, path)


def strip_debug_archive(path: Path, target: str) -> None:
    if "windows" in target:
        transform_coff_archive(path, target)
    else:
        run([tool("llvm-strip"), "--strip-debug", str(path)])


def rename_coff_runtime(path: Path, mapping: dict[bytes, bytes], rename_symbols: bool = True) -> None:
    # llvm-objcopy renames symbols, not .drectve string references, and its
    # writer rebuilds symbol indices without handling .voltbl/.chks64. Keep
    # every symbol/auxiliary slot and section byte in place instead. Only an
    # affected object's symbol/string tables are copied, never its debug data.
    import zlib

    tokens = re.compile(rb"[A-Za-z_?$@.][A-Za-z0-9_?$@.]*")
    with path.open("r+b") as stream:
        length = path.stat().st_size

        def read_at(offset: int, size: int) -> bytes:
            if offset < 0 or size < 0 or offset + size > length:
                raise RuntimeError(f"truncated COFF structure in {path}")
            stream.seek(offset)
            value = stream.read(size)
            if len(value) != size:
                raise RuntimeError(f"truncated COFF structure in {path}")
            return value

        header = read_at(0, min(56, length))
        big = header[:4] == b"\0\0\xff\xff"
        if big:
            if len(header) != 56 or int.from_bytes(header[4:6], "little") != 2:
                raise RuntimeError(f"unsupported COFF bigobj header in {path}")
            section_count = int.from_bytes(header[44:48], "little")
            section_start, symbol_field, symbol_size = 56, 48, 20
        else:
            if len(header) < 20 or int.from_bytes(header[16:18], "little"):
                raise RuntimeError(f"unsupported COFF object header in {path}")
            section_count = int.from_bytes(header[2:4], "little")
            section_start, symbol_field, symbol_size = 20, 8, 18
        symbol_start = int.from_bytes(header[symbol_field:symbol_field + 4], "little")
        symbol_count = int.from_bytes(header[symbol_field + 4:symbol_field + 8], "little")
        directives = {}
        for section_index in range(section_count):
            offset = section_start + section_index * 40
            section = read_at(offset, 40)
            if section[:8] != b".drectve":
                continue
            size = int.from_bytes(section[16:20], "little")
            position = int.from_bytes(section[20:24], "little")
            data = read_at(position, size)
            replacement = tokens.sub(lambda match: mapping.get(match[0], match[0]), data)
            if replacement != data:
                # Equal-length private names keep all directive offsets valid.
                if len(replacement) != size:
                    raise RuntimeError(f"runtime rename changes COFF directive size in {path}")
                directives[section_index + 1] = (position, replacement)
        if not rename_symbols and not directives:
            return
        if not symbol_start or not symbol_count:
            raise RuntimeError(f"affected COFF object has no symbol table in {path}")
        string_start = symbol_start + symbol_count * symbol_size
        string_size = int.from_bytes(read_at(string_start, 4), "little")
        if string_size < 4:
            raise RuntimeError(f"invalid COFF string table in {path}")
        strings = read_at(string_start, string_size)
        symbols = read_at(symbol_start, symbol_count * symbol_size)
        changed = None
        added = bytearray()
        index = 0
        while index < symbol_count:
            offset = index * symbol_size
            entry = symbols[offset:offset + symbol_size]
            auxiliary = entry[-1]
            if index + auxiliary >= symbol_count:
                raise RuntimeError(f"invalid COFF auxiliary symbol count in {path}")
            if entry[:4] == b"\0\0\0\0":
                name_offset = int.from_bytes(entry[4:8], "little")
                name_end = strings.find(b"\0", name_offset)
                if not 4 <= name_offset < string_size or name_end < 0:
                    raise RuntimeError(f"invalid COFF symbol name in {path}")
                name = strings[name_offset:name_end]
            else:
                name = entry[:8].rstrip(b"\0")
            if name in mapping:
                if changed is None:
                    changed = bytearray(symbols)
                changed[offset:offset + 8] = b"\0" * 4 + (string_size + len(added)).to_bytes(4, "little")
                added.extend(mapping[name] + b"\0")
            section_number = int.from_bytes(entry[12:16 if big else 14], "little", signed=True)
            if entry[-2] == 3 and auxiliary == 1 and name == b".drectve" and section_number in directives:
                if changed is None:
                    changed = bytearray(symbols)
                checksum = zlib.crc32(directives[section_number][1], 0xffffffff) ^ 0xffffffff
                auxiliary_offset = offset + symbol_size
                changed[auxiliary_offset + 8:auxiliary_offset + 12] = checksum.to_bytes(4, "little")
            index += auxiliary + 1
        if changed is not None:
            # Appending avoids assumptions about trailing CodeView data. Old
            # string offsets (including long section names) remain valid.
            new_start = length
            if new_start + len(changed) + string_size + len(added) >= 1 << 32:
                raise RuntimeError(f"isolated COFF object exceeds 32-bit offsets: {path}")
            stream.seek(new_start)
            stream.write(changed)
            stream.write((string_size + len(added)).to_bytes(4, "little"))
            stream.write(strings[4:])
            stream.write(added)
            stream.seek(symbol_field)
            stream.write(new_start.to_bytes(4, "little"))
        for position, data in directives.values():
            stream.seek(position)
            stream.write(data)


def isolate_rust_runtime(path: Path, target: str, revision: str) -> dict[str, str]:
    # These are Rust ABI implementation names, not C/system ABI builtins such
    # as memcpy, __udivti3, _Unwind_* or __CxxFrameHandler3. Isolate mangled
    # Rust names too: a same-version host has identical std crate identities,
    # and private personality references force both std objects into the link.
    # Rust statics also emit COFF __imp_ aliases; these are not system imports.
    # rust_link_cplusplus is the C++ link helper's C ABI, not Rust's runtime.
    family = re.compile(r"(?:rust_|__rust_)[A-Za-z0-9_]+|__(?:rg|rdl)_(?:alloc|alloc_zeroed|dealloc|realloc|oom)")
    mangled = re.compile(r"_R[CNIMXY][A-Za-z0-9_]+(?:\.llvm\.[0-9]+)?|_ZN.*17h[0-9a-f]{16}E(?:\.llvm\.[0-9]+)?")
    # Diagnostics for symbol-less members must not interleave with records.
    output = subprocess.check_output(
        [tool("llvm-nm"), "--format=posix", "--print-file-name", "--extern-only", str(path)],
        text=True, encoding="utf-8",
    )
    definitions = set()
    candidates = set()
    existing = set()
    symbol_members: dict[str, set[str]] = {}
    for line in output.splitlines():
        match = re.match(r"^.*\[([^\]]+)\]: (\S+) ([A-Za-z?])(?:\s|$)", line)
        if not match:
            if line.strip():
                raise RuntimeError(f"unrecognized llvm-nm archive record: {line}")
            continue
        member, name, kind = match.groups()
        existing.add(name)
        base_name = name.removeprefix("__imp_")
        runtime = family.fullmatch(base_name) is not None
        private = runtime or mangled.fullmatch(base_name) is not None
        if private or name.startswith((".weak.", "DW.ref.")):
            symbol_members.setdefault(name, set()).add(member.replace("\\", "/").rsplit("/", 1)[-1])
        if not private or base_name == "rust_link_cplusplus":
            continue
        candidates.add(name)
        if kind not in {"U", "w", "v"}:
            if runtime and not member.endswith(".rcgu.o"):
                raise RuntimeError(f"Rust runtime name {name} has a non-Rust definition in {member}")
            definitions.add(name)
    if not definitions or candidates - definitions:
        raise RuntimeError(f"Rust runtime closure is absent or incomplete in {path}: {sorted(candidates - definitions)}")
    # COFF exports .weak.__rust_*.default targets; ELF emits weak
    # DW.ref.rust_eh_personality COMDAT pointers. Privatize those too, or the
    # host can coalesce the pointer and silently select its own personality.
    # llvm-objcopy updates ELF symbol names, including SHT_GROUP signatures.
    for name in existing:
        if name.startswith("DW.ref.") and name.removeprefix("DW.ref.") in candidates:
            definitions.add(name)
        elif name.startswith(".weak."):
            base = name.removeprefix(".weak.")
            while "." in base:
                base = base.rsplit(".", 1)[0]
                if base in candidates:
                    definitions.add(name)
                    break
    # Fixed width permits .drectve replacement without changing offsets. Hash
    # the exact SDK revision and original name; reject every collision rather
    # than relying on the truncated hash's probability.
    renames = {name: "moli_" + hashlib.shake_256(f"{revision}:{name}".encode("ascii")).hexdigest((len(name) - 4) // 2)[:len(name) - 5] for name in sorted(definitions)}
    if len(set(renames.values())) != len(renames) or set(renames.values()) & existing:
        raise RuntimeError(f"private Rust runtime symbol collision in {path}")
    print(f"SDK private Rust symbols: {len(renames)}", flush=True)
    if "windows" in target:
        affected_members = set().union(*(symbol_members[name] for name in renames))
        transform_coff_archive(path, target, renames, affected_members)
    else:
        # ELF archives can be handled in one invocation: parsing the full
        # Rust mapping once per member would make this needlessly quadratic.
        with tempfile.TemporaryDirectory(prefix="rust-symbols-", dir=path.parent) as temporary:
            temporary = Path(temporary)
            rename_file = temporary / "renames.txt"
            with rename_file.open("w", encoding="ascii") as output:
                for old, new in renames.items():
                    output.write(f"{old} {new}\n")
            rewritten = temporary / path.name
            run([tool("llvm-objcopy"), f"--redefine-syms={rename_file}", str(path), str(rewritten)])
            verify_machine(rewritten, target)
            os.replace(rewritten, path)
    return renames


def notices(destination: Path, cargo_metadata: dict, native_sources: list[Path]) -> None:
    destination.mkdir(parents=True)
    for name in ("LICENSE-APACHE", "LICENSE-MIT", "license-metadata.json"):
        shutil.copy2(ROOT / name, destination / name)
    shutil.copytree(ROOT / "licenses", destination / "repository")
    rust_notices = Path(run(["rustc", "--print", "sysroot"]).strip()) / "share/doc/rust"
    rust_destination = destination / "rust-runtime"
    rust_destination.mkdir()
    shutil.copy2(rust_notices / "COPYRIGHT-library.html", rust_destination)
    shutil.copytree(rust_notices / "licenses", rust_destination / "licenses")
    inventory = []
    for package in cargo_metadata["packages"]:
        source = Path(package["manifest_path"]).parent
        package_name = f"{package['name']}-{package['version']}"
        license_files = [p for p in source.iterdir() if p.is_file() and re.match(r"^(licen[sc]e|copying|notice)([._-]|$)", p.name, re.I)]
        if package.get("license_file"):
            specified = source / package["license_file"]
            if specified.is_file() and specified not in license_files:
                license_files.append(specified)
        if license_files:
            (destination / "cargo" / package_name).mkdir(parents=True, exist_ok=True)
            for path in license_files:
                shutil.copy2(path, destination / "cargo" / package_name / path.name)
        inventory.append({key: package.get(key) for key in ("name", "version", "source", "license", "license_file", "repository")})
        # V8 and BoringSSL carry additional vendored third-party license files.
        if package["name"] in {"v8", "btls-sys"}:
            for path in source.rglob("*"):
                if path.is_file() and not path.is_symlink() and re.match(r"^(licen[sc]e|copying|notice)([._-]|$)", path.name, re.I):
                    output = destination / "native-vendored" / package_name / path.relative_to(source)
                    output.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(path, output)
    for source in native_sources:
        if not source.exists():
            raise RuntimeError(f"native license source is absent: {source}")
        target = destination / "system-build-packages" / source.name
        if source.is_dir():
            shutil.copytree(source, target, symlinks=False, dirs_exist_ok=True)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
    write_json(destination / "cargo-packages.json", inventory)


def build(args: argparse.Namespace) -> None:
    target = args.target
    host = re.search(r"^host: (.+)$", run(["rustc", "-vV"]), re.M)
    if not host or host[1] != target:
        raise RuntimeError(f"SDK release builds must be native: rustc host={host[1] if host else 'unknown'}, target={target}")
    windows = "windows" in target
    revision = run(["git", "rev-parse", "HEAD"]).strip()
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise RuntimeError("implementation must have an exact Git commit identity")
    # A dirty implementation would falsely claim to be this revision. Binding-only edits are later.
    dirty = run(["git", "status", "--porcelain", "--untracked-files=normal"]).strip()
    if dirty and not args.local:
        raise RuntimeError("release packaging requires a clean implementation commit; use --local for explicitly trusted uncommitted artifacts")
    if args.profile != "release" and not args.local:
        raise RuntimeError("non-optimized builds require --local and cannot enter Release bindings")
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    work = output / f"work-{target}"
    if work.exists():
        raise RuntimeError(f"refusing to overwrite previous package workspace: {work}")
    package = work / "package"
    symbols = work / "symbols"
    (package / "lib").mkdir(parents=True)
    (symbols / "lib").mkdir(parents=True)
    env = os.environ.copy()
    env.update(CARGO_PROFILE_RELEASE_DEBUG="2", CARGO_PROFILE_RELEASE_STRIP="none", CARGO_PROFILE_RELEASE_PANIC="unwind", CARGO_PROFILE_DEV_DEBUG="2", CARGO_PROFILE_DEV_STRIP="none", CARGO_PROFILE_DEV_PANIC="unwind", PKG_CONFIG_ALL_STATIC="1")
    if windows:
        # 减少调试信息副本，避免默认并行代码生成将 COFF 归档推过 4 GiB 边界。
        env["CARGO_PROFILE_DEV_CODEGEN_UNITS"] = "1"
    if "RUST_FONTCONFIG_DLOPEN" in env:
        raise RuntimeError("RUST_FONTCONFIG_DLOPEN is incompatible with complete static SDK packaging")
    flags = env.get("RUSTFLAGS", "")
    tokens = shlex.split(flags)
    crt_static = None
    for index, token in enumerate(tokens):
        option = tokens[index + 1] if token == "-C" and index + 1 < len(tokens) else token.removeprefix("-C")
        if option.startswith("target-feature="):
            for feature in option.removeprefix("target-feature=").split(","):
                if feature in {"+crt-static", "-crt-static"}:
                    crt_static = feature == "+crt-static"
    env["RUSTFLAGS"] = flags + (" -C target-feature=+crt-static" if windows and crt_static is not True else "")
    target_dir = Path(env.get("CARGO_TARGET_DIR", str(ROOT / "target"))).resolve()
    profile_flags = ["--release"] if args.profile == "release" else []
    profile_dir = "release" if args.profile == "release" else "debug"
    log = run(["cargo", "rustc", "--locked", *profile_flags, "--target", target, "--package", "moli-sdk-ffi", "--message-format=json-render-diagnostics", "--", "--print", "native-static-libs"], env=env)
    (work / "build.log").write_text(log, encoding="utf-8")
    rendered = log
    searches: list[Path] = []
    build_outputs: set[Path] = set()
    if not windows:
        rust_libraries = Path(run(["rustc", "--print", "target-libdir", "--target", target], env=env).strip())
        searches.extend([rust_libraries, rust_libraries / "self-contained"])
    for line in log.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("reason") == "compiler-message":
            rendered += "\n" + (message["message"].get("rendered") or "")
        if message.get("reason") == "build-script-executed":
            build_outputs.add(Path(message["out_dir"]))
            for value in message.get("linked_paths", []):
                searches.append(Path(value.split("=", 1)[-1]))
    lists = re.findall(r"native-static-libs:\s*([^\r\n]+)", rendered)
    if not lists:
        raise RuntimeError("rustc did not report native-static-libs; cannot infer complete downstream link closure")
    native = shlex.split(lists[-1])
    source = target_dir / target / profile_dir / ("moli_sdk_ffi.lib" if windows else "libmoli_sdk_ffi.a")
    libraries = [{"name": "moli_sdk_ffi", "file": f"lib/{source.name}"}]
    shutil.copy2(source, package / libraries[0]["file"])
    private_symbols = isolate_rust_runtime(package / libraries[0]["file"], target, revision)
    write_json(symbols / "rust-private-symbols.json", {"implementation_revision": revision, "target": target, "renames": private_symbols})
    system: list[str] = []
    for argument in native:
        if windows:
            name = argument.lower().removeprefix("/defaultlib:").removesuffix(".lib")
        else:
            if not argument.startswith("-l"):
                raise RuntimeError(f"unhandled rustc native linker argument {argument}; audit before publishing")
            name = argument[2:]
        if not re.fullmatch(r"[A-Za-z0-9_+.-]+", name):
            raise RuntimeError(f"unsafe native library name {name}")
        if name in (WINDOWS_SYSTEM if windows else LINUX_SYSTEM):
            if name not in system:
                system.append(name)
            continue
        if any(lib["name"] == name for lib in libraries):
            continue
        filename = f"{name}.lib" if windows else f"lib{name}.a"
        candidates = [directory / filename for directory in searches if (directory / filename).is_file()]
        if not windows:
            located = run([env.get("CXX", "c++"), f"-print-file-name={filename}"], env=env).strip()
            if Path(located).is_file():
                candidates.append(Path(located))
            for directory in run(["pkg-config", "--variable=libdir", "fontconfig"], env=env).splitlines():
                if (Path(directory) / filename).is_file():
                    candidates.append(Path(directory) / filename)
        if not candidates:
            raise RuntimeError(f"non-system native dependency {name} has no static archive; refusing a dynamic fallback")
        unique = {sha(path): path for path in candidates}
        if len(unique) != 1:
            raise RuntimeError(f"ambiguous native archives for {name}: {candidates}")
        shutil.copy2(next(iter(unique.values())), package / "lib" / filename)
        libraries.append({"name": name, "file": f"lib/{filename}"})
    for library in libraries:
        path = package / library["file"]
        verify_machine(path, target)
        # Full unstripped archives preserve COFF CodeView and DWARF, including archive member identity.
        shutil.copy2(path, symbols / library["file"])
        if args.profile == "release":
            strip_debug_archive(path, target)
    if windows:
        # Only this implementation's build outputs belong to its symbol
        # package; a shared Cargo target can also contain unrelated test PDBs.
        for directory in sorted(build_outputs):
            for pdb in directory.rglob("*.pdb"):
                destination = symbols / "pdb" / pdb.relative_to(target_dir)
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(pdb, destination)
    metadata_text = subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target], cwd=ROOT, text=True, encoding="utf-8", env=env)
    metadata = json.loads(metadata_text)
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    pending = [package["id"] for package in metadata["packages"] if package["name"] == "moli-sdk-ffi"]
    reachable = set()
    while pending:
        package_id = pending.pop()
        if package_id not in reachable:
            reachable.add(package_id)
            pending.extend(nodes[package_id]["dependencies"])
    metadata["packages"] = [package for package in metadata["packages"] if package["id"] in reachable]
    notices(package / "notices", metadata, [Path(path) for path in args.native_notices])
    shutil.copytree(package / "notices", symbols / "notices")
    manifest = {
        "schema": 1, "target": target, "abi": (ROOT / "moli-sdk/abi.txt").read_text().strip(),
        "crt": "static" if windows else "system", "implementation_revision": revision,
        "profile": args.profile, "dirty": bool(dirty), "local": args.local,
        "rustc": run(["rustc", "-vV"]).strip(),
        "libraries": libraries, "system_libraries": system,
        "files": {path.relative_to(package).as_posix(): sha(path) for path in sorted(package.rglob("*")) if path.is_file()},
    }
    write_json(package / "manifest.json", manifest)
    name = f"moli-sdk-{revision}-{target}" + ("-local" if args.local else "")
    main_archive = output / f"{name}.tar.gz"
    archive(package, main_archive)
    symbols_manifest = {key: manifest[key] for key in ("schema", "target", "abi", "crt", "implementation_revision", "rustc", "profile", "dirty", "local")}
    symbols_manifest.update(artifact_sha256=sha(main_archive), files={path.relative_to(symbols).as_posix(): sha(path) for path in sorted(symbols.rglob("*")) if path.is_file()})
    write_json(symbols / "manifest.json", symbols_manifest)
    symbols_archive = output / f"{name}-symbols.tar.gz"
    archive(symbols, symbols_archive)
    record = {"target": target, "asset": main_archive.name, "sha256": sha(main_archive), "manifest_sha256": sha(package / "manifest.json"), "implementation_revision": revision, "abi": manifest["abi"], "symbols": {"asset": symbols_archive.name, "sha256": sha(symbols_archive)}}
    write_json(output / f"{name}.json", record)
    print(f"Local override: MOLI_SDK_ARTIFACT_DIR={package}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=TARGETS)
    parser.add_argument("--output", default="dist/sdk")
    parser.add_argument("--local", action="store_true", help="Explicit unpublished build; permits dirty sources and never qualifies for Release binding")
    parser.add_argument("--profile", choices=("release", "dev"), default="release")
    parser.add_argument("--native-notices", action="append", default=[], help="Copyright/license tree for native system-build packages (not runtime dependencies)")
    args = parser.parse_args()
    try:
        build(args)
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"SDK packaging failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
