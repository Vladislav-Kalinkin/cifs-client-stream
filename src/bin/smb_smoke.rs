use std::env;
use std::time::Duration;
use std::time::Instant;

use cifs_client::{
    media_presentations, resolve_smb_uri, Auth, Cifs, Error, MediaPresentation, StreamOptions,
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
        let mut stream = cifs
            .open_read_ahead_with_options(&share, &path, StreamOptions::default())
            .await?;
        let started = Instant::now();
        let mut total = 0usize;
        let mut slowest = Duration::ZERO;

        for block_index in 0..read_blocks {
            let block_started = Instant::now();
            let block = stream
                .read_block_timeout(&mut cifs, read_bytes, timeout)
                .await?
                .unwrap_or_default();
            let block_elapsed = block_started.elapsed();
            if block.is_empty() {
                println!("reached EOF after {} blocks", block_index);
                break;
            }
            total += block.len();
            slowest = slowest.max(block_elapsed);
            let block_mbps = if block_elapsed.is_zero() {
                0.0
            } else {
                (block.len() as f64 / (1024.0 * 1024.0)) / block_elapsed.as_secs_f64()
            };
            println!(
                "block {} read {} bytes in {:?} ({:.2} MiB/s) at source_position={} buffered={}",
                block_index + 1,
                block.len(),
                block_elapsed,
                block_mbps,
                stream.stats().source_position,
                stream.stats().buffered
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
        cifs.close_read_ahead(stream).await?;
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

fn presentation_name(presentation: &MediaPresentation) -> &'static str {
    match presentation {
        MediaPresentation::Folder { .. } => "folder",
        MediaPresentation::MovieFolder { .. } => "movie-folder",
        MediaPresentation::PlayableFile { .. } => "playable-file",
    }
}
