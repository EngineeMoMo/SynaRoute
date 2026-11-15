use super::*;

fn link_dir_for_test(target: &std::path::Path, link: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return true;
        }
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

fn link_file_for_test(target: &std::path::Path, link: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_file(target, link).is_ok() {
            return true;
        }
        std::fs::hard_link(target, link).is_ok()
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

#[test]
fn vcs_directory_alias_is_denied_by_read_images_and_retrieval() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::write(root.join(".git/config"), "synthetic-private-config").unwrap();
    std::fs::write(root.join(".git/picture.png"), "synthetic-image").unwrap();
    if !link_dir_for_test(&root.join(".git"), &root.join("project-meta")) {
        eprintln!("skip: directory links are unavailable");
        return;
    }
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
    if !link_file_for_test(&root.join(".git/config"), &root.join("metadata.md")) {
        eprintln!("skip: file links are unavailable");
        return;
    }
    assert!(is_sensitive_read_path(&root.join("metadata.md")));
    assert!(resolve_readable(root, "metadata.md").is_err());
    assert!(resolve_readable(root, "readme.md").is_ok());
}
