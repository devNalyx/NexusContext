//! THROWAWAY probe for issue #83: checks whether GitHub Actions' Windows
//! runners can create symlinks without elevation. Remove once the
//! `test-windows` CI result has been observed.
#![cfg(windows)]

#[test]
fn windows_runner_can_create_symlink_without_elevation() {
    let dir =
        std::env::temp_dir().join(format!("nexuscontext-symlink-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("target.txt");
    std::fs::write(&target, b"hello").unwrap();
    let link = dir.join("link.txt");

    let result = std::os::windows::fs::symlink_file(&target, &link);
    assert!(
        result.is_ok(),
        "symlink_file failed on this Windows runner: {result:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
