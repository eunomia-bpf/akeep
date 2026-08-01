use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn git_backend_commits_recovers_and_rehydrates_its_cache() {
    let fixture = GitFixture::new();
    fixture.init();
    fixture.command().arg("status").assert().success();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"git-backed agent history",
    )
    .unwrap();

    fixture
        .command()
        .args(["commit", "-m", "first remote snapshot"])
        .assert()
        .success();
    assert_eq!(fixture.remote_commit_count(), 2);
    assert!(fixture.remote_has("repository/refs/latest"));

    let config = akeep::config::Config::load(&fixture.config).unwrap();
    fs::remove_dir_all(&config.vault.state_path).unwrap();
    fixture.command().args(["fsck", "HEAD"]).assert().success();

    let recovered = fixture.temp.path().join("recovered");
    fixture
        .command()
        .args(["checkout", "HEAD", "--to", recovered.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(
        fs::read(recovered.join("claude-code/projects/demo/session.jsonl")).unwrap(),
        b"git-backed agent history"
    );

    let bundle = fixture.temp.path().join("bundle");
    fixture
        .command()
        .args(["clone", bundle.to_str().unwrap()])
        .assert()
        .success();
    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            bundle.join("config.toml").to_str().unwrap(),
            "fsck",
            "HEAD",
        ])
        .assert()
        .success();
}

#[test]
fn a_second_client_adopts_an_existing_git_vault() {
    let first = GitFixture::new();
    first.init();
    fs::write(
        first.claude.join("projects/demo/session.jsonl"),
        b"shared history",
    )
    .unwrap();
    first.command().arg("commit").assert().success();
    let first_config = akeep::config::Config::load(&first.config).unwrap();

    let second_root = first.temp.path().join("second-client");
    let second_config = second_root.join("config.toml");
    let mut init = isolated_command(&second_root, &second_config);
    init.args([
        "init",
        "--git-repository",
        first.remote.to_str().unwrap(),
        "--git-branch",
        "akeep",
    ])
    .assert()
    .success();

    let adopted = akeep::config::Config::load(&second_config).unwrap();
    assert_eq!(adopted.vault.id, first_config.vault.id);
    let output = isolated_command(&second_root, &second_config)
        .args(["log", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let snapshots: Vec<akeep::archive::SnapshotInfo> =
        serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(snapshots.len(), 1);
    isolated_command(&second_root, &second_config)
        .args(["fsck", "HEAD"])
        .assert()
        .success();
}

#[test]
fn rejected_git_push_does_not_publish_a_partial_snapshot() {
    let fixture = GitFixture::new();
    fixture.init();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"do not publish partially",
    )
    .unwrap();
    let hook = fixture.remote.join("hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    run_git(
        &fixture.remote,
        &[
            "config",
            "core.hooksPath",
            fixture.remote.join("hooks").to_str().unwrap(),
        ],
    );

    fixture
        .command()
        .arg("commit")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Git failed to push repository"));
    assert_eq!(fixture.remote_commit_count(), 1);
    assert!(!fixture.remote_has("repository/refs/latest"));

    fs::remove_file(hook).unwrap();
    fixture.command().arg("commit").assert().success();
    assert_eq!(fixture.remote_commit_count(), 2);
    assert!(fixture.remote_has("repository/refs/latest"));
}

#[test]
fn encrypted_git_vault_requires_the_same_recovery_identity() {
    let fixture = GitFixture::new();
    fixture
        .command()
        .args([
            "init",
            "--git-repository",
            fixture.remote.to_str().unwrap(),
            "--encryption",
            "age",
        ])
        .assert()
        .success();
    let first = akeep::config::Config::load(&fixture.config).unwrap();
    let identity = first.encryption.identity_file.unwrap();

    let second_root = fixture.temp.path().join("encrypted-second");
    let second_config = second_root.join("config.toml");
    isolated_command(&second_root, &second_config)
        .args([
            "init",
            "--git-repository",
            fixture.remote.to_str().unwrap(),
            "--encryption",
            "age",
            "--age-identity-file",
            identity.to_str().unwrap(),
        ])
        .assert()
        .success();

    let wrong_root = fixture.temp.path().join("encrypted-wrong-key");
    let wrong_config = wrong_root.join("config.toml");
    isolated_command(&wrong_root, &wrong_config)
        .args([
            "init",
            "--git-repository",
            fixture.remote.to_str().unwrap(),
            "--encryption",
            "age",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not unlock"));
    assert!(!wrong_config.exists());
}

struct GitFixture {
    temp: TempDir,
    config: PathBuf,
    remote: PathBuf,
    claude: PathBuf,
}

impl GitFixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let remote = temp.path().join("remote.git");
        run_git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
        let claude = temp.path().join("client/claude");
        fs::create_dir_all(claude.join("projects/demo")).unwrap();
        Self {
            config: temp.path().join("client/config.toml"),
            temp,
            remote,
            claude,
        }
    }

    fn init(&self) {
        self.command()
            .args([
                "init",
                "--git-repository",
                self.remote.to_str().unwrap(),
                "--git-branch",
                "akeep",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("Target: git:"));
    }

    fn command(&self) -> Command {
        let mut command = isolated_command(self.temp.path().join("client"), &self.config);
        command.env("CLAUDE_CONFIG_DIR", &self.claude);
        command
    }

    fn remote_commit_count(&self) -> usize {
        let output = run_git(&self.remote, &["rev-list", "--count", "refs/heads/akeep"]);
        String::from_utf8(output).unwrap().trim().parse().unwrap()
    }

    fn remote_has(&self, path: &str) -> bool {
        ProcessCommand::new("git")
            .arg("--git-dir")
            .arg(&self.remote)
            .args(["cat-file", "-e", &format!("refs/heads/akeep:{path}")])
            .output()
            .unwrap()
            .status
            .success()
    }
}

fn isolated_command(root: impl AsRef<Path>, config: &Path) -> Command {
    let root = root.as_ref();
    let mut command = Command::cargo_bin("akeep").unwrap();
    command
        .env("XDG_STATE_HOME", root.join("state-home"))
        .env("CODEX_HOME", root.join("missing-codex"))
        .env("GROK_HOME", root.join("missing-grok"))
        .env("KIMI_CODE_HOME", root.join("missing-kimi"))
        .env("OPENCODE_SHARE_DIR", root.join("missing-opencode-share"))
        .env("OPENCODE_STATE_DIR", root.join("missing-opencode-state"))
        .args(["--config", config.to_str().unwrap()]);
    command
}

fn run_git(directory: &Path, args: &[&str]) -> Vec<u8> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
