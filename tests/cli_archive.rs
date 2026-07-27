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
            "fsck",
            "latest",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Checked commit"));
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
            "log",
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
            "checkout",
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
fn recover_can_select_one_provider_without_marking_full_verification() {
    let fixture = Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"claude history",
    )
    .unwrap();
    let codex = fixture.temp.path().join("codex");
    fs::create_dir_all(codex.join("sessions")).unwrap();
    fs::write(codex.join("sessions/session.jsonl"), b"codex history").unwrap();

    let mut config = akeep::config::Config::load(&fixture.config).unwrap();
    config.sources.codex_home = Some(codex);
    fs::write(&fixture.config, toml::to_string_pretty(&config).unwrap()).unwrap();
    fixture.backup();

    let recovery = fixture.temp.path().join("provider-recovery");
    let output = Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "checkout",
            "latest",
            "--provider",
            "claude-code",
            "--to",
            recovery.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: akeep::archive::RecoveryReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report.provider,
        Some(akeep::providers::Provider::ClaudeCode)
    );
    assert_eq!(report.files, 1);
    assert_eq!(report.logical_bytes, 14);
    assert!(
        recovery
            .join("claude-code/projects/demo/session.jsonl")
            .is_file()
    );
    assert!(!recovery.join("codex").exists());
    assert_eq!(
        snapshot_list(&fixture)[0].verification,
        akeep::archive::VerificationLevel::Quick
    );
}

#[test]
fn recover_rejects_a_provider_absent_from_the_snapshot_before_creating_target() {
    let fixture = Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"claude history",
    )
    .unwrap();
    fixture.backup();
    let recovery = fixture.temp.path().join("provider-recovery");

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "checkout",
            "latest",
            "--provider",
            "codex",
            "--to",
            recovery.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("contains no codex files"));
    assert!(!recovery.exists());
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
            "fsck",
            "latest",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("decompress"));
}

#[test]
fn repeated_chunks_in_one_parallel_batch_are_stored_once() {
    let fixture = Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"abcdabcdabcdabcdabcdabcdabcdabcd",
    )
    .unwrap();
    let backup = fixture.backup();

    assert_eq!(backup.chunk_references, 8);
    assert_eq!(backup.unique_objects, 1);
    assert_eq!(backup.new_objects, 1);
    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "fsck",
            "latest",
        ])
        .assert()
        .success();
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
            "checkout",
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
            "fsck",
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
            "checkout",
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

#[cfg(unix)]
#[test]
fn recover_refuses_a_symlinked_target() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"important",
    )
    .unwrap();
    fixture.backup();
    let real_target = fixture.temp.path().join("real-recovery");
    let recovery_link = fixture.temp.path().join("recovery-link");
    fs::create_dir(&real_target).unwrap();
    symlink(&real_target, &recovery_link).unwrap();

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "checkout",
            "latest",
            "--to",
            recovery_link.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("real directory"));
    assert!(fs::read_dir(real_target).unwrap().next().is_none());
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
            "checkout",
            "latest",
            "--to",
            recovery.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("overlaps repository/state"));
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
            "commit",
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
        .args(["--config", config_path.to_str().unwrap(), "fsck", "latest"])
        .assert()
        .success();
    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "checkout",
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

    let clone = temp.path().join("encrypted-clone");
    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "clone",
            clone.to_str().unwrap(),
        ])
        .assert()
        .success();
    let clone_config_path = clone.join("config.toml");
    let clone_config = akeep::config::Config::load(&clone_config_path).unwrap();
    assert_eq!(
        clone_config.encryption.identity_file.as_ref(),
        Some(&identity)
    );
    assert!(
        !clone.join(identity.file_name().unwrap()).exists(),
        "clone must not copy the age identity"
    );
    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            clone_config_path.to_str().unwrap(),
            "fsck",
            "HEAD",
        ])
        .assert()
        .success();

    let hidden_identity = identity.with_extension("hidden");
    fs::rename(&identity, &hidden_identity).unwrap();
    Command::cargo_bin("akeep")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap(), "fsck"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("age identity"));
}

#[test]
fn commit_history_supports_messages_head_ancestors_and_clean_commands() {
    let fixture = Fixture::new();
    let history = fixture.claude.join("projects/demo/session.jsonl");
    fs::write(&history, b"first version").unwrap();

    let first_output = Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "commit",
            "-m",
            "initial agent context",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(first_output.status.success());
    let first: akeep::archive::BackupReport = serde_json::from_slice(&first_output.stdout).unwrap();

    fs::write(&history, b"second version").unwrap();
    let second_output = Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "commit",
            "--message",
            "after implementation",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(second_output.status.success());
    let second: akeep::archive::BackupReport =
        serde_json::from_slice(&second_output.stdout).unwrap();

    let commits = snapshot_list(&fixture);
    assert_eq!(commits[0].snapshot_id, second.snapshot_id);
    assert_eq!(
        commits[0].parent.as_deref(),
        Some(first.snapshot_id.as_str())
    );
    assert_eq!(commits[0].message.as_deref(), Some("after implementation"));
    assert_eq!(commits[1].parent, None);
    assert_eq!(commits[1].message.as_deref(), Some("initial agent context"));

    let checkout = fixture.temp.path().join("head-parent");
    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "checkout",
            "HEAD~1",
            "--to",
            checkout.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert_eq!(
        fs::read(checkout.join("claude-code/projects/demo/session.jsonl")).unwrap(),
        b"first version"
    );

    for old_command in ["add", "doctor", "backup", "snapshots", "verify", "recover"] {
        Command::cargo_bin("akeep")
            .unwrap()
            .arg(old_command)
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }
}

