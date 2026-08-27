//! Consolidated adversarial test suite for issue #61: every MCP tool
//! function that accepts a caller-supplied `repo_path` must enforce
//! `allowed_roots` the same way, with the same canonicalize-before-check
//! ordering (issue #29's fix). Covers the two functions that were already
//! correct (`get_file_context`, `detect_changes`) plus the five that were
//! not (`search_code`, `get_architecture`, `detect_dead_code`,
//! `call_graph_dot`, `run_query`/`run_cypher_query`).
//!
//! `nexus_core::Paths::resolve()` (what every one of these functions calls
//! internally) resolves its config directory from `$HOME` on Unix via the
//! `directories` crate - there's no injectable override, so these tests
//! redirect `HOME` to a scratch directory for the duration of each test and
//! write a `config.toml` there with a controlled `allowed_roots`. That env
//! var is process-global, so `ENV_LOCK` below serializes every test in this
//! binary against it (integration test binaries otherwise run their `#[test]`
//! functions on multiple threads); it's still `#[cfg(unix)]`-only because
//! Windows' known-folder config lookup isn't `HOME`-overridable the same
//! way (see `nexus_core::paths::Paths::resolve`).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use nexus_index::{
    call_graph_dot, detect_changes, detect_dead_code, get_architecture, get_file_context,
    run_cypher_query, search_code,
};

/// Serializes every test below against the process-global `HOME` env var
/// mutation each one performs.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn scratch_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "nexuscontext-mcp-pathsec-{label}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ))
}

/// Sets up a fake `$HOME` with `config.toml` configured with
/// `allowed_roots = [allowed_root]` at whatever path
/// `nexus_core::Paths::resolve()` actually resolves for that `$HOME`, and
/// returns the fake home dir plus a guard that restores the previous
/// `$HOME`/`$XDG_CONFIG_HOME` on drop.
///
/// This must go through `Paths::resolve()` itself rather than hardcoding
/// `$HOME/.config/nexuscontext` - `directories::ProjectDirs` (what
/// `Paths::resolve()` uses) honors `$XDG_CONFIG_HOME` over `$HOME` on
/// Linux when set (as some CI runners do), and uses an entirely different
/// layout on macOS (`~/Library/Application Support`, not `~/.config`).
/// Hardcoding the Linux path silently no-ops the whole test on both: the
/// fake config is never found, `allowed_roots` falls back to "unrestricted"
/// (its documented behavior for an absent/empty config), and every
/// "must be rejected" assertion below fails as "the call succeeded".
struct FakeHome {
    prev_home: Option<std::ffi::OsString>,
    prev_xdg_config_home: Option<std::ffi::OsString>,
    _dir: PathBuf,
}

