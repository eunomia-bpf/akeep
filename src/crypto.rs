use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use age::secrecy::ExposeSecret;
use anyhow::{Context, Result, bail};

use crate::config::{
    EncryptionConfig, EncryptionMode, create_private_directory, write_private_new_file,
};

const SELF_TEST_PLAINTEXT: &[u8] = b"akeep age self-test v1";

pub struct CryptoContext {
    mode: EncryptionMode,
    recipient: Option<age::x25519::Recipient>,
    identity: Option<age::x25519::Identity>,
}

pub struct PreparedEncryption {
    pub config: EncryptionConfig,
    pub generated_identity_file: Option<PathBuf>,
}

impl CryptoContext {
    pub fn from_config(config: &EncryptionConfig) -> Result<Self> {
        match config.mode {
            EncryptionMode::None => Ok(Self {
                mode: EncryptionMode::None,
                recipient: None,
                identity: None,
            }),
            EncryptionMode::Age => {
                let recipient_text = config
                    .recipient
                    .as_deref()
                    .context("age recipient is missing")?;
                let recipient = age::x25519::Recipient::from_str(recipient_text)
                    .map_err(|error| anyhow::anyhow!("invalid age recipient: {error}"))?;
                let identity_path = config
                    .identity_file
                    .as_deref()
                    .context("age identity file is missing")?;
                let identity = load_identity(identity_path)?;
                if identity.to_public().to_string() != recipient.to_string() {
                    bail!(
                        "age identity {} does not match configured recipient",
                        identity_path.display()
                    );
                }

                let context = Self {
                    mode: EncryptionMode::Age,
                    recipient: Some(recipient),
                    identity: Some(identity),
                };
                let encrypted = context.encrypt(SELF_TEST_PLAINTEXT)?;
                let decrypted = context.decrypt(&encrypted)?;
                if decrypted != SELF_TEST_PLAINTEXT {
                    bail!("age encryption self-test failed");
                }
                Ok(context)
            }
        }
    }

    pub fn mode(&self) -> EncryptionMode {
        self.mode
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        match self.mode {
            EncryptionMode::None => Ok(plaintext.to_vec()),
            EncryptionMode::Age => age::encrypt(
                self.recipient
                    .as_ref()
                    .context("age recipient is unavailable")?,
                plaintext,
            )
            .map_err(|error| anyhow::anyhow!("age encryption failed: {error}")),
        }
    }

    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        match self.mode {
            EncryptionMode::None => Ok(ciphertext.to_vec()),
            EncryptionMode::Age => age::decrypt(
                self.identity
                    .as_ref()
                    .context("age identity is unavailable")?,
                ciphertext,
            )
            .map_err(|error| anyhow::anyhow!("age decryption failed: {error}")),
        }
    }
}

pub fn prepare_encryption(
    mode: EncryptionMode,
    config_path: &Path,
    existing_identity_file: Option<&Path>,
) -> Result<PreparedEncryption> {
    match mode {
        EncryptionMode::None => {
            if existing_identity_file.is_some() {
                bail!("--age-identity-file requires --encryption age");
            }
            Ok(PreparedEncryption {
                config: EncryptionConfig {
                    mode,
                    recipient: None,
                    identity_file: None,
                },
                generated_identity_file: None,
            })
        }
        EncryptionMode::Age => {
            if let Some(path) = existing_identity_file {
                let identity_path = fs::canonicalize(path).with_context(|| {
                    format!("failed to resolve age identity {}", path.display())
                })?;
                let identity = load_identity(&identity_path)?;
                let config = EncryptionConfig {
                    mode,
                    recipient: Some(identity.to_public().to_string()),
                    identity_file: Some(identity_path),
                };
                CryptoContext::from_config(&config)?;
                return Ok(PreparedEncryption {
                    config,
                    generated_identity_file: None,
                });
            }

            let identity = age::x25519::Identity::generate();
            let recipient = identity.to_public().to_string();
            let parent = config_path.parent().with_context(|| {
                format!("configuration path {} has no parent", config_path.display())
            })?;
            create_private_directory(parent)?;
            let suffix = &uuid::Uuid::new_v4().simple().to_string()[..8];
            let identity_path = parent.join(format!("age-identity-{suffix}.txt"));
            let contents = format!(
                "# Akeep age recovery identity\n# Recipient: {recipient}\n{}\n",
                identity.to_string().expose_secret()
            );
            write_private_new_file(&identity_path, contents.as_bytes())?;
            let identity_path = fs::canonicalize(&identity_path)?;
            let config = EncryptionConfig {
                mode,
                recipient: Some(recipient),
                identity_file: Some(identity_path.clone()),
            };
            CryptoContext::from_config(&config)?;
            Ok(PreparedEncryption {
                config,
                generated_identity_file: Some(identity_path),
            })
        }
    }
}

fn load_identity(path: &Path) -> Result<age::x25519::Identity> {
    ensure_private_key_permissions(path)?;
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read age identity {}", path.display()))?;
    let secret = contents
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .with_context(|| format!("no age identity found in {}", path.display()))?;
    age::x25519::Identity::from_str(secret)
        .map_err(|error| anyhow::anyhow!("invalid age identity in {}: {error}", path.display()))
}

#[cfg(unix)]
fn ensure_private_key_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)
        .with_context(|| format!("failed to inspect age identity {}", path.display()))?
        .permissions()
        .mode()
        & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "age identity {} has unsafe permissions {:03o}; expected 0600",
            path.display(),
            mode
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_key_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn generates_and_round_trips_an_age_identity() {
        let temp = TempDir::new().unwrap();
        let prepared =
            prepare_encryption(EncryptionMode::Age, &temp.path().join("config.toml"), None)
                .unwrap();
        let context = CryptoContext::from_config(&prepared.config).unwrap();
        let encrypted = context.encrypt(b"private history").unwrap();
        assert_ne!(encrypted, b"private history");
        assert_eq!(context.decrypt(&encrypted).unwrap(), b"private history");
        assert!(prepared.generated_identity_file.unwrap().exists());
    }

    #[test]
    fn none_mode_needs_no_key() {
        let temp = TempDir::new().unwrap();
        let prepared =
            prepare_encryption(EncryptionMode::None, &temp.path().join("config.toml"), None)
                .unwrap();
        let context = CryptoContext::from_config(&prepared.config).unwrap();
        assert_eq!(context.encrypt(b"plain").unwrap(), b"plain");
        assert_eq!(context.decrypt(b"plain").unwrap(), b"plain");
    }
}
