//! Closure guard for the renderer-neutral callback kernel itself.
//!
//! The kernel may temporarily root one dynamically resolved callback-interface
//! operation, but it delegates the actual V8 call to a renderer adapter. It
//! must not grow a hidden invocation, scheduler, or browser-owner policy.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[test]
fn callback_kernel_raw_function_boundary_is_frozen() {
    assert_eq!(
        production_source_inventory(count_raw_global_functions),
        BTreeMap::from([("invocation.rs".to_owned(), 1)]),
        "the callback kernel may only root the one dynamically resolved interface operation"
    );
}

#[test]
fn callback_kernel_contains_no_direct_v8_call() {
    assert!(
        production_source_inventory(count_direct_v8_calls).is_empty(),
        "the callback kernel must delegate V8 invocation to the renderer-owned adapter"
    );
}

fn production_source_inventory(count: fn(&str) -> usize) -> BTreeMap<String, usize> {
    // Do not embed CARGO_MANIFEST_DIR: a shared target directory may reuse this
    // binary across worktrees after its build-time worktree has been removed.
    // Nextest starts package-selected runs from the package root and workspace
    // runs from the workspace root, so resolve both shapes at runtime.
    let current_dir = std::env::current_dir()
        .expect("callback crate package or workspace directory should be available");
    let source_root = callback_source_root_from(&current_dir).unwrap_or_else(|| {
        panic!(
            "callback crate source directory should be discoverable from {}",
            current_dir.display()
        )
    });
    fs::read_dir(&source_root)
        .expect("callback crate source directory should exist")
        .map(|entry| entry.expect("callback crate source entry should be readable"))
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                return None;
            }
            let file_name = path.file_name()?.to_str()?;
            if matches!(file_name, "source_boundary_tests.rs" | "tests.rs") {
                return None;
            }
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            let matches = count(&source);
            (matches > 0).then(|| (file_name.to_owned(), matches))
        })
        .collect()
}

fn callback_source_root_from(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|ancestor| {
        [
            ancestor.join("src"),
            ancestor.join("moli-webidl-callback").join("src"),
        ]
        .into_iter()
        .find(|candidate| {
            candidate.join("lib.rs").is_file() && candidate.join("invocation.rs").is_file()
        })
    })
}

#[test]
fn callback_source_root_resolves_package_and_workspace_invocations() {
    let current_dir = std::env::current_dir()
        .expect("callback crate package or workspace directory should be available");
    let source_root = callback_source_root_from(&current_dir)
        .expect("callback crate source directory should be discoverable");
    let package_root = source_root
        .parent()
        .expect("callback source directory should have a package root");
    let workspace_root = package_root
        .parent()
        .expect("callback package directory should have a workspace root");

    assert_eq!(
        callback_source_root_from(package_root),
        Some(source_root.clone())
    );
    assert_eq!(callback_source_root_from(workspace_root), Some(source_root));
}

fn compact_source(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn count_raw_global_functions(source: &str) -> usize {
    let source = compact_source(source);
    source.matches(concat!("Global<", "v8::Function>")).count()
        + source.matches(concat!("Global<", "Function>")).count()
}

fn count_direct_v8_calls(source: &str) -> usize {
    let source = compact_source(source);
    source.matches(concat!(".call(", "scope")).count()
        + source.matches(concat!(".call(&", "scope")).count()
        + source.matches(concat!(".call(&mut", "scope")).count()
}
