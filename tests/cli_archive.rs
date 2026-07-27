use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn backup_verify_list_and_recover_round_trip() {
    let fixture = Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"abcdefghijk",
    )
    .unwrap();
    fs::write(
        fixture.claude.join("projects/demo/duplicate.jsonl"),
        b"abcdefghijk",
    )
    .unwrap();
    fs::write(fixture.claude.join("projects/demo/empty.jsonl"), b"").unwrap();
    fs::write(
        fixture.claude.join(".credentials.json"),
        b"must-not-back-up",
    )
    .unwrap();

    let first = fixture.backup();
    assert_eq!(first.files, 3);
    assert_eq!(first.logical_bytes, 22);
    assert!(first.new_objects > 0);
    assert!(first.unique_objects < first.chunk_references);

    let snapshots = snapshot_list(&fixture);
    assert_eq!(
        snapshots[0].verification,
        akeep::archive::VerificationLevel::Quick
    );
    assert!(snapshots[0].full_verified_at.is_none());

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "verify",
            "latest",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Verified recovery point"));
    let snapshots = snapshot_list(&fixture);
    assert_eq!(
        snapshots[0].verification,
        akeep::archive::VerificationLevel::Full
    );
    assert!(snapshots[0].full_verified_at.is_some());

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "snapshots",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(&first.snapshot_id));

    let recovery = fixture.temp.path().join("recovery");
    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "recover",
            "latest",
            "--to",
            recovery.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert_eq!(
        fs::read(recovery.join("claude-code/projects/demo/session.jsonl")).unwrap(),
        b"abcdefghijk"
    );
    assert_eq!(
        fs::read(recovery.join("claude-code/projects/demo/duplicate.jsonl")).unwrap(),
        b"abcdefghijk"
    );
    assert_eq!(
        fs::read(recovery.join("claude-code/projects/demo/empty.jsonl")).unwrap(),
        b""
    );
    assert!(!recovery.join("claude-code/.credentials.json").exists());
    assert!(!recovery.join(".akeep-recovery-incomplete").exists());

    let second = fixture.backup();
    assert_eq!(second.new_objects, 0);
    assert_eq!(second.new_stored_bytes, 0);
}

#[test]
fn full_verify_detects_corruption() {
    let fixture = Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"important",
    )
    .unwrap();
    let backup = fixture.backup();
    let manifest = load_manifest(&fixture.vault, &backup.snapshot_id);
    let object = &manifest.files[0].chunks[0].id;
    let object_path = fixture
        .vault
        .join("objects")
        .join(&object[..2])
        .join(format!("{}.zst", &object[2..]));
    fs::write(object_path, b"corrupt").unwrap();

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "verify",
            "latest",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("decompress"));
}

#[test]
fn failed_recovery_keeps_an_incomplete_marker() {
    let fixture = Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"abcdefghijkl",
    )
    .unwrap();
    let backup = fixture.backup();
    let manifest = load_manifest(&fixture.vault, &backup.snapshot_id);
    let object = &manifest.files[0].chunks[1].id;
    let object_path = fixture
        .vault
        .join("objects")
        .join(&object[..2])
        .join(format!("{}.zst", &object[2..]));
    fs::write(object_path, b"corrupt").unwrap();
    let recovery = fixture.temp.path().join("recovery");

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "recover",
            "latest",
            "--to",
            recovery.to_str().unwrap(),
        ])
        .assert()
        .failure();

    assert!(recovery.join(".akeep-recovery-incomplete").is_file());
    assert!(
        recovery
            .join("claude-code/projects/demo/session.jsonl")
            .is_file()
    );
}

#[test]
fn full_verify_detects_reordered_objects() {
    let fixture = Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"abcdefghijkl",
    )
    .unwrap();
    let backup = fixture.backup();
    let mut manifest = load_manifest(&fixture.vault, &backup.snapshot_id);
    manifest.files[0].chunks.swap(0, 1);
    fs::write(
        fixture
            .vault
            .join("manifests")
            .join(format!("{}.json", backup.snapshot_id)),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "verify",
            "latest",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("file hash mismatch"));
}

#[test]
fn recover_refuses_a_nonempty_target() {
    let fixture = Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"important",
    )
    .unwrap();
    fixture.backup();
    let recovery = fixture.temp.path().join("recovery");
    fs::create_dir(&recovery).unwrap();
    fs::write(recovery.join("keep.txt"), b"do not overwrite").unwrap();

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "recover",
            "latest",
            "--to",
            recovery.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not empty"));
    assert_eq!(
        fs::read(recovery.join("keep.txt")).unwrap(),
        b"do not overwrite"
    );
}

