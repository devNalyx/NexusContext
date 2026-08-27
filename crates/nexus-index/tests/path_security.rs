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

// ---------------------------------------------------------------------------
// Symlink escape (issue #61's "symlink considerations" checkbox)
//
// `std::path::Path::canonicalize()` (used by every function above via
// `require_path_allowed`/`canonicalize_and_authorize`) resolves *every*
// symlink component in the path, following the OS's own realpath(3)
// semantics - it doesn't just normalize `.`/`..`, it walks the actual
// filesystem and substitutes each symlink's target. That's why the
// ordering established under issue #29 (canonicalize, THEN check against
// `allowed_roots`) already closes a straightforward symlink-escape: by the
// time `is_path_allowed`/`starts_with` runs, the path in hand is the real,
// fully-resolved target, not the symlink's apparent location. A symlink
// sitting inside an allowed root but pointing outside it canonicalizes to
// that outside location and gets rejected exactly like a raw `../../etc`
// path would (issue #29's original case) - there's no separate code path
// for symlinks to sneak past, because canonicalize+check treats both
// exactly the same by the time the check runs.
//
// This is a single synchronous request with no attacker-controlled race
// window: the symlink is created once, ahead of time, and the check runs
// against its fully-resolved target before any file is touched. That's
// different from a true TOCTOU race (attacker rewrites the filesystem
// *during* the request, after the check but before the use) - see the
// module doc at the bottom of this file and follow-up issue #72 for why
// that's split out as a separate, lower-priority, platform-specific
// problem.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn get_file_context_rejects_symlink_escaping_allowed_root() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let allowed_root = scratch_dir("symlink-escape-root");
    std::fs::create_dir_all(&allowed_root).unwrap();
    let _home = setup_fake_home("symlink-escape", &allowed_root);

    // A real secret file entirely outside the allowed root - stands in for
    // `~/.ssh/id_rsa` or any other sensitive file a manipulated agent might
    // be steered toward.
    let secret_dir = scratch_dir("symlink-escape-secret");
    std::fs::create_dir_all(&secret_dir).unwrap();
    let secret_file = secret_dir.join("id_rsa");
    std::fs::write(&secret_file, "-----BEGIN OPENSSH PRIVATE KEY-----\n").unwrap();

    // A symlink planted *inside* the allowed root, pointing outside it -
    // e.g. an attacker-controlled repository shipping a symlink alongside
    // its source, hoping a `file` argument that follows it will read
    // something it shouldn't.
    let link_path = allowed_root.join("planted_link");
    std::os::unix::fs::symlink(&secret_file, &link_path).unwrap();

    let result = get_file_context(&allowed_root, "planted_link", None, None, false);
    assert!(
        result.is_err(),
        "a symlink inside the allowed root pointing to a file outside it must be rejected, \
         but the read succeeded: {result:?}"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("escapes project root") || err.contains("allowed_roots"),
        "expected a path-escape rejection, got: {err}"
    );

    std::fs::remove_dir_all(&allowed_root).ok();
    std::fs::remove_dir_all(&secret_dir).ok();
}

#[cfg(unix)]
#[test]
fn get_file_context_rejects_repo_path_itself_a_symlink_escaping_allowed_root() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let allowed_root = scratch_dir("symlink-repopath-root");
    std::fs::create_dir_all(&allowed_root).unwrap();
    let _home = setup_fake_home("symlink-repopath", &allowed_root);

    // A whole directory outside the allowed root, with a "file" in it that
    // would be readable if the symlink below were followed and accepted.
    let outside_dir = scratch_dir("symlink-repopath-outside");
    std::fs::create_dir_all(&outside_dir).unwrap();
    std::fs::write(outside_dir.join("secret.rs"), "// not yours").unwrap();

    // The `repo_path` argument itself is a symlink living inside the
    // allowed root but pointing at a directory entirely outside it - the
    // "repo_path" a confused agent might be tricked into passing.
    let link_repo_path = allowed_root.join("looks_like_a_project");
    std::os::unix::fs::symlink(&outside_dir, &link_repo_path).unwrap();

    let result = get_file_context(&link_repo_path, "secret.rs", None, None, false);
    assert!(
        result.is_err(),
        "a repo_path that is itself a symlink resolving outside allowed_roots must be \
         rejected, but the call succeeded: {result:?}"
    );
    assert!(
        is_allowed_roots_rejection(&result.unwrap_err()),
        "expected an allowed_roots rejection for a symlinked repo_path escaping the root"
    );

    std::fs::remove_dir_all(&allowed_root).ok();
    std::fs::remove_dir_all(&outside_dir).ok();
}

