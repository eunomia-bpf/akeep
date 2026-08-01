use std::path::Path;

use anyhow::{Context, Result};
use sysinfo::System;

use crate::config::{Config, DEFAULT_ARCHIVE_WORKERS};

pub const ARCHIVE_CHUNKS_PER_FILE_BATCH: usize = 4;
pub const S3_UPLOAD_BATCH_BYTES: u64 = 512 * 1024 * 1024;
pub const RESOURCE_RESERVE_BYTES: u64 = 256 * 1024 * 1024;

pub fn cpu_threads() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
}

pub fn archive_workers(config: &Config) -> usize {
    config
        .archive
        .workers
        .unwrap_or(DEFAULT_ARCHIVE_WORKERS)
        .min(cpu_threads())
        .max(1)
}

pub fn estimated_peak_memory_bytes(config: &Config) -> u64 {
    let buffers_per_worker = (ARCHIVE_CHUNKS_PER_FILE_BATCH + 4) as u64;
    (archive_workers(config) as u64)
        .saturating_mul(buffers_per_worker)
        .saturating_mul(config.archive.chunk_size)
}

pub fn archive_pool(config: &Config) -> Result<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(archive_workers(config))
        .thread_name(|index| format!("akeep-worker-{index}"))
        .build()
        .context("failed to create bounded worker pool")
}

pub fn available_memory_bytes() -> Option<u64> {
    let mut system = System::new();
    system.refresh_memory();
    let available = system.available_memory();
    (available > 0).then_some(available)
}

pub fn available_disk_bytes(path: &Path) -> Result<u64> {
    fs2::available_space(path)
        .with_context(|| format!("failed to inspect free space at {}", path.display()))
}
