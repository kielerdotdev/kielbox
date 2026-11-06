use std::{
    fs,
    path::Path,
    process::{Command, Output},
    time::{Duration, SystemTime},
};

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_clean"))
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn json_preview_is_read_only_and_apply_removes_only_old_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("package.json"), "{}").unwrap();
    let modules = temp.path().join("node_modules");
    fs::create_dir(&modules).unwrap();
    let file = modules.join("fixture.js");
    fs::write(&file, "fixture").unwrap();
    let old =
        filetime::FileTime::from_system_time(SystemTime::now() - Duration::from_secs(40 * 86400));
    filetime::set_file_mtime(&file, old).unwrap();
    filetime::set_file_mtime(&modules, old).unwrap();
    let output = run(temp.path(), &["--json"]);
    assert!(output.status.success(), "{:?}", output);
    let preview: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(preview["applied"], false);
    assert_eq!(preview["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(preview["logical_bytes"], 7);
    assert!(file.exists());
    let output = run(temp.path(), &["--json", "--apply"]);
    assert!(output.status.success(), "{:?}", output);
    let applied: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["removed"].as_array().unwrap().len(), 1);
    assert!(!modules.exists());
    assert!(temp.path().join("package.json").exists());
}

#[test]
fn invalid_arguments_fail_before_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        vec!["--apply", "--dry-run"],
        vec!["--older-than", "0"],
        vec!["--kind", "everything"],
    ] {
        assert_eq!(run(temp.path(), &args).status.code(), Some(2));
    }
    let output = run(&temp.path().join("missing"), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["success"], false);
}
