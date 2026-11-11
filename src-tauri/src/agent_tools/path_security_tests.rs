use super::*;

#[test]
fn vcs_directory_alias_is_denied_by_read_images_and_retrieval() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::write(root.join(".git/config"), "synthetic-private-config").unwrap();
    std::fs::write(root.join(".git/picture.png"), "synthetic-image").unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(root.join(".git"), root.join("project-meta")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join(".git"), root.join("project-meta")).unwrap();
    let environment = ToolEnv::bare(root);
    for path in [".git/config", "project-meta/config"] {
        assert!(read_file_tool(&environment, &json!({"path": path})).is_err());
        assert!(is_sensitive_read_path(&root.join(path)));
    }
    assert!(load_images(root.to_str(), &["project-meta/picture.png".into()]).is_err());
}

#[test]
fn vcs_file_alias_is_denied_but_regular_files_remain_readable() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::write(root.join(".git/config"), "synthetic-private-config").unwrap();
    std::fs::write(root.join("readme.md"), "public-text").unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(root.join(".git/config"), root.join("metadata.md")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join(".git/config"), root.join("metadata.md")).unwrap();
    assert!(is_sensitive_read_path(&root.join("metadata.md")));
    assert!(resolve_readable(root, "metadata.md").is_err());
    assert!(resolve_readable(root, "readme.md").is_ok());
}
