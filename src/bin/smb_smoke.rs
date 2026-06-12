use std::env;
use std::time::Duration;
use std::time::Instant;

use cifs_client::{
    media_presentations, resolve_smb_uri, Auth, Cifs, Error, MediaPresentation,
    SmbMediaStreamOptions, StreamOptions, StreamingWorkerOptions, StreamingWorkerStats,
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

    let print_entries = env_bool("SMB_PRINT_ENTRIES", false);
    let print_blocks = env_bool("SMB_PRINT_BLOCKS", false);
    let worker_prefill_high = env_bool("SMB_WORKER_PREFILL_HIGH", false);
    let worker_initial_buffer = env_usize("SMB_WORKER_INITIAL_BUFFER_BYTES", 1024 * 1024);

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

    let Some(share_name) = config.share else {
        println!("connected to \\\\{connect_host}");
        if connect_host != config.hostname {
            println!(
                "resolved uri host {} via SMB_HOST {connect_host}",
                config.hostname
            );
        }
        println!("server root URI detected: smb://{}/", config.hostname);
        println!("share discovery is not implemented yet");
        println!("next step: add SMB1 share listing and show shares as virtual Volumes");
        return Ok(());
    };

    let mount_path = format!("\\\\{}\\{}", connect_host, share_name);
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

    if print_entries {
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
    }

    if let Some(path) = read_path {
        println!(
            "stream options: read_bytes={} read_blocks={} read_ahead_capacity={} chunk_size={} mode=media-stream print_blocks={} prefill_high={}",
            read_bytes,
            read_blocks,
            stream_options.read_ahead_capacity,
            stream_options.chunk_size,
            print_blocks,
            worker_prefill_high
        );

        let default_low_watermark = worker_initial_buffer.max(read_bytes);
        let low_watermark = env_usize("SMB_LOW_WATERMARK_BYTES", default_low_watermark);
        let default_prefill_target = worker_initial_buffer
            .saturating_mul(2)
            .max(read_bytes)
            .min(stream_options.read_ahead_capacity);

        let high_watermark = env_usize_optional("SMB_WORKER_PREFILL_TARGET_BYTES")
            .unwrap_or_else(|| env_usize("SMB_HIGH_WATERMARK_BYTES", default_prefill_target));
        let worker_options =
            StreamingWorkerOptions::new(stream_options, low_watermark, high_watermark)?;
        let media_stream_options =
            SmbMediaStreamOptions::new(worker_options, worker_initial_buffer)?;

        println!(
                "worker options: low_watermark={} high_watermark={} initial_buffer={} prefill_target={}",
                media_stream_options.worker_options.low_watermark,
                media_stream_options.worker_options.high_watermark,
                media_stream_options.initial_buffer_size,
                media_stream_options.worker_options.high_watermark
            );

        let mut media_stream = cifs
            .open_media_stream_with_options(&share, &path, media_stream_options)
            .await?;

        let mut initial_prefill_elapsed = Duration::ZERO;
        let mut initial_prefill_bytes = 0usize;

        if media_stream_options.initial_buffer_size > 0 {
            let before = media_stream.stats();
            let initial_started = Instant::now();

            media_stream
                .fill_initial_buffer_timeout(&mut cifs, timeout)
                .await?;

            let initial_elapsed = initial_started.elapsed();
            let after = media_stream.stats();
            let source_delta = after.source_position.saturating_sub(before.source_position);

            initial_prefill_elapsed = initial_elapsed;
            initial_prefill_bytes = source_delta as usize;

            println!(
                concat!(
                    "initial worker buffer: source {}->{} source_delta={} ",
                    "in {:?} ({:.2} MiB/s) buffered={} chunks={}"
                ),
                before.source_position,
                after.source_position,
                source_delta,
                initial_elapsed,
                mib_per_second(initial_prefill_bytes, initial_elapsed),
                after.buffered,
                after.buffered_chunks
            );
        }

        let started = Instant::now();
        let mut measurements = SmokeMeasurements::new(read_blocks);
        let mut prefill_measurements = SmokePrefillMeasurements::default();

        for block_index in 0..read_blocks {
            let before = media_stream.stats();
            let block_started = Instant::now();
            let block = media_stream
                .read_block_timeout(&mut cifs, read_bytes, timeout)
                .await?
                .unwrap_or_default();
            let block_elapsed = block_started.elapsed();
            let after = media_stream.stats();

            if block.is_empty() {
                println!("reached EOF after {} blocks", block_index);
                break;
            }

            measurements.record_block(
                block_index,
                block.len(),
                block_elapsed,
                SmokeBlockStats::from_worker(&before),
                SmokeBlockStats::from_worker(&after),
                print_blocks,
            );

            if worker_prefill_high && media_stream.should_prefill() {
                let before_prefill = media_stream.stats();
                let prefill_started = Instant::now();

                media_stream
                    .maybe_prefill_timeout(&mut cifs, timeout)
                    .await?;

                let prefill_elapsed = prefill_started.elapsed();
                let after_prefill = media_stream.stats();
                let source_delta = after_prefill
                    .source_position
                    .saturating_sub(before_prefill.source_position)
                    as usize;

                prefill_measurements.record(source_delta, prefill_elapsed);

                if print_blocks && source_delta > 0 {
                    println!(
                        concat!(
                            "prefill after block {} source {}->{} source_delta={} ",
                            "in {:?} ({:.2} MiB/s) buffered={} chunks={}"
                        ),
                        block_index + 1,
                        before_prefill.source_position,
                        after_prefill.source_position,
                        source_delta,
                        prefill_elapsed,
                        mib_per_second(source_delta, prefill_elapsed),
                        after_prefill.buffered,
                        after_prefill.buffered_chunks
                    );
                }
            }
        }

        let elapsed = started.elapsed();
        measurements.print_summary(&path, elapsed);
        prefill_measurements.print_summary();

        if worker_initial_buffer > 0 {
            let total_with_initial = elapsed + initial_prefill_elapsed;
            println!(
                    "total including initial buffer: delivered={} bytes in {:?} ({:.2} MiB/s delivered), initial_buffer={} bytes ({:.2} MiB/s source)",
                    measurements.total,
                    total_with_initial,
                    mib_per_second(measurements.total, total_with_initial),
                    initial_prefill_bytes,
                    mib_per_second(initial_prefill_bytes, initial_prefill_elapsed)
                );
        }

        cifs.close_media_stream(media_stream).await?;
    }

    cifs.umount(share).await?;
    Ok(())
}

