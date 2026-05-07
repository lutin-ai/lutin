//! Streaming HTTP download helper, shared across CP-side model
//! fetchers (whisper, TTS, future backends). Hoisted out of
//! `transcribe.rs` so the same atomic-rename + size-check scheme isn't
//! reimplemented per backend.
//!
//! Contract:
//! - downloads stream to `<dest>.tmp`, fsync, rename atomically;
//! - rejects partial transfers when the server advertises a length;
//! - the caller owns directory creation and "skip if already present".

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use futures_util::StreamExt;

/// 30-minute ceiling on a single download — large GGUFs (multi-GB
/// over a slow link) need more headroom than reqwest's default.
const DOWNLOAD_TIMEOUT_SECS: u64 = 60 * 30;

/// Stream `src` into `dest` via a `<dest>.tmp` staging file, then
/// `rename` into place once the body is fully written + fsynced. The
/// staging extension is fixed (no random suffix) so a crashed prior
/// run is overwritten on retry rather than accumulating debris.
pub async fn download_to(src: &str, dest: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .build()?;
    let response = client.get(src).send().await?.error_for_status()?;
    let total = response.content_length();
    let tmp = PathBuf::from(format!("{}.tmp", dest.display()));
    let tmp_for_open = tmp.clone();
    let (writer_tx, mut writer_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(8);
    let writer_task = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
        let mut file = std::fs::File::create(&tmp_for_open)?;
        let mut written = 0u64;
        while let Some(chunk) = writer_rx.blocking_recv() {
            file.write_all(&chunk)?;
            written += chunk.len() as u64;
        }
        file.sync_all()?;
        Ok(written)
    });

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if writer_tx.send(chunk).await.is_err() {
            break;
        }
    }
    drop(writer_tx);
    let written = writer_task
        .await
        .map_err(|e| anyhow!("download writer panicked: {e}"))??;

    if let Some(expected) = total
        && written != expected
    {
        return Err(anyhow!(
            "size mismatch: expected {expected}, wrote {written}"
        ));
    }
    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}
