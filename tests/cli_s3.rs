use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const S3_COMPATIBLE_ENDPOINT: &str = "https://example.r2.cloudflarestorage.com";

#[test]
fn s3_backup_deduplicates_verifies_lists_and_recovers() {
    let fixture = S3Fixture::new();
    fs::write(
        fixture.claude.join("projects/demo/session.jsonl"),
        b"remote backup payload",
    )
    .unwrap();

    fs::write(&fixture.aws_log, b"").unwrap();
    let first = fixture.backup(None);
    assert!(first.new_objects > 0);
    let aws_log = fs::read_to_string(&fixture.aws_log).unwrap();
    let object_list_calls = aws_log
        .lines()
        .filter(|line| *line == "s3api list-objects-v2")
        .count();
    assert!(
        object_list_calls <= 12,
        "object metadata was queried per chunk:\n{aws_log}"
    );
    let object_upload_calls = aws_log.lines().filter(|line| *line == "s3 cp").count();
    assert!(
        object_upload_calls <= 4,
        "objects were uploaded with one AWS CLI process per chunk:\n{aws_log}"
    );
    assert_eq!(
        aws_log
            .lines()
            .filter(|line| *line == "s3 cp recursive")
            .count(),
        1,
        "new objects were not uploaded as one staged batch:\n{aws_log}"
    );
    let second = fixture.backup(None);
    assert_eq!(second.new_objects, 0);
    assert_eq!(second.new_stored_bytes, 0);

    fixture
        .command()
        .args(["log", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains(&first.snapshot_id));
    fixture
        .command()
        .args(["fsck", "latest"])
        .assert()
        .success();

    let recovery = fixture.temp.path().join("recovery");
    fixture
        .command()
        .args(["checkout", "latest", "--to"])
        .arg(&recovery)
        .assert()
        .success();
    assert_eq!(
        fs::read(recovery.join("claude-code/projects/demo/session.jsonl")).unwrap(),
        b"remote backup payload"
    );

    let clone = fixture.temp.path().join("s3-clone");
    fixture
        .command()
        .args(["clone"])
        .arg(&clone)
        .assert()
        .success();
    let clone_config = clone.join("config.toml");
    Command::cargo_bin("akeep")
        .unwrap()
        .args(["--config", clone_config.to_str().unwrap(), "fsck", "HEAD"])
        .assert()
        .success();
    assert!(clone.join("repository/refs/latest").is_file());
    assert!(!clone.join(".akeep-clone-incomplete").exists());

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
        .args(["commit"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("injected upload failure"));
    fixture
        .command()
        .args(["log"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No commits."));
    fixture
        .command()
        .args(["fsck", "latest"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("repository has no commits"));

    let recovered = fixture.backup(None);
    assert!(recovered.new_objects > 0);
    fixture
        .command()
        .args(["fsck", "latest"])
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
    aws_log: PathBuf,
    claude: PathBuf,
}

impl S3Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let cloud = temp.path().join("cloud");
        let state_home = temp.path().join("state");
        let aws = temp.path().join("fake-aws");
        let aws_log = temp.path().join("aws.log");
        let claude = temp.path().join("claude");
        fs::create_dir_all(claude.join("projects/demo")).unwrap();
        fs::create_dir_all(&cloud).unwrap();
        fs::write(&aws_log, b"").unwrap();
        write_fake_aws(&aws);

        Command::cargo_bin("akeep")
            .unwrap()
            .env("FAKE_S3_ROOT", &cloud)
            .env("FAKE_S3_LOG", &aws_log)
            .env("FAKE_S3_EXPECT_ENDPOINT", S3_COMPATIBLE_ENDPOINT)
            .env("XDG_STATE_HOME", &state_home)
            .args([
                "--config",
                config.to_str().unwrap(),
                "init",
                "--s3-bucket",
                "test-bucket",
                "--s3-prefix",
                "akeep",
                "--s3-endpoint-url",
                S3_COMPATIBLE_ENDPOINT,
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
            aws_log,
            claude,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("akeep").unwrap();
        command
            .env("FAKE_S3_ROOT", &self.cloud)
            .env("FAKE_S3_LOG", &self.aws_log)
            .env("FAKE_S3_EXPECT_ENDPOINT", S3_COMPATIBLE_ENDPOINT)
            .env("XDG_STATE_HOME", &self.state_home)
            .args(["--config", self.config.to_str().unwrap()]);
        command
    }

    fn backup(&self, failure: Option<&str>) -> akeep::archive::BackupReport {
        let mut command = self.command();
        if let Some(failure) = failure {
            command.env("FAKE_S3_FAIL_UPLOAD_CONTAINS", failure);
        }
        let output = command.args(["commit", "--json"]).output().unwrap();
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
endpoint=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --profile|--region) shift 2 ;;
        --endpoint-url) endpoint=$2; shift 2 ;;
        *) break ;;
    esac
done
if [ "$endpoint" != "${FAKE_S3_EXPECT_ENDPOINT:-}" ]; then
    echo "unexpected S3-compatible endpoint: $endpoint" >&2
    exit 2
fi

service=${1:?}
operation=${2:?}
shift 2
if [ -n "${FAKE_S3_LOG:-}" ]; then
    mode=
    for argument in "$@"; do
        if [ "$argument" = "--recursive" ]; then mode=" recursive"; fi
    done
    printf '%s %s%s\n' "$service" "$operation" "$mode" >> "$FAKE_S3_LOG"
fi

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
            if [ -d "$source" ]; then
                relative=${relative%/}
                find "$source" -type f -print | LC_ALL=C sort | while IFS= read -r file; do
                    suffix=${file#"$source/"}
                    object="$relative/$suffix"
                    case "$object" in
                        *"${FAKE_S3_FAIL_UPLOAD_CONTAINS:-__never__}"*)
                            echo "injected upload failure for $object" >&2
                            exit 42
                            ;;
                    esac
                    mkdir -p "$(dirname "$root/$object")"
                    cp "$file" "$root/$object"
                done
                exit 0
            fi
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
