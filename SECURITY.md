# Security policy

## Reporting

Please use GitHub's private **Report a vulnerability** flow for security or
privacy issues. Do not open a public issue containing session data, credentials,
bucket/account identifiers, an age identity, or an unredacted configuration.

A useful private report includes the affected Akeep version, target type,
encryption mode, a synthetic reproducer, and whether confidentiality, integrity,
or recoverability was affected.

## Security boundaries

- Akeep is local-first and has no telemetry.
- Network access occurs only for an explicitly configured S3 target.
- Client-side age encryption is optional. In plaintext mode, filesystem and
  storage administrators can read the archive.
- The local configuration, generated age identity, search index, exports, and
  handoff files are sensitive. Unix permissions reduce accidental exposure but
  do not protect a compromised user account.
- Akeep is not a defense against a malicious root user, compromised provider,
  hostile AWS CLI executable, or an attacker who can rewrite both an
  unencrypted archive and the expected hashes.
- Losing every copy of an age identity is permanent data loss. Akeep has no
  escrow or recovery service.

The latest `0.1.0-alpha.*` release and `main` receive security fixes. Alpha
archives remain readable across patch releases unless release notes explicitly
document a migration.
