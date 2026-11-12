//! The worker carries upstream dexdec's version: its package version is the
//! root workspace's `[workspace.package] version`.

#[test]
fn the_worker_version_is_upstream_dexdecs() {
    let root =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../Cargo.toml")).unwrap();
    let package = root
        .split("[workspace.package]")
        .nth(1)
        .expect("[workspace.package] in the root Cargo.toml");
    let version = package
        .lines()
        .find_map(|line| line.trim().strip_prefix("version = \"")?.strip_suffix('"'))
        .expect("a version in [workspace.package]");
    assert_eq!(env!("CARGO_PKG_VERSION"), version);
}
