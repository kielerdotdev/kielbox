use super::*;
use filetime::{FileTime, set_file_mtime};

fn age(path: &Path) {
    let metadata = fs::symlink_metadata(path).unwrap();
    if metadata.is_dir() && !is_link(&metadata) {
        for entry in fs::read_dir(path).unwrap() {
            age(&entry.unwrap().path());
        }
    }
    let old = FileTime::from_system_time(SystemTime::now() - Duration::from_secs(3 * 86400));
    if is_link(&metadata) {
        filetime::set_symlink_file_times(path, old, old).unwrap();
    } else {
        set_file_mtime(path, old).unwrap();
    }
}

fn options(root: &Path) -> Options {
    Options {
        roots: vec![root.to_owned()],
        older_than: Duration::from_secs(86400),
        kinds: vec![],
        allow_ignored_worktree_data: false,
    }
}

fn node(root: &Path) -> PathBuf {
    fs::write(root.join("package.json"), "{}").unwrap();
    let path = root.join("node_modules");
    fs::create_dir_all(path.join("package")).unwrap();
    fs::write(path.join("package/index.js"), "fixture").unwrap();
    age(&path);
    path
}

#[test]
fn plan_is_read_only_and_apply_reclaims_only_recognized_old_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    let modules = node(temp.path());
    fs::create_dir(temp.path().join("target")).unwrap(); // No Cargo.toml.
    fs::write(temp.path().join("source.js"), "keep").unwrap();
    let cancel = CancelToken::new();
    let plan = plan(&options(temp.path()), &cancel).unwrap();
    assert_eq!(plan.candidates().len(), 1);
    assert_eq!(plan.bytes(), 7);
    assert!(modules.exists());
    let result = apply(&plan, &cancel, |_| {}).unwrap();
    assert_eq!(result.removed.len(), 1);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(!modules.exists());
    assert!(temp.path().join("target").exists());
    assert!(temp.path().join("source.js").exists());
}

#[test]
fn new_descendant_and_changes_after_preview_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let modules = node(temp.path());
    let cancel = CancelToken::new();
    let old_plan = plan(&options(temp.path()), &cancel).unwrap();
    fs::write(modules.join("package/index.js"), "agent is using this").unwrap();
    // An old outer directory must not hide recent nested writes.
    assert!(
        plan(&options(temp.path()), &cancel)
            .unwrap()
            .candidates()
            .is_empty()
    );
    let outcome = apply(&old_plan, &cancel, |_| {}).unwrap();
    assert!(outcome.removed.is_empty());
    assert_eq!(outcome.errors.len(), 1);
    assert!(modules.exists());
}

#[test]
fn overlapping_roots_are_deduplicated_and_kind_filter_is_respected() {
    let temp = tempfile::tempdir().unwrap();
    let modules = node(temp.path());
    let cancel = CancelToken::new();
    let mut opts = options(temp.path());
    opts.roots.push(modules.clone());
    opts.roots.push(temp.path().to_owned());
    assert_eq!(plan(&opts, &cancel).unwrap().candidates().len(), 1);
    opts.roots = vec![temp.path().to_owned()];
    opts.kinds = vec![Kind::RustTarget];
    assert!(plan(&opts, &cancel).unwrap().candidates().is_empty());
}

#[test]
fn cancellation_never_deletes_and_bad_roots_fail() {
    let temp = tempfile::tempdir().unwrap();
    let modules = node(temp.path());
    let cancel = CancelToken::new();
    let plan = plan(&options(temp.path()), &cancel).unwrap();
    cancel.cancel();
    assert!(apply(&plan, &cancel, |_| {}).unwrap().cancelled);
    assert!(modules.exists());
    assert!(super::plan(&options(&temp.path().join("missing")), &CancelToken::new()).is_err());
}

fn fixture_git(path: &Path, args: &[&str]) {
    git(path, args).unwrap();
}

fn worktree(root: &Path) -> PathBuf {
    let repo = root.join("main");
    fs::create_dir(&repo).unwrap();
    fixture_git(&repo, &["init"]);
    fixture_git(&repo, &["config", "user.name", "Cleanup test"]);
    fixture_git(&repo, &["config", "user.email", "cleanup@example.invalid"]);
    fixture_git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("source"), "keep").unwrap();
    fixture_git(&repo, &["add", "."]);
    fixture_git(&repo, &["commit", "-m", "fixture"]);
    fixture_git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    fixture_git(&repo, &["worktree", "add", "-b", "agent", "../agent"]);
    let path = root.join("agent");
    let admin = worktree_admin(&path).unwrap();
    age(&path);
    age(&admin);
    path
}

#[test]
fn clean_old_worktree_is_removed_through_git_and_registration_disappears() {
    let temp = tempfile::tempdir().unwrap();
    let path = worktree(temp.path());
    let admin = worktree_admin(&path).unwrap();
    let cancel = CancelToken::new();
    let plan = plan(&options(temp.path()), &cancel).unwrap();
    assert_eq!(plan.candidates().len(), 1, "{:?}", plan.skipped);
    assert_eq!(plan.candidates()[0].kind, Kind::Worktree);
    let outcome = apply(&plan, &cancel, |_| {}).unwrap();
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert_eq!(outcome.removed.len(), 1);
    assert!(!path.exists());
    assert!(!admin.exists());
    assert!(temp.path().join("main/source").exists());
}

