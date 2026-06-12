use std::env;
use std::time::Duration;
use std::time::Instant;

use cifs_client::{
    media_presentations, resolve_smb_uri, Auth, Cifs, Error, MediaPresentation, StreamOptions,
    StreamingWorkerOptions,
};

const DEFAULT_READ_BYTES: usize = 256 * 1024;
const DEFAULT_READ_BLOCKS: usize = 1;
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const MAX_ENTRIES_TO_PRINT: usize = 25;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("smb smoke failed: {error}");
        eprintln!(
            "kind={:?} retryable={} timeout={} connection_lost={}",
            error.kind(),
            error.is_retryable(),
            error.is_timeout(),
            error.is_connection_lost()
        );
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Error> {
    let uri = env::var("SMB_URI").map_err(|_| {
        Error::InvalidConfig(
            "SMB_URI is required, for example smb://user:pass@host/share/path".into(),
        )
    })?;
    let host = env::var("SMB_HOST").ok();
    let user = env::var("SMB_USER").ok();
    let password = env::var("SMB_PASSWORD").ok();
    let domain = env::var("SMB_DOMAIN").ok();
    let read_path = env::var("SMB_READ_PATH").ok();
    let timeout = env_u64("SMB_TIMEOUT_MS", DEFAULT_TIMEOUT_MS);
    let read_bytes = env_usize("SMB_READ_BYTES", DEFAULT_READ_BYTES);
    let read_blocks = env_usize("SMB_READ_BLOCKS", DEFAULT_READ_BLOCKS);

    let default_stream_options = StreamOptions::default();
    let read_ahead_bytes = env_usize(
        "SMB_READ_AHEAD_BYTES",
        default_stream_options.read_ahead_capacity,
    );
    let chunk_size = env_u16("SMB_CHUNK_SIZE", default_stream_options.chunk_size);
    let stream_options = StreamOptions::new(read_ahead_bytes, chunk_size)?;
    let use_streaming_worker = env_bool("SMB_STREAMING_WORKER", false);

    let timeout = Duration::from_millis(timeout);

    let config = resolve_smb_uri(&uri)?;
    let connect_host = host.as_deref().unwrap_or(config.hostname);
    let auth_user = user.as_deref().or(config.user);
    let auth_password = password.as_deref().or(config.password).unwrap_or("");
    let auth_domain = domain
        .as_deref()
        .or(config.domain)
        .unwrap_or(config.hostname);
    let auth = auth_user.map(|user| Auth::new(user, "CIFSCLIENT", auth_domain, auth_password));

    let mut cifs = Cifs::open_timeout(connect_host, config.port, auth, timeout).await?;
    let mount_path = format!("\\\\{}\\{}", connect_host, config.share);
    let share = cifs.mount(&mount_path).await?;

    let pattern = match config.path {
        Some(path) if !path.is_empty() => format!("{path}/*"),
        _ => "*".to_owned(),
    };

    let mut reader = cifs
        .open_dir_reader_timeout(&share, &pattern, timeout)
        .await?;
    let entries = reader
        .next_media_entries_timeout(&mut cifs, timeout)
        .await?
        .unwrap_or_default();
    let presentations = media_presentations(&entries);

    println!("connected to {mount_path}");
    if connect_host != config.hostname {
        println!(
            "resolved uri host {} via SMB_HOST {connect_host}",
            config.hostname
        );
    }
    println!("listed pattern: {pattern}");
    println!("media entries in first batch: {}", entries.len());
    for (entry, presentation) in entries
        .iter()
        .zip(presentations.iter())
        .take(MAX_ENTRIES_TO_PRINT)
    {
        println!(
            "- {:?} {} size={} presentation={}",
            entry.kind,
            entry.name,
            entry.size,
            presentation_name(presentation)
        );
    }

    if let Some(path) = read_path {
        println!(
            "stream options: read_bytes={} read_blocks={} read_ahead_capacity={} chunk_size={} mode={}",
            read_bytes,
            read_blocks,
            stream_options.read_ahead_capacity,
            stream_options.chunk_size,
            if use_streaming_worker {
                "streaming-worker"
            } else {
                "read-ahead"
            }
        );

        if use_streaming_worker {
            let low_watermark = env_usize(
                "SMB_LOW_WATERMARK_BYTES",
                stream_options.read_ahead_capacity / 4,
            );
            let high_watermark = env_usize(
                "SMB_HIGH_WATERMARK_BYTES",
                stream_options.read_ahead_capacity,
            );
            let worker_options = StreamingWorkerOptions::new(
                stream_options,
                low_watermark,
                high_watermark,
                read_bytes,
            )?;

            println!(
                "worker options: low_watermark={} high_watermark={} read_request_size={}",
                worker_options.low_watermark,
                worker_options.high_watermark,
                worker_options.read_request_size
            );

            let mut worker = cifs
                .open_streaming_worker_with_options(&share, &path, worker_options)
                .await?;

            let started = Instant::now();
            let mut total = 0usize;
            let mut slowest = Duration::ZERO;
            let mut block_times = Vec::with_capacity(read_blocks);
            let mut refill_times = Vec::new();
            let mut cached_times = Vec::new();
            let mut refill_bytes = 0usize;
            let mut cached_bytes = 0usize;

            for block_index in 0..read_blocks {
                let before = worker.stats();
                let block_started = Instant::now();
                let block = worker
                    .read_block_timeout(&mut cifs, read_bytes, timeout)
                    .await?
                    .unwrap_or_default();
                let block_elapsed = block_started.elapsed();
                let after = worker.stats();

                if block.is_empty() {
                    println!("reached EOF after {} blocks", block_index);
                    break;
                }

                total += block.len();
                slowest = slowest.max(block_elapsed);
                block_times.push(block_elapsed);

                let source_delta =
                    after.source_position.saturating_sub(before.source_position) as usize;

                if source_delta > 0 {
                    refill_times.push(block_elapsed);
                    refill_bytes += source_delta;
                } else {
                    cached_times.push(block_elapsed);
                    cached_bytes += block.len();
                }

                let block_mbps = if block_elapsed.is_zero() {
                    0.0
                } else {
                    (block.len() as f64 / (1024.0 * 1024.0)) / block_elapsed.as_secs_f64()
                };

                println!(
                    concat!(
                        "block {} read {} bytes in {:?} ({:.2} MiB/s) ",
                        "playback {}->{} source {}->{} source_delta={} ",
                        "prefetched={} buffered={} chunks={} remaining={}"
                    ),
                    block_index + 1,
                    block.len(),
                    block_elapsed,
                    block_mbps,
                    before.playback_position,
                    after.playback_position,
                    before.source_position,
                    after.source_position,
                    source_delta,
                    after.prefetched(),
                    after.buffered,
                    after.buffered_chunks,
                    after.remaining()
                );
            }

            let elapsed = started.elapsed();
            let mbps = if elapsed.is_zero() {
                0.0
            } else {
                (total as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64()
            };

            println!(
                "read {} bytes from {} in {:?} ({:.2} MiB/s), slowest block {:?}",
                total, path, elapsed, mbps, slowest
            );

            if !refill_times.is_empty() {
                let refill_elapsed: Duration = refill_times.iter().copied().sum();
                let refill_mbps = if refill_elapsed.is_zero() {
                    0.0
                } else {
                    (refill_bytes as f64 / (1024.0 * 1024.0)) / refill_elapsed.as_secs_f64()
                };

                println!(
                    "refill blocks: {} source bytes {} in {:?} ({:.2} MiB/s), p95 {:?}, p99 {:?}",
                    refill_times.len(),
                    refill_bytes,
                    refill_elapsed,
                    refill_mbps,
                    percentile_duration(&mut refill_times, 95),
                    percentile_duration(&mut refill_times, 99)
                );
            }

            if !cached_times.is_empty() {
                println!(
                    "cached blocks: {} delivered bytes {}, p95 {:?}, p99 {:?}",
                    cached_times.len(),
                    cached_bytes,
                    percentile_duration(&mut cached_times, 95),
                    percentile_duration(&mut cached_times, 99)
                );
            }

            if !block_times.is_empty() {
                println!(
                    "block latency: p95 {:?}, p99 {:?}",
                    percentile_duration(&mut block_times, 95),
                    percentile_duration(&mut block_times, 99)
                );
            }

            cifs.close_streaming_worker(worker).await?;
        } else {
            let mut stream = cifs
                .open_read_ahead_with_options(&share, &path, stream_options)
                .await?;

            let started = Instant::now();
            let mut total = 0usize;
            let mut slowest = Duration::ZERO;
            let mut block_times = Vec::with_capacity(read_blocks);
            let mut refill_times = Vec::new();
            let mut cached_times = Vec::new();
            let mut refill_bytes = 0usize;
            let mut cached_bytes = 0usize;

            for block_index in 0..read_blocks {
                let before = stream.stats();
                let block_started = Instant::now();
                let block = stream
                    .read_block_timeout(&mut cifs, read_bytes, timeout)
                    .await?
                    .unwrap_or_default();
                let block_elapsed = block_started.elapsed();
                let after = stream.stats();

                if block.is_empty() {
                    println!("reached EOF after {} blocks", block_index);
                    break;
                }

                total += block.len();
                slowest = slowest.max(block_elapsed);
                block_times.push(block_elapsed);

                let source_delta =
                    after.source_position.saturating_sub(before.source_position) as usize;

                if source_delta > 0 {
                    refill_times.push(block_elapsed);
                    refill_bytes += source_delta;
                } else {
                    cached_times.push(block_elapsed);
                    cached_bytes += block.len();
                }

                let block_mbps = if block_elapsed.is_zero() {
                    0.0
                } else {
                    (block.len() as f64 / (1024.0 * 1024.0)) / block_elapsed.as_secs_f64()
                };

                println!(
                    concat!(
                        "block {} read {} bytes in {:?} ({:.2} MiB/s) ",
                        "playback {}->{} source {}->{} source_delta={} ",
                        "prefetched={} buffered={} chunks={} remaining={}"
                    ),
                    block_index + 1,
                    block.len(),
                    block_elapsed,
                    block_mbps,
                    before.position,
                    after.position,
                    before.source_position,
                    after.source_position,
                    source_delta,
                    after.prefetched(),
                    after.buffered,
                    after.buffered_chunks,
                    after.remaining()
                );
            }

            let elapsed = started.elapsed();
            let mbps = if elapsed.is_zero() {
                0.0
            } else {
                (total as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64()
            };

            println!(
                "read {} bytes from {} in {:?} ({:.2} MiB/s), slowest block {:?}",
                total, path, elapsed, mbps, slowest
            );

            if !refill_times.is_empty() {
                let refill_elapsed: Duration = refill_times.iter().copied().sum();
                let refill_mbps = if refill_elapsed.is_zero() {
                    0.0
                } else {
                    (refill_bytes as f64 / (1024.0 * 1024.0)) / refill_elapsed.as_secs_f64()
                };

                println!(
                    "refill blocks: {} source bytes {} in {:?} ({:.2} MiB/s), p95 {:?}, p99 {:?}",
                    refill_times.len(),
                    refill_bytes,
                    refill_elapsed,
                    refill_mbps,
                    percentile_duration(&mut refill_times, 95),
                    percentile_duration(&mut refill_times, 99)
                );
            }

            if !cached_times.is_empty() {
                println!(
                    "cached blocks: {} delivered bytes {}, p95 {:?}, p99 {:?}",
                    cached_times.len(),
                    cached_bytes,
                    percentile_duration(&mut cached_times, 95),
                    percentile_duration(&mut cached_times, 99)
                );
            }

            if !block_times.is_empty() {
                println!(
                    "block latency: p95 {:?}, p99 {:?}",
                    percentile_duration(&mut block_times, 95),
                    percentile_duration(&mut block_times, 99)
                );
            }

            cifs.close_read_ahead(stream).await?;
        }
    }

    cifs.umount(share).await?;
    Ok(())
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_u16(name: &str, default: u16) -> u16 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .and_then(|value| match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

fn percentile_duration(values: &mut [Duration], percentile: usize) -> Duration {
    values.sort_unstable();
    let index = (values.len() * percentile).div_ceil(100).saturating_sub(1);
    values[index]
}

fn presentation_name(presentation: &MediaPresentation) -> &'static str {
    match presentation {
        MediaPresentation::Folder { .. } => "folder",
        MediaPresentation::MovieFolder { .. } => "movie-folder",
        MediaPresentation::PlayableFile { .. } => "playable-file",
    }
}
