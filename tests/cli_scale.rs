use std::fs;
use std::path::Path;
use std::time::Instant;

use assert_cmd::Command;
use tempfile::TempDir;

const MIB: usize = 1024 * 1024;
const SCALE_BYTES: usize = 32 * MIB;

#[test]
fn archives_and_recovers_a_multichunk_scale_fixture_incrementally() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("config.toml");
    let vault = temp.path().join("vault");
    let claude = temp.path().join("claude");
    fs::create_dir_all(claude.join("projects/scale")).unwrap();
    let source = claude.join("projects/scale/session.jsonl");
    let payload = deterministic_payload(SCALE_BYTES);
    let expected_hash = blake3::hash(&payload);
    fs::write(&source, &payload).unwrap();

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
    active.archive.chunk_size = MIB as u64;
    active.sources.claude_home = Some(claude);
    active.sources.codex_home = Some(temp.path().join("missing-codex"));
    active.sources.grok_home = Some(temp.path().join("missing-grok"));
    active.sources.kimi_home = Some(temp.path().join("missing-kimi"));
    active.sources.opencode_share = Some(temp.path().join("missing-opencode-share"));
    active.sources.opencode_state = Some(temp.path().join("missing-opencode-state"));
    fs::write(&config, toml::to_string_pretty(&active).unwrap()).unwrap();

    let first_started = Instant::now();
    let first = backup(&config);
    let first_elapsed = first_started.elapsed();
    assert_eq!(first.logical_bytes, SCALE_BYTES as u64);
    assert_eq!(first.chunk_references, 32);
    assert_eq!(first.unique_objects, 32);
    assert_eq!(first.new_objects, 32);

    let second_started = Instant::now();
    let second = backup(&config);
    let second_elapsed = second_started.elapsed();
    assert_eq!(second.new_objects, 0);
    assert_eq!(second.new_stored_bytes, 0);

    let recovery = temp.path().join("recovery");
    Command::cargo_bin("akeep")
        .unwrap()
        .args([
            "--config",
            config.to_str().unwrap(),
            "recover",
            "latest",
            "--to",
            recovery.to_str().unwrap(),
        ])
        .assert()
        .success();
    let recovered = fs::read(recovery.join("claude-code/projects/scale/session.jsonl")).unwrap();
    assert_eq!(recovered.len(), SCALE_BYTES);
    assert_eq!(blake3::hash(&recovered), expected_hash);

    eprintln!(
        "32 MiB scale smoke: first={first_elapsed:?}, deduplicated={second_elapsed:?}, stored={} bytes",
        first.new_stored_bytes
    );
}

fn backup(config: &Path) -> akeep::archive::BackupReport {
    let output = Command::cargo_bin("akeep")
        .unwrap()
        .args(["--config", config.to_str().unwrap(), "backup", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn deterministic_payload(size: usize) -> Vec<u8> {
    let mut state = 0x9e37_79b9_u32;
    let mut payload = Vec::with_capacity(size);
    for _ in 0..size {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        payload.push((state & 0xff) as u8);
    }
    payload
}
