//! End-to-end CLI tests that drive the built binary against a temp dir of
//! fake "layers". We never actually call HcsDestroyLayer here — that would
//! require Administrator and modify global state.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

fn make_fake_layers_dir() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("aaaa")).unwrap();
    fs::write(dir.path().join("aaaa").join("payload"), vec![0u8; 4096]).unwrap();
    fs::create_dir(dir.path().join("bbbb")).unwrap();
    fs::write(dir.path().join("bbbb").join("payload"), vec![0u8; 8192]).unwrap();
    dir
}

#[test]
fn list_outputs_each_fake_layer() {
    let dir = make_fake_layers_dir();
    Command::cargo_bin("dcmfree")
        .unwrap()
        .arg("list")
        .arg("--layers-dir")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("aaaa"))
        .stdout(predicate::str::contains("bbbb"));
}

#[test]
fn info_reports_two_layers_and_total_size() {
    let dir = make_fake_layers_dir();
    Command::cargo_bin("dcmfree")
        .unwrap()
        .arg("info")
        .arg("--layers-dir")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Total layers:     2"))
        .stdout(predicate::str::contains("Reclaimable:"));
}

#[test]
fn info_json_emits_valid_json_with_expected_fields() {
    let dir = make_fake_layers_dir();
    let assert = Command::cargo_bin("dcmfree")
        .unwrap()
        .arg("info")
        .arg("--json")
        .arg("--layers-dir")
        .arg(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(parsed["layer_count"], 2);
    assert!(parsed["total_bytes"].as_u64().unwrap() >= 12_288);
}

#[test]
fn clean_dry_run_does_not_modify_filesystem() {
    let dir = make_fake_layers_dir();
    Command::cargo_bin("dcmfree")
        .unwrap()
        .arg("clean")
        .arg("--dry-run")
        .arg("--min-age")
        .arg("0")
        .arg("--layers-dir")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("--dry-run set"));

    // Both subdirs should still exist after the dry run.
    assert!(dir.path().join("aaaa").is_dir());
    assert!(dir.path().join("bbbb").is_dir());
}

#[test]
fn clean_without_yes_and_no_tty_aborts() {
    let dir = make_fake_layers_dir();
    Command::cargo_bin("dcmfree")
        .unwrap()
        .arg("clean")
        .arg("--min-age")
        .arg("0")
        .arg("--layers-dir")
        .arg(dir.path())
        .write_stdin("")
        .assert()
        // Either it aborts cleanly (Ok) or refuses for lack of elevation —
        // both outcomes leave the filesystem untouched, which is what we
        // really care about. Stdout/stderr should not say "Destroyed".
        .stdout(predicate::str::contains("Destroyed").not());

    assert!(dir.path().join("aaaa").is_dir());
    assert!(dir.path().join("bbbb").is_dir());
}

#[test]
fn missing_layers_dir_produces_clear_error() {
    Command::cargo_bin("dcmfree")
        .unwrap()
        .arg("list")
        .arg("--layers-dir")
        .arg(r"C:\definitely-not-a-real-dir-for-dcmfree-test")
        .assert()
        .failure()
        .stderr(predicate::str::contains("layers directory not found"));
}

#[test]
fn help_succeeds() {
    Command::cargo_bin("dcmfree")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("dcmfree"));
}

#[test]
fn version_succeeds() {
    Command::cargo_bin("dcmfree")
        .unwrap()
        .arg("--version")
        .assert()
        .success();
}
