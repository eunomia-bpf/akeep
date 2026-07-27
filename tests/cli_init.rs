use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn init_creates_a_private_valid_configuration() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("config").join("config.toml");
    let vault = temp.path().join("vault");

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
        .success()
        .stdout(predicate::str::contains("Initialized Akeep repository"))
        .stdout(predicate::str::contains("Encryption: none"));

    assert!(vault.is_dir());
    let parsed = akeep::config::Config::load(&config).unwrap();
    assert_eq!(
        parsed.target,
        akeep::config::TargetConfig::Filesystem {
            path: vault.canonicalize().unwrap()
        }
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&vault).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
fn init_refuses_to_overwrite_an_existing_configuration() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("config.toml");
    let vault = temp.path().join("vault");

    let run = || {
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
    };

    run().success();
    run()
        .failure()
        .stderr(predicate::str::contains("refusing to overwrite"));
}

#[test]
fn config_show_loads_and_prints_the_configuration() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("config.toml");
    let vault = temp.path().join("vault");

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

    Command::cargo_bin("akeep")
        .unwrap()
        .args(["--config", config.to_str().unwrap(), "config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("format_version = 1"))
        .stdout(predicate::str::contains("mode = \"none\""));
}

#[test]
fn failed_remote_init_does_not_leave_a_configuration() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("config.toml");
    let missing_aws = temp.path().join("missing-aws");

    Command::cargo_bin("akeep")
        .unwrap()
        .env("XDG_STATE_HOME", temp.path().join("state"))
        .args([
            "--config",
            config.to_str().unwrap(),
            "init",
            "--s3-bucket",
            "test-bucket",
            "--aws-cli",
            missing_aws.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("executable"));

    assert!(!config.exists());
}