#[test]
fn locked_dirty_and_unpushed_worktrees_are_preserved_even_after_preview() {
    let temp = tempfile::tempdir().unwrap();
    let path = worktree(temp.path());
    let cancel = CancelToken::new();
    let original = plan(&options(temp.path()), &cancel).unwrap();
    fixture_git(&path, &["worktree", "lock", "."]);
    assert!(
        plan(&options(temp.path()), &cancel)
            .unwrap()
            .candidates()
            .is_empty()
    );
    assert_eq!(apply(&original, &cancel, |_| {}).unwrap().errors.len(), 1);
    fixture_git(&path, &["worktree", "unlock", "."]);
    fs::write(path.join("untracked"), "precious").unwrap();
    assert!(
        worktree_safe(&path, false)
            .unwrap_err()
            .to_string()
            .contains("untracked")
    );
    fixture_git(&path, &["add", "."]);
    fixture_git(&path, &["commit", "-m", "unpublished work"]);
    assert!(
        worktree_safe(&path, false)
            .unwrap_err()
            .to_string()
            .contains("remote-tracking")
    );
    assert!(path.exists());
}

#[test]
fn primary_repo_and_current_worktree_are_never_removal_targets() {
    let cwd = fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    let cancel = CancelToken::new();
    assert!(
        validate(
            &cwd,
            Kind::Worktree,
            std::slice::from_ref(&cwd),
            SystemTime::now(),
            &cancel,
            false
        )
        .unwrap_err()
        .to_string()
        .contains("working directory")
    );
    let temp = tempfile::tempdir().unwrap();
    worktree(temp.path());
    assert!(
        worktree_admin(&temp.path().join("main"))
            .unwrap_err()
            .to_string()
            .contains("primary")
    );
}

#[test]
fn package_links_do_not_delete_external_content_and_link_roots_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let modules = node(temp.path());
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("precious"), "keep").unwrap();
    let link = modules.join("linked-package");
    #[cfg(windows)]
    match std::os::windows::fs::symlink_dir(outside.path(), &link) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(1314) => {
            return;
        }
        Err(e) => panic!("{e}"),
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    age(&modules);
    let cancel = CancelToken::new();
    assert!(plan(&options(&link), &cancel).is_err());
    let plan = plan(&options(temp.path()), &cancel).unwrap();
    assert_eq!(plan.candidates().len(), 1);
    let outcome = apply(&plan, &cancel, |_| {}).unwrap();
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert!(outside.path().join("precious").exists());
}

#[test]
fn cancellation_after_first_removal_retains_ledger_and_preserves_remaining_target() {
    let temp = tempfile::tempdir().unwrap();
    for name in ["a", "b"] {
        let root = temp.path().join(name);
        fs::create_dir(&root).unwrap();
        node(&root);
    }
    let cancel = CancelToken::new();
    let plan = plan(&options(temp.path()), &cancel).unwrap();
    assert_eq!(plan.candidates().len(), 2);
    let outcome = apply(&plan, &cancel, |_| cancel.cancel()).unwrap();
    assert!(outcome.cancelled);
    assert!(outcome.errors.is_empty());
    assert_eq!(outcome.removed.len(), 1);
    assert_eq!(outcome.removed[0].path, plan.candidates()[0].path);
    assert_eq!(outcome.removed[0].bytes, 7);
    assert!(!plan.candidates()[0].path.exists());
    assert!(plan.candidates()[1].path.exists());
}

#[test]
fn ignored_worktree_data_requires_opt_in_and_is_rechecked_after_preview() {
    let temp = tempfile::tempdir().unwrap();
    let path = worktree(temp.path());
    let cancel = CancelToken::new();
    let original = plan(&options(temp.path()), &cancel).unwrap();
    // A repository-local exclude models ignored secrets/data without changing HEAD.
    let common = git_path(&path, "--git-common-dir").unwrap();
    fs::write(common.join("info/exclude"), "local-data\n").unwrap();
    fs::write(path.join("local-data"), "irreplaceable local data").unwrap();
    age(&path);
    age(&worktree_admin(&path).unwrap());
    let protected = plan(&options(temp.path()), &cancel).unwrap();
    assert!(protected.candidates().is_empty());
    assert!(
        protected
            .skipped
            .iter()
            .any(|n| n.reason.contains("ignored data"))
    );
    let outcome = apply(&original, &cancel, |_| {}).unwrap();
    assert!(outcome.removed.is_empty());
    assert!(outcome.errors[0].reason.contains("ignored data"));
    assert!(path.join("local-data").exists());
    let mut opted_in = options(temp.path());
    opted_in.allow_ignored_worktree_data = true;
    let approved = plan(&opted_in, &cancel).unwrap();
    assert_eq!(approved.candidates().len(), 1, "{:?}", approved.skipped);
    // Opt-in must still preserve dirty tracked content.
    fs::write(path.join("source"), "modified").unwrap();
    assert_eq!(apply(&approved, &cancel, |_| {}).unwrap().errors.len(), 1);
    fixture_git(&path, &["checkout", "--", "source"]);
    age(&path);
    age(&worktree_admin(&path).unwrap());
    let outcome = apply(&approved, &cancel, |_| {}).unwrap();
    assert_eq!(outcome.removed.len(), 1, "{:?}", outcome.errors);
    assert!(!path.exists());
}