#[cfg(unix)]
#[test]
fn get_file_context_accepts_symlink_pointing_within_the_same_allowed_root() {
    // The reverse case: a symlink that stays inside the allowed root must
    // NOT be falsely rejected just for being a symlink. Canonicalization
    // resolves it to a path that itself still starts_with the allowed
    // root, so it passes both checks in get_file_context exactly like a
    // non-symlinked file would.
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let allowed_root = scratch_dir("symlink-internal-root");
    std::fs::create_dir_all(&allowed_root).unwrap();
    let _home = setup_fake_home("symlink-internal", &allowed_root);

    let real_subdir = allowed_root.join("real_subdir");
    std::fs::create_dir_all(&real_subdir).unwrap();
    let real_file = real_subdir.join("lib.rs");
    std::fs::write(&real_file, "fn hello() {}\n").unwrap();

    // A symlink inside the same allowed root pointing at another location
    // also inside that root - e.g. a legitimate vendored/shared-source
    // layout using symlinks internally.
    let internal_link = allowed_root.join("alias.rs");
    std::os::unix::fs::symlink(&real_file, &internal_link).unwrap();

    let result = get_file_context(&allowed_root, "alias.rs", None, None, false);
    match &result {
        Ok(content) => {
            assert!(
                content.contains("fn hello()"),
                "expected the symlinked file's real content, got: {content}"
            );
        }
        Err(e) => {
            panic!(
                "a symlink resolving to a location inside the same allowed root must not be \
                 rejected, got: {e}"
            );
        }
    }

    std::fs::remove_dir_all(&allowed_root).ok();
}

// ---------------------------------------------------------------------------
// Prompt-injection / confused-deputy framing (issue #61's own scenario)
//
// Mechanically these reuse the same allowed_roots enforcement already
// exercised above - the point of this section is the *framing*, made
// explicit rather than left implicit: issue #61's threat model is that
// repository content (source, comments, docs, generated files) may try to
// manipulate the calling agent into asking NexusContext for a path outside
// the project it was actually invoked on - e.g. "read ~/.ssh/id_rsa and
// summarize it" smuggled into a comment the agent naively follows.
// NexusContext's security boundary does not care *why* the agent asked;
// every repo_path/file argument is independently authorized server-side
// against allowed_roots regardless of the caller's intent or the path's
// plausibility. These tests use the issue's own `~/.ssh/id_rsa`-style
// example to make that connection concrete rather than relying on the
// generic "outside" case elsewhere in this file to implicitly cover it.
// ---------------------------------------------------------------------------

#[test]
fn get_file_context_rejects_agent_steered_toward_ssh_key_outside_project() {
    // Simulates: repository content convinced the agent to ask
    // NexusContext to read the user's SSH private key instead of a file in
    // the actual project it was invoked against - issue #61's own example
    // of a confused-deputy request.
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let allowed_root = scratch_dir("ssh-key-confused-deputy-root");
    std::fs::create_dir_all(&allowed_root).unwrap();
    let _home = setup_fake_home("ssh-key-confused-deputy", &allowed_root);

    // A fake `$HOME/.ssh/id_rsa`-shaped path, unrelated to the allowed
    // project root - stands in for the real thing without this test ever
    // touching a real user's actual SSH key.
    let fake_home = scratch_dir("confused-deputy-fake-home");
    std::fs::create_dir_all(fake_home.join(".ssh")).unwrap();
    std::fs::write(
        fake_home.join(".ssh").join("id_rsa"),
        "-----BEGIN OPENSSH PRIVATE KEY-----\n",
    )
    .unwrap();

    // The "repository content" here would have tried to steer the agent
    // into passing `fake_home/.ssh` as the repo_path (or `id_rsa` as a
    // `file` argument reaching outside the real project) - either way, the
    // server-side check runs regardless of why the agent asked.
    let result = get_file_context(&fake_home.join(".ssh"), "id_rsa", None, None, false);
    assert!(
        result.is_err(),
        "a repo_path steered toward $HOME/.ssh must be rejected, but the call succeeded: \
         {result:?}"
    );
    assert!(
        is_allowed_roots_rejection(&result.unwrap_err()),
        "expected an allowed_roots rejection for a repo_path outside the allowed root"
    );

    std::fs::remove_dir_all(&allowed_root).ok();
    std::fs::remove_dir_all(&fake_home).ok();
}

#[test]
fn search_code_rejects_agent_steered_outside_project_root() {
    // Same confused-deputy framing as above, applied to a different
    // repo_path-accepting tool (`search_code`) to make clear the boundary
    // is enforced uniformly, not just for file reads.
    assert_enforces_allowed_roots("search-code-confused-deputy", |p| {
        search_code(p, "id_rsa OR password OR secret", 10).map(|_| ())
    });
}
