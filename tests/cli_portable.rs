use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn handoff_works_from_a_verified_recovery_point() {
    let fixture = Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"{\"role\":\"user\",\"text\":\"akeep-needle private context\"}\n",
    )
    .unwrap();
    let first = fixture.backup();

    let repository = fixture.temp.path().join("repository");
    initialize_repository(&repository);
    let artifact = repository.join("notes.txt");
    fs::write(&artifact, b"changed artifact\n").unwrap();
    let handoff = fixture.temp.path().join("handoff.md");
    fixture
        .command()
        .args([
            "handoff",
            &first.snapshot_id,
            "--from",
            "claude-code",
            "--for",
            "codex",
            "--goal",
            "Finish the recovery drill",
            "--decision",
            "Keep encryption optional",
            "--open-task",
            "Run the final test suite",
            "--test-status",
            "Unit tests pass",
            "--repo",
            repository.to_str().unwrap(),
            "--artifact",
            artifact.to_str().unwrap(),
            "--to",
            handoff.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-code → codex"));
    let handoff = fs::read_to_string(handoff).unwrap();
    for expected in [
        "Finish the recovery drill",
        "Keep encryption optional",
        "Run the final test suite",
        "Unit tests pass",
        "notes.txt",
        "akeep-needle private context",
        "git status --short",
    ] {
        assert!(handoff.contains(expected), "missing {expected:?}");
    }
}

struct Fixture {
    temp: TempDir,
    config: PathBuf,
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
        active.archive.chunk_size = 8;
        active.sources.agentsight_home = Some(temp.path().join("missing-agentsight"));
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
            claude,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("akeep").unwrap();
        command.args(["--config", self.config.to_str().unwrap()]);
        command
    }

    fn backup(&self) -> akeep::archive::BackupReport {
        let output = self.command().args(["commit", "--json"]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn initialize_repository(path: &Path) {
    fs::create_dir_all(path).unwrap();
    run_git(path, &["init", "-q"]);
    run_git(path, &["config", "user.email", "akeep@example.invalid"]);
    run_git(path, &["config", "user.name", "Akeep Test"]);
    fs::write(path.join("notes.txt"), b"initial artifact\n").unwrap();
    run_git(path, &["add", "notes.txt"]);
    run_git(path, &["commit", "-qm", "initial"]);
}

fn run_git(path: &Path, arguments: &[&str]) {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(path)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}