impl Drop for FakeHome {
    fn drop(&mut self) {
        match &self.prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match &self.prev_xdg_config_home {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}

fn setup_fake_home(label: &str, allowed_root: &Path) -> FakeHome {
    let home = scratch_dir(&format!("home-{label}"));
    std::fs::create_dir_all(&home).unwrap();

    let prev_home = std::env::var_os("HOME");
    let prev_xdg_config_home = std::env::var_os("XDG_CONFIG_HOME");
    std::env::set_var("HOME", &home);
    // A stray XDG_CONFIG_HOME from the outer environment would otherwise
    // keep pointing at the real config dir even after $HOME is redirected.
    std::env::remove_var("XDG_CONFIG_HOME");

    let config_file = nexus_core::Paths::resolve().config_file();
    std::fs::create_dir_all(config_file.parent().unwrap()).unwrap();
    let config_toml = format!("allowed_roots = [{:?}]\n", allowed_root.to_string_lossy());
    std::fs::write(&config_file, config_toml).unwrap();

    FakeHome {
        prev_home,
        prev_xdg_config_home,
        _dir: home,
    }
}

/// True if `err` is the "outside the configured allowed_roots" rejection
/// from `nexus_index::project::require_path_allowed`, as opposed to some
/// later error (e.g. "no index found") that means the allowed_roots check
/// itself was passed.
fn is_allowed_roots_rejection(err: &anyhow::Error) -> bool {
    err.to_string().contains("allowed_roots")
}

/// One adversarial case, run against every repo_path-accepting function.
enum Case {
    /// A path with no relation to the allowed root at all - must be
    /// rejected.
    Outside,
    /// A `..`-traversal path that resolves outside the allowed root once
    /// canonicalized - must be rejected even though a naive
    /// `starts_with` on the raw path would have accepted it (issue #29).
    DotDotTraversal,
    /// A genuine subdirectory of the allowed root - must be accepted (the
    /// allowed_roots check must pass; any later "no index found" is fine
    /// and expected since nothing was actually indexed here).
    AllowedSubdir,
}

/// Builds the on-disk layout for one case and returns the `repo_path` to
/// pass to the function under test, plus whether that path should be
/// accepted by the allowed_roots check.
fn build_case(label: &str, case: Case) -> (PathBuf, PathBuf, bool) {
    let root = scratch_dir(&format!("root-{label}"));
    std::fs::create_dir_all(&root).unwrap();

    match case {
        Case::Outside => {
            let outside = scratch_dir(&format!("outside-{label}"));
            std::fs::create_dir_all(&outside).unwrap();
            (root, outside, false)
        }
        Case::DotDotTraversal => {
            let outside = scratch_dir(&format!("outside-dotdot-{label}"));
            std::fs::create_dir_all(&outside).unwrap();
            let escaping = root.join("..").join(outside.file_name().unwrap());
            (root, escaping, false)
        }
        Case::AllowedSubdir => {
            let nested = root.join("nested").join("project");
            std::fs::create_dir_all(&nested).unwrap();
            (root.clone(), nested, true)
        }
    }
}

/// Runs the three adversarial cases against one `repo_path`-accepting
/// closure, asserting the allowed_roots check behaves identically no
/// matter which of the seven functions it wraps.
fn assert_enforces_allowed_roots(label: &str, call: impl Fn(&Path) -> anyhow::Result<()>) {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    for (case_name, case) in [
        ("outside", Case::Outside),
        ("dotdot", Case::DotDotTraversal),
        ("subdir", Case::AllowedSubdir),
    ] {
        let (allowed_root, repo_path, should_pass) =
            build_case(&format!("{label}-{case_name}"), case);
        let _home = setup_fake_home(&format!("{label}-{case_name}"), &allowed_root);

        let result = call(&repo_path);

        if should_pass {
            assert!(
                result.is_err(),
                "{label}/{case_name}: expected the wrapped call to fail past the \
                 allowed_roots check (nothing is actually indexed here), but it \
                 succeeded outright"
            );
            let err = result.unwrap_err();
            assert!(
                !is_allowed_roots_rejection(&err),
                "{label}/{case_name}: a genuine subdirectory of an allowed root must \
                 pass the allowed_roots check, got: {err}"
            );
        } else {
            assert!(
                result.is_err(),
                "{label}/{case_name}: a repo_path outside allowed_roots must be \
                 rejected, but the call succeeded"
            );
            let err = result.unwrap_err();
            assert!(
                is_allowed_roots_rejection(&err),
                "{label}/{case_name}: expected an allowed_roots rejection, got: {err}"
            );
        }

        std::fs::remove_dir_all(&allowed_root).ok();
    }
}

#[test]
fn search_code_enforces_allowed_roots() {
    assert_enforces_allowed_roots("search_code", |p| {
        search_code(p, "anything", 10).map(|_| ())
    });
}

#[test]
fn get_architecture_enforces_allowed_roots() {
    assert_enforces_allowed_roots("get_architecture", |p| get_architecture(p).map(|_| ()));
}

#[test]
fn detect_dead_code_enforces_allowed_roots() {
    assert_enforces_allowed_roots("detect_dead_code", |p| detect_dead_code(p).map(|_| ()));
}

#[test]
fn call_graph_dot_enforces_allowed_roots() {
    assert_enforces_allowed_roots("call_graph_dot", |p| {
        call_graph_dot(p, "some_fn", nexus_index::Direction::Outbound, 2).map(|_| ())
    });
}

#[test]
fn run_cypher_query_enforces_allowed_roots() {
    assert_enforces_allowed_roots("run_cypher_query", |p| {
        run_cypher_query(p, "MATCH (a:Function)-[:CALLS]->(b:Function) RETURN a", 10).map(|_| ())
    });
}

#[test]
fn get_file_context_enforces_allowed_roots() {
    assert_enforces_allowed_roots("get_file_context", |p| {
        get_file_context(p, "some_file.rs", None, None, false).map(|_| ())
    });
}

#[test]
fn detect_changes_enforces_allowed_roots() {
    assert_enforces_allowed_roots("detect_changes", |p| detect_changes(p).map(|_| ()));
}
