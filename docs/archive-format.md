# Akeep archive format v1

The archive is an application-level content-addressed store. Filesystem and S3
targets expose the same logical keys:

```text
vault.json
objects/<first two BLAKE3 hex characters>/<remaining hex>.zst[.age]
manifests/<snapshot-id>.json[.age]
refs/latest
```

`vault.json` records the format version, random vault UUID, and immutable
encryption mode. It contains no provider paths or credentials.

## Objects

Each regular input or consistent SQLite snapshot is read in fixed-size chunks
(4 MiB by default). The object ID is the lowercase 64-character BLAKE3 digest
of the uncompressed bytes. A chunk is compressed independently with zstd. In
age mode, that compressed frame is then authenticated-encrypted to the vault's
X25519 recipient.

The same raw chunk maps to the same logical object key across files, providers,
and recovery points. Existing objects are never overwritten. Akeep compares
their stored size during incremental backup; `verify` recomputes content hashes
after decoding.

Fixed-size chunking was chosen for v1 because coding-agent state is dominated by
append-only JSONL and SQLite snapshots, it supports bounded memory, and it keeps
the recovery implementation small enough to audit. A future format may add a
content-defined algorithm under a new manifest descriptor.

## Manifests

A manifest is published only after every referenced object exists with its
expected stored size. It records:

- vault and snapshot identifiers;
- UTC creation time and hostname;
- chunk, compression, and encryption parameters;
- provider summaries;
- stable provider-relative logical paths;
- raw and stored sizes;
- file and chunk BLAKE3 hashes;
- Unix mode and modification time when available.

In age mode, the entire manifest payload is encrypted. Object identifiers and
snapshot identifiers remain visible in key names; provider paths, hostnames, and
file metadata do not. `refs/latest` contains only the latest snapshot ID.

Snapshot IDs combine a sortable UTC timestamp, millisecond precision, and a
random suffix. Publishing a manifest last means an interrupted backup can leave
unreferenced immutable objects but cannot create a false-complete recovery
point. Retrying safely reuses those objects.

## Durability and concurrency

Filesystem objects and references are written to a temporary file, synced, and
atomically renamed. S3 puts are complete-object operations and uploaded sizes
are re-read before publication. Bucket versioning protects the mutable
`refs/latest` object but is not required to address immutable manifests by ID.

The local state directory contains the per-vault advisory backup lock and
private staging directories. v0.1 is single-writer across one machine; operators
must not schedule the same S3 vault from multiple machines concurrently.

## Recovery safety

All object and logical path components are validated before use. Absolute paths,
empty components, `.` and `..` are rejected. Symlinks are not followed during
discovery, and recovery refuses a symlink, non-directory, non-empty directory,
or any target overlapping the vault or its state directory. Every recovered
chunk and file is hashed before the incomplete marker is removed.
