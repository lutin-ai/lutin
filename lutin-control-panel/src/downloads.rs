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
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use tracing::info;

/// 30-minute ceiling on a single download — large GGUFs (multi-GB
/// over a slow link) need more headroom than reqwest's default.
const DOWNLOAD_TIMEOUT_SECS: u64 = 60 * 30;

/// How often to fire the progress callback while bytes are streaming.
/// Throttling matters: callers broadcast each tick to every connected
/// client, and an unthrottled per-chunk emit on a multi-GB body would
/// flood the WS even on a fast link.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(2);

/// Stream `src` into `dest` via a `<dest>.tmp` staging file, then
/// `rename` into place once the body is fully written + fsynced. The
/// staging extension is fixed (no random suffix) so a crashed prior
/// run is overwritten on retry rather than accumulating debris.
pub async fn download_to(src: &str, dest: &Path) -> Result<()> {
    download_to_with_progress(src, dest, |_, _| {}).await
}

/// Same as [`download_to`] but invokes `on_progress(downloaded, total)`
/// at most once per [`PROGRESS_INTERVAL`] plus once on completion.
/// `total` is `Some` iff the server sent `Content-Length`. The
/// callback is also a `tracing::info!` site so progress is visible in
/// CP logs without needing the wire-broadcast path.
pub async fn download_to_with_progress<F>(
    src: &str,
    dest: &Path,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(u64, Option<u64>),
{
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
    let mut downloaded: u64 = 0;
    let mut last_emit = Instant::now();
    let dest_label = dest
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("download")
        .to_owned();
    // Fire once at 0 so the UI can show "starting…" before any bytes
    // arrive — most useful when `total` is unknown and the first
    // chunk takes a while.
    on_progress(0, total);
    info!(file = %dest_label, total = ?total, "download start");
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        downloaded += chunk.len() as u64;
        if writer_tx.send(chunk).await.is_err() {
            break;
        }
        if last_emit.elapsed() >= PROGRESS_INTERVAL {
            last_emit = Instant::now();
            info!(
                file = %dest_label,
                downloaded,
                total = ?total,
                pct = total.map(|t| downloaded * 100 / t.max(1)),
                "download progress",
            );
            on_progress(downloaded, total);
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
    // Final 100% emission so subscribers can see "done" even if the
    // last in-loop tick fell just before completion.
    on_progress(written, total.or(Some(written)));
    info!(file = %dest_label, downloaded = written, "download complete");
    Ok(())
}