#[test]
fn diff_reports_added_modified_and_removed_history_files() {
    let fixture = Fixture::new();
    let project = fixture.claude.join("projects/demo");
    let changed = project.join("changed.jsonl");
    let removed = project.join("removed.jsonl");
    let added = project.join("added.jsonl");
    fs::write(&changed, b"before").unwrap();
    fs::write(&removed, b"removed").unwrap();
    fixture.backup();

    fs::write(&changed, b"after, with more context").unwrap();
    fs::remove_file(&removed).unwrap();
    fs::write(&added, b"new").unwrap();
    fixture.backup();

    let output = Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "diff",
            "HEAD~1",
            "HEAD",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: akeep::archive::DiffReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report.files_added, 1);
    assert_eq!(report.files_modified, 1);
    assert_eq!(report.files_removed, 1);
    assert_eq!(
        report
            .changes
            .iter()
            .map(|change| (
                change.kind,
                change.logical_path.as_str(),
                change.old_size,
                change.new_size
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                akeep::archive::FileChangeKind::Added,
                "claude-code/projects/demo/added.jsonl",
                None,
                Some(3)
            ),
            (
                akeep::archive::FileChangeKind::Modified,
                "claude-code/projects/demo/changed.jsonl",
                Some(6),
                Some(24)
            ),
            (
                akeep::archive::FileChangeKind::Removed,
                "claude-code/projects/demo/removed.jsonl",
                Some(7),
                None
            ),
        ]
    );

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "diff",
            "--name-only",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "A  claude-code/projects/demo/added.jsonl",
        ))
        .stdout(predicate::str::contains(
            "M  claude-code/projects/demo/changed.jsonl",
        ))
        .stdout(predicate::str::contains(
            "D  claude-code/projects/demo/removed.jsonl",
        ));
}

#[test]
fn clone_creates_a_self_contained_repository_bundle() {
    let fixture = Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"portable agent history",
    )
    .unwrap();
    let commit = fixture.backup();
    let destination = fixture.temp.path().join("clone");

    let output = Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "clone",
            destination.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: akeep::archive::CloneReport = serde_json::from_slice(&output.stdout).unwrap();
    let canonical_destination = fs::canonicalize(&destination).unwrap();
    assert_eq!(report.head, commit.snapshot_id);
    assert!(report.repository_objects >= 3);
    assert!(report.stored_bytes > 0);
    assert_eq!(report.config, canonical_destination.join("config.toml"));
    assert!(!destination.join(".akeep-clone-incomplete").exists());

    let clone_config = akeep::config::Config::load(&report.config).unwrap();
    assert_eq!(
        clone_config.target,
        akeep::config::TargetConfig::Filesystem {
            path: canonical_destination.join("repository")
        }
    );
    assert_eq!(
        clone_config.vault.state_path,
        canonical_destination.join("state")
    );

    Command::cargo_bin("akeep")
        .unwrap()
        .args(["--config", report.config.to_str().unwrap(), "log", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains(&commit.snapshot_id));
    Command::cargo_bin("akeep")
        .unwrap()
        .args(["--config", report.config.to_str().unwrap(), "fsck", "HEAD"])
        .assert()
        .success();
    let checkout = fixture.temp.path().join("clone-checkout");
    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            report.config.to_str().unwrap(),
            "checkout",
            "HEAD",
            "--to",
            checkout.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert_eq!(
        fs::read(checkout.join("claude-code/projects/demo/session.jsonl")).unwrap(),
        b"portable agent history"
    );

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "clone",
            destination.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "clone",
            fixture.vault.join("nested-clone").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("overlaps repository/state path"));
}

#[test]
fn failed_clone_keeps_an_incomplete_marker() {
    let fixture = Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"history",
    )
    .unwrap();
    let commit = fixture.backup();
    fs::write(
        fixture
            .vault
            .join("manifests")
            .join(format!("{}.json", commit.snapshot_id)),
        b"corrupt manifest",
    )
    .unwrap();
    let destination = fixture.temp.path().join("failed-clone");

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            fixture.config.to_str().unwrap(),
            "clone",
            destination.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to parse manifest"));
    assert!(destination.join(".akeep-clone-incomplete").is_file());
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
                "commit",
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
            "log",
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
