//! Only this loader (not the implementation workspace) runs in a downstream build.
use flate2::read::GzDecoder;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    env,
    error::Error,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

type Result<T, E = Box<dyn Error>> = std::result::Result<T, E>;
const TARGETS: &[&str] = &[
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
];
const MAX_ARCHIVE: u64 = 8 * 1024 * 1024 * 1024;
const MAX_UNPACKED: u64 = 24 * 1024 * 1024 * 1024;

pub fn main() {
    main_with_fetch(|url| {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(Duration::from_secs(900)))
            .build()
            .into();
        Ok(agent
            .get(url)
            .call()
            .map_err(|e| format!("download {url} failed: {e}"))?
            .into_body())
    });
}

// Private build-script seam: only a replacement build entrypoint can select transport.
pub fn main_with_fetch(fetch: impl Fn(&str) -> Result<ureq::Body>) {
    if let Err(error) = run(&fetch) {
        panic!(
            "Moli SDK artifact error: {error}. No source-build fallback is available. See docs/SDK.md."
        );
    }
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing/string field {key}").into())
}

fn digest(path: &Path) -> Result<String> {
    let mut reader = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha(value: &str) -> Result<&str> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("SHA-256 must be 64 lowercase hexadecimal characters".into());
    }
    Ok(value)
}

fn safe_path(value: &str) -> Result<&Path> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains(['\\', ':', '\n', '\r'])
        || path
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err(format!("unsafe artifact path {value:?}").into());
    }
    Ok(path)
}

fn identifier(value: &str) -> Result<&str> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_+.-".contains(&b))
    {
        return Err(format!("invalid artifact identifier {value:?}").into());
    }
    Ok(value)
}

fn read_json(path: &Path) -> Result<Value> {
    if fs::metadata(path)?.len() > 4 * 1024 * 1024 {
        return Err("manifest exceeds 4 MiB".into());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn regular_file(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = safe_path(relative)?;
    let mut current = root.to_path_buf();
    for component in path.components() {
        current.push(component);
        if fs::symlink_metadata(&current)?.file_type().is_symlink() {
            return Err(format!("symlink in artifact: {}", current.display()).into());
        }
    }
    if !fs::metadata(&current)?.is_file() {
        return Err(format!("not a regular file: {}", current.display()).into());
    }
    Ok(current)
}

fn system_library(target: &str, name: &str) -> bool {
    if target.contains("windows") {
        // Windows SDK import libraries and the explicitly static MSVC runtime.
        [
            "advapi32",
            "bcrypt",
            "crypt32",
            "dbghelp",
            "dnsapi",
            "gdi32",
            "iphlpapi",
            "kernel32",
            "msimg32",
            "ncrypt",
            "netapi32",
            "ntdll",
            "ole32",
            "oleaut32",
            "opengl32",
            "powrprof",
            "psapi",
            "rpcrt4",
            "secur32",
            "setupapi",
            "shell32",
            "shlwapi",
            "synchronization",
            "user32",
            "userenv",
            "uuid",
            "version",
            "winhttp",
            "winmm",
            "winspool",
            "ws2_32",
            "wsock32",
            "wtsapi32",
            "dwrite",
            "d2d1",
            "dxgi",
            "d3d11",
            "windowsapp",
            "libcmt",
            "libvcruntime",
            "libucrt",
            "libcpmt",
            "legacy_stdio_definitions",
            "oldnames",
        ]
        .contains(&name)
    } else {
        ["c", "m", "dl", "pthread", "rt", "util", "resolv", "gcc_s"].contains(&name)
    }
}

fn validate(root: &Path, target: &str, abi: &str, revision: Option<&str>) -> Result<Value> {
    let manifest = read_json(&regular_file(root, "manifest.json")?)?;
    if manifest["schema"] != 1 {
        return Err("unsupported artifact manifest schema".into());
    }
    for (key, expected) in [
        ("target", target),
        ("abi", abi),
        (
            "crt",
            if target.contains("windows") {
                "static"
            } else {
                "system"
            },
        ),
    ] {
        if text(&manifest, key)? != expected {
            return Err(format!(
                "artifact {key} mismatch: expected {expected}, got {:?}",
                manifest[key]
            )
            .into());
        }
    }
    let implementation = text(&manifest, "implementation_revision")?;
    if implementation.len() != 40 || !implementation.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid implementation revision".into());
    }
    if revision.is_some_and(|expected| expected != implementation) {
        return Err("artifact implementation revision differs from SDK binding".into());
    }
    if revision.is_some()
        && (manifest["dirty"] != false
            || manifest["local"] != false
            || manifest["profile"] != "release")
    {
        return Err("default SDK binding requires a clean optimized release artifact".into());
    }
    let files = manifest["files"]
        .as_object()
        .ok_or("manifest files must be an object")?;
    if files.is_empty() {
        return Err("empty artifact file manifest".into());
    }
    for (name, expected) in files {
        let expected = sha(expected.as_str().ok_or("file digest must be a string")?)?;
        let path = regular_file(root, name)?;
        if digest(&path)? != expected {
            return Err(format!("file integrity failure: {}", path.display()).into());
        }
    }
    // 原生搜索目录中额外的同名库可能遮蔽系统库；清单必须覆盖整个包，
    // 而不只是保证已经列出的文件正确。也不接受清单外的链接或设备。
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                directories.push(path);
            } else {
                let relative = path
                    .strip_prefix(root)?
                    .to_str()
                    .ok_or("non-UTF8 artifact filename")?
                    .replace('\\', "/");
                if !kind.is_file()
                    || (relative != "manifest.json" && !files.contains_key(&relative))
                {
                    return Err(format!("unlisted or non-regular artifact file: {relative}").into());
                }
            }
        }
    }
    let libraries = manifest["libraries"]
        .as_array()
        .ok_or("missing libraries")?;
    if libraries.is_empty() {
        return Err("artifact has no static libraries".into());
    }
    let mut names = BTreeSet::new();
    for library in libraries {
        let name = identifier(text(library, "name")?)?;
        if !names.insert(name) {
            return Err("duplicate static library name".into());
        }
        let file = text(library, "file")?;
        let expected_file = if target.contains("windows") {
            format!("lib/{name}.lib")
        } else {
            format!("lib/lib{name}.a")
        };
        if file != expected_file || !files.contains_key(file) {
            return Err(format!("unverified/invalid static library {file}").into());
        }
        let mut magic = [0_u8; 8];
        File::open(regular_file(root, file)?)?.read_exact(&mut magic)?;
        if &magic != b"!<arch>\n" {
            return Err(format!("not a regular static archive: {file}").into());
        }
    }
    if !names.contains("moli_sdk_ffi") {
        return Err("implementation static library is missing".into());
    }
    for library in manifest["system_libraries"]
        .as_array()
        .ok_or("missing system_libraries")?
    {
        let name = library.as_str().ok_or("system library must be string")?;
        if !system_library(target, name) {
            return Err(format!("unapproved system dependency {name}; non-system dependencies must be packaged statically").into());
        }
    }
    Ok(manifest)
}