#[test]
fn recover_refuses_a_target_inside_the_vault() {
    let fixture = Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"important",
    )
    .unwrap();
    fixture.backup();
    let recovery = fixture.vault.join("recovery");

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "recover",
            "latest",
            "--to",
            recovery.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("overlaps vault"));
    assert!(!recovery.exists());
}

#[test]
fn age_vault_encrypts_objects_and_manifests_and_recovers() {
    let temp = TempDir::new().unwrap();
    let config_path = temp.path().join("config.toml");
    let vault = temp.path().join("vault");
    let claude = temp.path().join("claude");
    fs::create_dir_all(claude.join("projects/demo")).unwrap();
    fs::write(
        claude.join("projects/demo/session.jsonl"),
        b"private transcript",
    )
    .unwrap();

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "init",
            "--target",
            vault.to_str().unwrap(),
            "--encryption",
            "age",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Recovery identity:"));

    let mut config = akeep::config::Config::load(&config_path).unwrap();
    let identity = config.encryption.identity_file.clone().unwrap();
    config.archive.chunk_size = 4;
    config.sources.claude_home = Some(claude);
    config.sources.codex_home = Some(temp.path().join("missing-codex"));
    config.sources.grok_home = Some(temp.path().join("missing-grok"));
    config.sources.kimi_home = Some(temp.path().join("missing-kimi"));
    config.sources.opencode_share = Some(temp.path().join("missing-opencode-share"));
    config.sources.opencode_state = Some(temp.path().join("missing-opencode-state"));
    fs::write(&config_path, toml::to_string_pretty(&config).unwrap()).unwrap();

    let output = Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "backup",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: akeep::archive::BackupReport = serde_json::from_slice(&output.stdout).unwrap();
    let manifest_path = vault
        .join("manifests")
        .join(format!("{}.json.age", report.snapshot_id));
    let manifest_ciphertext = fs::read(&manifest_path).unwrap();
    assert!(manifest_ciphertext.starts_with(b"age-encryption.org/v1"));
    assert!(
        !String::from_utf8_lossy(&manifest_ciphertext).contains("session.jsonl"),
        "encrypted manifest leaked a logical path"
    );

    let recovery = temp.path().join("recovery");
    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "verify",
            "latest",
        ])
        .assert()
        .success();
    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "recover",
            "latest",
            "--to",
            recovery.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert_eq!(
        fs::read(recovery.join("claude-code/projects/demo/session.jsonl")).unwrap(),
        b"private transcript"
    );

    let hidden_identity = identity.with_extension("hidden");
    fs::rename(&identity, &hidden_identity).unwrap();
    Command::cargo_bin("akeep")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap(), "verify"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("age identity"));
}

struct Fixture {
    temp: TempDir,
    config: PathBuf,
    vault: PathBuf,
    claude: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let vault = temp.path().join("vault");
        let claude = temp.path().join("claude");
        fs::create_dir_all(claude.join("projects/demo")).unwrap();

        Command::cargo_bin("akeep")
            .unwrap()
            .args([
                "--config",
                config.to_str().unwrap(),
                "init",
                "--target",
                vault.to_str().unwrap(),
            ])
            .assert()
            .success();

        let mut active = akeep::config::Config::load(&config).unwrap();
        active.archive.chunk_size = 4;
        active.sources.claude_home = Some(claude.clone());
        active.sources.codex_home = Some(temp.path().join("missing-codex"));
        active.sources.grok_home = Some(temp.path().join("missing-grok"));
        active.sources.kimi_home = Some(temp.path().join("missing-kimi"));
        active.sources.opencode_share = Some(temp.path().join("missing-opencode-share"));
        active.sources.opencode_state = Some(temp.path().join("missing-opencode-state"));
        fs::write(&config, toml::to_string_pretty(&active).unwrap()).unwrap();

        Self {
            temp,
            config,
            vault,
            claude,
        }
    }

    fn backup(&self) -> akeep::archive::BackupReport {
        let output = Command::cargo_bin("akeep")
            .unwrap()
            .args([
                "--config",
                self.config.to_str().unwrap(),
                "backup",
                "--json",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn load_manifest(vault: &Path, snapshot_id: &str) -> akeep::manifest::Manifest {
    let path = vault.join("manifests").join(format!("{snapshot_id}.json"));
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn snapshot_list(fixture: &Fixture) -> Vec<akeep::archive::SnapshotInfo> {
    let output = Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "snapshots",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
