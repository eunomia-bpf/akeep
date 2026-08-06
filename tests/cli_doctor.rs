use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn doctor_reports_discovered_providers_as_json() {
    let temp = TempDir::new().unwrap();
    let config_path = temp.path().join("config.toml");
    let vault = temp.path().join("vault");
    let claude = temp.path().join("claude");
    fs::create_dir_all(claude.join("projects/demo")).unwrap();
    fs::write(claude.join("projects/demo/session.jsonl"), b"hello").unwrap();

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "init",
            "--target",
            vault.to_str().unwrap(),
        ])
        .assert()
        .success();

    let mut config = akeep::config::Config::load(&config_path).unwrap();
    config.sources.agentsight_home = Some(temp.path().join("missing-agentsight"));
    config.sources.claude_home = Some(claude);
    config.sources.codex_home = Some(temp.path().join("missing-codex"));
    config.sources.grok_home = Some(temp.path().join("missing-grok"));
    config.sources.kimi_home = Some(temp.path().join("missing-kimi"));
    config.sources.opencode_share = Some(temp.path().join("missing-opencode-share"));
    config.sources.opencode_state = Some(temp.path().join("missing-opencode-state"));
    fs::write(&config_path, toml::to_string_pretty(&config).unwrap()).unwrap();
    let expected_workers = akeep::resources::archive_workers(&config);

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "status",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"healthy\": true"))
        .stdout(predicate::str::contains(format!(
            "\"archive_workers\": {expected_workers}"
        )))
        .stdout(predicate::str::contains("\"staging_available_bytes\""))
        .stdout(predicate::str::contains("\"provider\": \"claude-code\""))
        .stdout(predicate::str::contains("\"file_count\": 1"));
}

#[test]
fn doctor_fails_when_target_overlaps_a_source() {
    let temp = TempDir::new().unwrap();
    let config_path = temp.path().join("config.toml");
    let claude = temp.path().join("claude");
    let vault = claude.join("vault");

    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "init",
            "--target",
            vault.to_str().unwrap(),
        ])
        .assert()
        .success();

    let mut config = akeep::config::Config::load(&config_path).unwrap();
    config.sources.agentsight_home = Some(temp.path().join("missing-agentsight"));
    config.sources.claude_home = Some(claude);
    config.sources.codex_home = Some(temp.path().join("missing-codex"));
    config.sources.grok_home = Some(temp.path().join("missing-grok"));
    config.sources.kimi_home = Some(temp.path().join("missing-kimi"));
    config.sources.opencode_share = Some(temp.path().join("missing-opencode-share"));
    config.sources.opencode_state = Some(temp.path().join("missing-opencode-state"));
    fs::write(&config_path, toml::to_string_pretty(&config).unwrap()).unwrap();

    Command::cargo_bin("akeep")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap(), "status"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("overlaps provider root"))
        .stderr(predicate::str::contains("blocking problems"));
}