fn unpack(archive: &Path, root: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(GzDecoder::new(File::open(archive)?));
    let mut seen = BTreeSet::new();
    let mut total = 0_u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry
            .path()?
            .to_str()
            .ok_or("non-UTF8 archive path")?
            .to_owned();
        safe_path(&path)?;
        if !seen.insert(path.clone()) {
            return Err(format!("duplicate archive entry {path}").into());
        }
        // No links, devices, sparse files, or archive-controlled modes.
        if !entry.header().entry_type().is_file() {
            return Err(format!("non-regular archive entry {path}").into());
        }
        total = total
            .checked_add(entry.size())
            .ok_or("archive size overflow")?;
        if total > MAX_UNPACKED {
            return Err("unpacked artifact exceeds 24 GiB".into());
        }
        let output = root.join(&path);
        fs::create_dir_all(output.parent().ok_or("missing parent")?)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)?;
        let expected = entry.size();
        if io::copy(&mut entry, &mut file)? != expected {
            return Err(format!("truncated archive entry {path}").into());
        }
        file.sync_all()?;
    }
    Ok(())
}

fn offline() -> bool {
    ["CARGO_NET_OFFLINE", "MOLI_SDK_OFFLINE"]
        .iter()
        .any(|name| env::var(name).is_ok_and(|v| v == "true" || v == "1"))
}