#[derive(Clone, Copy)]
struct SmokeBlockStats {
    source_position: u64,
    prefetched: u64,
    buffered: usize,
    buffered_chunks: usize,
    remaining: u64,
}

impl SmokeBlockStats {
    fn from_worker(stats: &StreamingWorkerStats) -> Self {
        Self {
            source_position: stats.source_position,
            prefetched: stats.prefetched(),
            buffered: stats.buffered,
            buffered_chunks: stats.buffered_chunks,
            remaining: stats.remaining(),
        }
    }
}

struct SmokeMeasurements {
    total: usize,
    slowest: Duration,
    block_times: Vec<Duration>,
    refill_times: Vec<Duration>,
    cached_times: Vec<Duration>,
    refill_bytes: usize,
    cached_bytes: usize,
}

impl SmokeMeasurements {
    fn new(read_blocks: usize) -> Self {
        Self {
            total: 0,
            slowest: Duration::ZERO,
            block_times: Vec::with_capacity(read_blocks),
            refill_times: Vec::new(),
            cached_times: Vec::new(),
            refill_bytes: 0,
            cached_bytes: 0,
        }
    }

    fn record_block(
        &mut self,
        block_index: usize,
        block_len: usize,
        block_elapsed: Duration,
        before: SmokeBlockStats,
        after: SmokeBlockStats,
        print_blocks: bool,
    ) {
        self.total += block_len;
        self.slowest = self.slowest.max(block_elapsed);
        self.block_times.push(block_elapsed);

        let source_delta = after.source_position.saturating_sub(before.source_position) as usize;
        if source_delta > 0 {
            self.refill_times.push(block_elapsed);
            self.refill_bytes += source_delta;
        } else {
            self.cached_times.push(block_elapsed);
            self.cached_bytes += block_len;
        }

        if print_blocks {
            println!(
                concat!(
                    "block {} read {} bytes in {:?} ({:.2} MiB/s) ",
                    "source {}->{} source_delta={} ",
                    "prefetched={} buffered={} chunks={} remaining={}"
                ),
                block_index + 1,
                block_len,
                block_elapsed,
                mib_per_second(block_len, block_elapsed),
                before.source_position,
                after.source_position,
                source_delta,
                after.prefetched,
                after.buffered,
                after.buffered_chunks,
                after.remaining
            );
        }
    }

    fn print_summary(&mut self, path: &str, elapsed: Duration) {
        println!(
            "read {} bytes from {} in {:?} ({:.2} MiB/s), slowest block {:?}",
            self.total,
            path,
            elapsed,
            mib_per_second(self.total, elapsed),
            self.slowest
        );

        if !self.refill_times.is_empty() {
            let refill_elapsed: Duration = self.refill_times.iter().copied().sum();

            println!(
                "refill blocks: {} source bytes {} in {:?} ({:.2} MiB/s), p95 {:?}, p99 {:?}",
                self.refill_times.len(),
                self.refill_bytes,
                refill_elapsed,
                mib_per_second(self.refill_bytes, refill_elapsed),
                percentile_duration(&mut self.refill_times, 95),
                percentile_duration(&mut self.refill_times, 99)
            );
        }

        if !self.cached_times.is_empty() {
            println!(
                "cached blocks: {} delivered bytes {}, p95 {:?}, p99 {:?}",
                self.cached_times.len(),
                self.cached_bytes,
                percentile_duration(&mut self.cached_times, 95),
                percentile_duration(&mut self.cached_times, 99)
            );
        }

        if !self.block_times.is_empty() {
            println!(
                "block latency: p95 {:?}, p99 {:?}",
                percentile_duration(&mut self.block_times, 95),
                percentile_duration(&mut self.block_times, 99)
            );
        }
    }
}

#[derive(Default)]
struct SmokePrefillMeasurements {
    events: usize,
    source_bytes: usize,
    elapsed: Duration,
    slowest: Duration,
}

impl SmokePrefillMeasurements {
    fn record(&mut self, source_bytes: usize, elapsed: Duration) {
        if source_bytes == 0 {
            return;
        }

        self.events += 1;
        self.source_bytes += source_bytes;
        self.elapsed += elapsed;
        self.slowest = self.slowest.max(elapsed);
    }

    fn print_summary(&self) {
        if self.events == 0 {
            return;
        }

        println!(
            "prefill events: {} source bytes {} in {:?} ({:.2} MiB/s), slowest {:?}",
            self.events,
            self.source_bytes,
            self.elapsed,
            mib_per_second(self.source_bytes, self.elapsed),
            self.slowest
        );
    }
}

fn mib_per_second(bytes: usize, elapsed: Duration) -> f64 {
    if elapsed.is_zero() {
        0.0
    } else {
        (bytes as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64()
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_usize_optional(name: &str) -> Option<usize> {
    env::var(name).ok().and_then(|value| value.parse().ok())
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
