use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn s3_backup_deduplicates_verifies_lists_and_recovers() {
    let fixture = S3Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"remote backup payload",
    )
    .unwrap();

    let first = fixture.backup(None);
    assert!(first.new_objects > 0);
    let second = fixture.backup(None);
    assert_eq!(second.new_objects, 0);
    assert_eq!(second.new_stored_bytes, 0);

    fixture
        .command()
        .args(["snapshots", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains(&first.snapshot_id));
    fixture
        .command()
        .args(["verify", "latest"])
        .assert()
        .success();

    let recovery = fixture.temp.path().join("recovery");
    fixture
        .command()
        .args(["recover", "latest", "--to"])
        .arg(&recovery)
        .assert()
        .success();
    assert_eq!(
        fs::read(recovery.join("claude-code/projects/demo/session.jsonl")).unwrap(),
        b"remote backup payload"
    );

    assert!(fixture.cloud.join("test-bucket/akeep/vault.json").is_file());
    assert!(
        fixture
            .cloud
            .join(format!(
                "test-bucket/akeep/manifests/{}.json",
                first.snapshot_id
            ))
            .is_file()
    );
    let active = akeep::config::Config::load(&fixture.config).unwrap();
    assert!(
        !active.vault.state_path.join("objects").exists(),
        "remote objects must not be copied into local state"
    );
}

#[test]
fn failed_s3_upload_never_publishes_a_recovery_point_and_retry_succeeds() {
    let fixture = S3Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"interrupted upload payload",
    )
    .unwrap();

    fixture
        .command()
        .env("FAKE_S3_FAIL_UPLOAD_CONTAINS", "objects/")
        .args(["backup"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("injected upload failure"));
    fixture
        .command()
        .args(["snapshots"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No recovery points."));
    fixture
        .command()
        .args(["verify", "latest"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no latest recovery point"));

    let recovered = fixture.backup(None);
    assert!(recovered.new_objects > 0);
    fixture
        .command()
        .args(["verify", "latest"])
        .assert()
        .success();
}

#[test]
fn failed_s3_vault_creation_rolls_back_local_initialization() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("config.toml");
    let cloud = temp.path().join("cloud");
    let state_home = temp.path().join("state");
    let aws = temp.path().join("fake-aws");
    fs::create_dir_all(&cloud).unwrap();
    write_fake_aws(&aws);

    Command::cargo_bin("akeep")
        .unwrap()
        .env("FAKE_S3_ROOT", &cloud)
        .env("FAKE_S3_FAIL_UPLOAD_CONTAINS", "vault.json")
        .env("XDG_STATE_HOME", &state_home)
        .args([
            "--config",
            config.to_str().unwrap(),
            "init",
            "--s3-bucket",
            "test-bucket",
            "--aws-cli",
            aws.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("rolled back"));

    assert!(!config.exists());
    let vault_states = state_home.join("akeep/vaults");
    assert!(
        !vault_states.exists() || fs::read_dir(vault_states).unwrap().next().is_none(),
        "failed initialization left vault state behind"
    );
}

struct S3Fixture {
    temp: TempDir,
    config: PathBuf,
    cloud: PathBuf,
    state_home: PathBuf,
    claude: PathBuf,
}

impl S3Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let cloud = temp.path().join("cloud");
        let state_home = temp.path().join("state");
        let aws = temp.path().join("fake-aws");
        let claude = temp.path().join("claude");
        fs::create_dir_all(claude.join("projects/demo")).unwrap();
        fs::create_dir_all(&cloud).unwrap();
        write_fake_aws(&aws);

        Command::cargo_bin("akeep")
            .unwrap()
            .env("FAKE_S3_ROOT", &cloud)
            .env("XDG_STATE_HOME", &state_home)
            .args([
                "--config",
                config.to_str().unwrap(),
                "init",
                "--s3-bucket",
                "test-bucket",
                "--s3-prefix",
                "akeep",
                "--aws-cli",
                aws.to_str().unwrap(),
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("Target: s3://test-bucket/akeep/"));

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
            cloud,
            state_home,
            claude,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("akeep").unwrap();
        command
            .env("FAKE_S3_ROOT", &self.cloud)
            .env("XDG_STATE_HOME", &self.state_home)
            .args(["--config", self.config.to_str().unwrap()]);
        command
    }

    fn backup(&self, failure: Option<&str>) -> akeep::archive::BackupReport {
        let mut command = self.command();
        if let Some(failure) = failure {
            command.env("FAKE_S3_FAIL_UPLOAD_CONTAINS", failure);
        }
        let output = command.args(["backup", "--json"]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn write_fake_aws(path: &Path) {
    fs::write(
        path,
        r#"#!/bin/sh
set -eu

root=${FAKE_S3_ROOT:?}
while [ "$#" -gt 0 ]; do
    case "$1" in
        --profile|--region|--endpoint-url) shift 2 ;;
        *) break ;;
    esac
done

service=${1:?}
operation=${2:?}
shift 2

if [ "$service" = "s3" ] && [ "$operation" = "cp" ]; then
    source=${1:?}
    destination=${2:?}
    case "$source" in
        s3://*)
            relative=${source#s3://}
            cat "$root/$relative"
            ;;
        *)
            relative=${destination#s3://}
            case "$relative" in
                *"${FAKE_S3_FAIL_UPLOAD_CONTAINS:-__never__}"*)
                    echo "injected upload failure for $relative" >&2
                    exit 42
                    ;;
            esac
            mkdir -p "$(dirname "$root/$relative")"
            cp "$source" "$root/$relative"
            ;;
    esac
    exit 0
fi

if [ "$service" = "s3api" ] && [ "$operation" = "list-objects-v2" ]; then
    bucket=
    prefix=
    max_keys=1000000
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --bucket) bucket=$2; shift 2 ;;
            --prefix) prefix=$2; shift 2 ;;
            --max-keys) max_keys=$2; shift 2 ;;
            --output) shift 2 ;;
            *) shift ;;
        esac
    done
    bucket_root="$root/$bucket"
    printf '{"Contents":['
    count=0
    if [ -d "$bucket_root" ]; then
        find "$bucket_root" -type f -print | LC_ALL=C sort | while IFS= read -r file; do
            key=${file#"$bucket_root/"}
            case "$key" in
                "$prefix"*)
                    if [ "$count" -lt "$max_keys" ]; then
                        if [ "$count" -gt 0 ]; then printf ','; fi
                        size=$(wc -c < "$file" | tr -d ' ')
                        printf '{"Key":"%s","Size":%s}' "$key" "$size"
                        count=$((count + 1))
                    fi
                    ;;
            esac
        done
    fi
    printf ']}\n'
    exit 0
fi

if [ "$service" = "s3api" ] && [ "$operation" = "get-bucket-versioning" ]; then
    printf '{"Status":"Enabled"}\n'
    exit 0
fi

echo "unsupported fake AWS command: $service $operation" >&2
exit 64
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}