fn cached_artifact(
    binding: &Value,
    target: &str,
    abi: &str,
    fetch: &impl Fn(&str) -> Result<ureq::Body>,
) -> Result<(PathBuf, Value)> {
    let target_binding = binding["targets"].get(target).ok_or_else(|| format!("SDK revision has no bound artifact for {target}; generate artifacts.json from actual six-target archives or explicitly set MOLI_SDK_ARTIFACT_DIR"))?;
    let expected = sha(text(target_binding, "sha256")?)?;
    let manifest_sha = sha(text(target_binding, "manifest_sha256")?)?;
    let asset = identifier(text(target_binding, "asset")?)?;
    let revision = text(target_binding, "implementation_revision")?;
    let repository = text(binding, "repository")?;
    let parts: Vec<_> = repository.split('/').collect();
    if parts.len() != 2 {
        return Err("binding repository must be owner/repository".into());
    }
    for part in parts {
        identifier(part)?;
    }
    let release = identifier(text(binding, "release")?)?;
    if release == "latest" {
        return Err("latest is not a fixed Release identity".into());
    }
    let cache = env::var_os("MOLI_SDK_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("CARGO_HOME").map(|p| PathBuf::from(p).join("moli-sdk")))
        .or_else(|| {
            env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|p| PathBuf::from(p).join(".cargo/moli-sdk"))
        })
        .ok_or("set MOLI_SDK_CACHE_DIR: no home directory available")?;
    let root = cache.join(target).join(expected);
    fs::create_dir_all(&root)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("artifact.lock"))?;
    lock.lock()?; // OS lock is released on process exit, including crashes.
    let archive = root.join("artifact.tar.gz");
    let package = root.join("package");
    if archive.exists() && digest(&archive)? != expected {
        return Err(format!(
            "cached archive integrity failure at {}; remove this cache entry to fetch again",
            archive.display()
        )
        .into());
    }
    if !archive.exists() {
        if offline() {
            return Err(format!("offline cache miss for {target} at {}; preseed artifact.tar.gz with the bound archive or set MOLI_SDK_ARTIFACT_DIR", root.display()).into());
        }
        let partial = root.join("download.partial");
        if partial.exists() {
            fs::remove_file(&partial)?;
        }
        let url = format!("https://github.com/{repository}/releases/download/{release}/{asset}");
        eprintln!("Moli SDK: downloading {url} into {}", root.display());
        let mut body = fetch(&url)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)?;
        let count = io::copy(&mut body.as_reader().take(MAX_ARCHIVE + 1), &mut output)?;
        output.flush()?;
        output.sync_all()?;
        drop(output);
        if count > MAX_ARCHIVE {
            return Err("download exceeds 8 GiB".into());
        }
        if digest(&partial)? != expected {
            return Err(format!(
                "download SHA-256 mismatch for {url}; partial file retained at {}",
                partial.display()
            )
            .into());
        }
        fs::rename(&partial, &archive)?;
    }
    let manifest = if package.exists() {
        // Authenticate the manifest before trusting its file inventory.
        if digest(&package.join("manifest.json"))? != manifest_sha {
            return Err(
                "cached manifest integrity failure against SDK binding; remove the cache entry"
                    .into(),
            );
        }
        validate(&package, target, abi, Some(revision))?
    } else {
        let staging = root.join("unpack.partial");
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir(&staging)?;
        unpack(&archive, &staging)?;
        if digest(&staging.join("manifest.json"))? != manifest_sha {
            return Err("archive manifest SHA-256 differs from SDK binding".into());
        }
        let manifest = validate(&staging, target, abi, Some(revision))?;
        fs::rename(&staging, &package)?;
        manifest
    };
    println!("cargo:rerun-if-changed={}", archive.display());
    Ok((package, manifest))
}

fn run(fetch: &impl Fn(&str) -> Result<ureq::Body>) -> Result<()> {
    for variable in [
        "MOLI_SDK_ARTIFACT_DIR",
        "MOLI_SDK_CACHE_DIR",
        "MOLI_SDK_OFFLINE",
        "CARGO_NET_OFFLINE",
        "CARGO_HOME",
        "HOME",
        "USERPROFILE",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    println!("cargo:rerun-if-changed=artifacts.json");
    println!("cargo:rerun-if-changed=abi.txt");
    let target = env::var("TARGET")?;
    if !TARGETS.contains(&target.as_str()) {
        return Err(format!(
            "unsupported target {target}; supported: {}",
            TARGETS.join(", ")
        )
        .into());
    }
    if target.contains("windows")
        && !env::var("CARGO_CFG_TARGET_FEATURE")
            .unwrap_or_default()
            .split(',')
            .any(|feature| feature == "crt-static")
    {
        return Err("Windows SDK requires static MSVC CRT: configure the HOST PROJECT with rustflags = [\"-C\", \"target-feature=+crt-static\"] for its Windows target (both x86_64 and ARM64)".into());
    }
    let source =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing CARGO_MANIFEST_DIR")?);
    let abi = fs::read_to_string(source.join("abi.txt"))?;
    let abi = abi.trim();
    if abi.is_empty() {
        return Err("empty SDK ABI identity".into());
    }
    let (root, manifest) = if let Some(path) = env::var_os("MOLI_SDK_ARTIFACT_DIR") {
        let path = fs::canonicalize(path)?;
        println!(
            "cargo:warning=Moli SDK explicitly trusting local manifest at {} (integrity is not provenance)",
            path.display()
        );
        let manifest = validate(&path, &target, abi, None)?;
        (path, manifest)
    } else {
        let binding = read_json(&source.join("artifacts.json"))?;
        if binding["schema"] != 1 {
            return Err("unsupported SDK binding schema".into());
        }
        cached_artifact(&binding, &target, abi, fetch)?
    };
    // 目录监视同时覆盖新增文件，不能只监视已有清单中的路径。
    println!("cargo:rerun-if-changed={}", root.display());
    println!(
        "cargo:rustc-link-search=native={}",
        root.join("lib").display()
    );
    for library in manifest["libraries"]
        .as_array()
        .ok_or("missing libraries")?
    {
        // 最终宿主按名字静态链接；避免把完整原生归档再复制进 SDK rlib。
        println!(
            "cargo:rustc-link-lib=static:-bundle={}",
            text(library, "name")?
        );
    }
    for library in manifest["system_libraries"]
        .as_array()
        .ok_or("missing system libraries")?
    {
        println!(
            "cargo:rustc-link-lib={}",
            library.as_str().ok_or("invalid system library")?
        );
    }
    Ok(())
}
