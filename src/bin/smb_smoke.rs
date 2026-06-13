use std::env;
use std::io::SeekFrom;
use std::time::Duration;
use std::time::Instant;

use cifs_client_stream::{
    Auth, Cifs, CifsIoStats, Error, MediaEntry, MediaFolderSummary, MediaPresentation, Share,
    SmbMediaStream, SmbMediaStreamOptions, StreamOptions, StreamingWorkerOptions,
    StreamingWorkerStats, media_presentations, media_presentations_with_summaries, resolve_smb_uri,
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
    let uri = env::var("SMB_URI").ok();
    let host = env::var("SMB_HOST").ok();
    let share_from_env = env::var("SMB_SHARE")
        .ok()
        .or_else(|| env::var("SMB_VOLUME_NAME").ok())
        .or_else(|| env::var("SMB_DISK_NAME").ok());
    let user = env::var("SMB_USER").ok();
    let password = env::var("SMB_PASSWORD").ok();
    let domain = env::var("SMB_DOMAIN").ok();
    let read_path = env::var("SMB_READ_PATH").ok();
    let list_path_env = env::var("SMB_LIST_PATH").ok();
    let report_path = env::var("SMB_REPORT_PATH").ok();
    let timeout = env_u64("SMB_TIMEOUT_MS", DEFAULT_TIMEOUT_MS);
    let read_bytes = env_usize("SMB_READ_BYTES", DEFAULT_READ_BYTES);
    let read_blocks = env_usize("SMB_READ_BLOCKS", DEFAULT_READ_BLOCKS);

    let default_stream_options = StreamOptions::default();
    let read_ahead_bytes = env_usize(
        "SMB_READ_AHEAD_BYTES",
        default_stream_options.read_ahead_capacity,
    );
    let chunk_size = env_u32("SMB_CHUNK_SIZE", default_stream_options.chunk_size);
    let stream_options = StreamOptions::new(read_ahead_bytes, chunk_size)?;

    let print_entries = env_bool("SMB_PRINT_ENTRIES", false);
    let print_blocks = env_bool("SMB_PRINT_BLOCKS", false);
    let scan_folder_summaries = env_bool("SMB_SCAN_FOLDER_SUMMARIES", false);
    let worker_prefill_high = env_bool("SMB_WORKER_PREFILL_HIGH", false);
    let seek_test = env_bool("SMB_SEEK_TEST", false);
    let worker_initial_buffer = env_usize("SMB_WORKER_INITIAL_BUFFER_BYTES", 1024 * 1024);
    let pipeline_depth = env_usize(
        "SMB_PIPELINE_DEPTH",
        StreamingWorkerOptions::default().pipeline_depth,
    );

    let timeout = Duration::from_millis(timeout);

    let parsed = uri.as_deref().map(resolve_smb_uri).transpose()?;

    let parsed_hostname = parsed.as_ref().map(|config| config.hostname);
    let parsed_port = parsed.as_ref().and_then(|config| config.port);
    let parsed_domain = parsed.as_ref().and_then(|config| config.domain);
    let parsed_user = parsed.as_ref().and_then(|config| config.user);
    let parsed_password = parsed.as_ref().and_then(|config| config.password);
    let parsed_share = parsed.as_ref().and_then(|config| config.share);
    let parsed_path = parsed.as_ref().and_then(|config| config.path);

    let connect_host = host.as_deref().or(parsed_hostname).ok_or_else(|| {
        Error::InvalidConfig("SMB_HOST is required when SMB_URI is not set".into())
    })?;

    let auth_user = user.as_deref().or(parsed_user);
    let auth_password = password.as_deref().or(parsed_password).unwrap_or("");
    let auth_domain = domain.as_deref().or(parsed_domain).unwrap_or(connect_host);
    let auth = auth_user.map(|user| Auth::new(user, "CIFSCLIENT", auth_domain, auth_password));

    let mut cifs = Cifs::open_timeout(connect_host, parsed_port, auth, timeout).await?;

    let share_name = parsed_share.map(ToOwned::to_owned).or(share_from_env);

    let Some(share_name) = share_name else {
        println!("connected to \\\\{connect_host}");
        if let Some(parsed_hostname) = parsed_hostname
            && connect_host != parsed_hostname
        {
            println!("resolved uri host {parsed_hostname} via SMB_HOST {connect_host}");
        }
        println!("SMB share name is required for this server");
        println!("set SMB_SHARE, for example: SMB_SHARE='HARD'");
        return Ok(());
    };

    if uri.is_some() && parsed_share.is_none() {
        println!("server root URI detected");
    }

    if parsed_share.is_none() {
        println!("using SMB share from environment: {share_name}");
    }

    let mount_path = format!("\\\\{}\\{}", connect_host, share_name);
    let share = cifs.mount(&mount_path).await?;

    let list_path = list_path_env.as_deref().or(parsed_path);

    let pattern = match list_path {
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

    let folder_summaries = if scan_folder_summaries {
        scan_media_folder_summaries(&mut cifs, &share, list_path, &entries, timeout).await?
    } else {
        Vec::new()
    };

    let presentations = if folder_summaries.is_empty() {
        media_presentations(&entries)
    } else {
        media_presentations_with_summaries(&entries, &folder_summaries)
    };

    println!("connected to {mount_path}");
    if let Some(parsed_hostname) = parsed_hostname
        && connect_host != parsed_hostname
    {
        println!("resolved uri host {parsed_hostname} via SMB_HOST {connect_host}");
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

            if let MediaPresentation::MovieFolder { summary, .. } = presentation {
                println!(
                    "  movie-folder: main_video={:?} primary_videos={} extras={} audio_tracks={}",
                    summary.main_video,
                    summary.primary_videos.len(),
                    summary.extras.len(),
                    summary.audio_tracks.len()
                );
            }
        }
    }

    if let Some(path) = read_path {
        println!(
            "stream options: read_bytes={} read_blocks={} read_ahead_capacity={} configured_chunk_size={} effective_chunk_size={} mode=media-stream print_blocks={} prefill_high={} seek_test={} pipeline_depth={}",
            read_bytes,
            read_blocks,
            stream_options.read_ahead_capacity,
            stream_options.chunk_size,
            stream_options.effective_chunk_size(),
            print_blocks,
            worker_prefill_high,
            seek_test,
            pipeline_depth
        );

        let default_low_watermark = worker_initial_buffer.max(read_bytes);
        let low_watermark = env_usize("SMB_LOW_WATERMARK_BYTES", default_low_watermark);
        let default_prefill_target = worker_initial_buffer
            .saturating_mul(2)
            .max(read_bytes)
            .min(stream_options.read_ahead_capacity);

        let high_watermark = env_usize_optional("SMB_WORKER_PREFILL_TARGET_BYTES")
            .unwrap_or_else(|| env_usize("SMB_HIGH_WATERMARK_BYTES", default_prefill_target));
        let worker_options = StreamingWorkerOptions::new_with_pipeline_depth(
            stream_options,
            low_watermark,
            high_watermark,
            pipeline_depth,
        )?;
        let media_stream_options =
            SmbMediaStreamOptions::new(worker_options, worker_initial_buffer)?;

        println!(
            "worker options: low_watermark={} high_watermark={} initial_buffer={} prefill_target={} pipeline_depth={}",
            media_stream_options.worker_options.low_watermark,
            media_stream_options.worker_options.high_watermark,
            media_stream_options.initial_buffer_size,
            media_stream_options.worker_options.high_watermark,
            media_stream_options.worker_options.pipeline_depth
        );

        cifs.reset_io_stats();
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

            print_io_stats("initial source reads", cifs.io_stats());
            cifs.reset_io_stats();
        }

        if seek_test {
            cifs.reset_io_stats();

            let started = Instant::now();
            let seek_measurements =
                run_seek_smoke_test(&mut cifs, &mut media_stream, read_bytes, timeout).await?;
            let elapsed = started.elapsed();
            let seek_io_stats = cifs.io_stats();

            let seek_summary =
                format_seek_report_summary(&seek_measurements, elapsed, seek_io_stats);

            print!("{seek_summary}");

            save_report_summary_if_requested(report_path.as_deref(), &seek_summary)?;
        } else {
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
            let stream_io_stats = cifs.io_stats();

            measurements.print_summary(&path, elapsed);
            prefill_measurements.print_summary();
            print_io_stats("stream source reads", stream_io_stats);

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

            let report_summary = format_report_summary(SmokeReportSummary {
                path: &path,
                read_bytes,
                read_blocks,
                stream_options: &stream_options,
                media_stream_options: &media_stream_options,
                measurements: &measurements,
                read_elapsed: elapsed,
                initial_prefill_bytes,
                initial_prefill_elapsed,
                stream_io_stats,
            });

            print!("{report_summary}");

            save_report_summary_if_requested(report_path.as_deref(), &report_summary)?;
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

struct SmokeReportSummary<'a> {
    path: &'a str,
    read_bytes: usize,
    read_blocks: usize,
    stream_options: &'a StreamOptions,
    media_stream_options: &'a SmbMediaStreamOptions,
    measurements: &'a SmokeMeasurements,
    read_elapsed: Duration,
    initial_prefill_bytes: usize,
    initial_prefill_elapsed: Duration,
    stream_io_stats: CifsIoStats,
}

struct SmokeSeekMeasurements {
    file_size: u64,
    read_bytes: usize,
    steps: Vec<SmokeSeekStep>,
}

struct SmokeSeekStep {
    label: &'static str,
    requested_offset: u64,
    actual_offset: u64,
    read_len: usize,
    elapsed: Duration,
    playback_position_after: u64,
    source_position_after: u64,
    buffered_after: usize,
}

async fn scan_media_folder_summaries(
    cifs: &mut Cifs,
    share: &Share,
    parent_path: Option<&str>,
    entries: &[MediaEntry],
    timeout: Duration,
) -> Result<Vec<(usize, MediaFolderSummary)>, Error> {
    let mut summaries = Vec::new();

    for (index, entry) in entries.iter().enumerate() {
        if !entry.is_folder() {
            continue;
        }

        let folder_path = join_smb_path(parent_path, &entry.name);
        let pattern = format!("{folder_path}/*");

        let mut reader = cifs
            .open_dir_reader_timeout(share, &pattern, timeout)
            .await?;

        let child_entries = reader
            .next_media_entries_timeout(cifs, timeout)
            .await?
            .unwrap_or_default();

        let summary = MediaFolderSummary::from_entries(&child_entries);

        if summary.can_collapse_to_movie() {
            summaries.push((index, summary));
        }
    }

    Ok(summaries)
}

fn join_smb_path(parent: Option<&str>, child: &str) -> String {
    match parent.map(str::trim).filter(|path| !path.is_empty()) {
        Some(parent) => format!("{}/{}", parent.trim_end_matches('/'), child),
        None => child.to_owned(),
    }
}

fn mib_per_second(bytes: usize, elapsed: Duration) -> f64 {
    if elapsed.is_zero() {
        0.0
    } else {
        (bytes as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64()
    }
}

fn print_io_stats(label: &str, stats: CifsIoStats) {
    if stats.read_at_calls == 0 {
        return;
    }

    println!(
        "{}: calls={} bytes={} avg_size={} avg_latency={:?} summed_source_time={:?} summed_source_rate={:.2} MiB/s",
        label,
        stats.read_at_calls,
        stats.read_at_bytes,
        stats.average_read_size(),
        stats.average_read_latency(),
        stats.read_at_elapsed,
        stats.read_throughput_mib_per_second()
    );
}

fn format_seek_report_summary(
    measurements: &SmokeSeekMeasurements,
    elapsed: Duration,
    io_stats: CifsIoStats,
) -> String {
    let mut out = String::new();

    out.push('\n');
    out.push_str("--- SMOKE SEEK TEST SUMMARY ---\n");
    out.push_str(&format!("file_size: {}\n", measurements.file_size));
    out.push_str(&format!("seek_read_bytes: {}\n", measurements.read_bytes));
    out.push_str(&format!("seek_steps: {}\n", measurements.steps.len()));
    out.push_str(&format!("total_elapsed: {:?}\n", elapsed));

    for step in &measurements.steps {
        out.push_str(&format!(
            "step: {} requested_offset={} actual_offset={} read_len={} elapsed={:?} playback_after={} source_after={} buffered_after={} read_mib_s={:.2}\n",
            step.label,
            step.requested_offset,
            step.actual_offset,
            step.read_len,
            step.elapsed,
            step.playback_position_after,
            step.source_position_after,
            step.buffered_after,
            mib_per_second(step.read_len, step.elapsed)
        ));
    }

    out.push_str(&format!(
        "internal_read_calls: {}\n",
        io_stats.read_at_calls
    ));
    out.push_str(&format!(
        "internal_read_avg_size: {}\n",
        io_stats.average_read_size()
    ));
    out.push_str(&format!(
        "internal_read_avg_latency: {:?}\n",
        io_stats.average_read_latency()
    ));
    out.push_str(&format!(
        "internal_summed_source_time: {:?}\n",
        io_stats.read_at_elapsed
    ));
    out.push_str(&format!(
        "internal_summed_source_rate_mib_s: {:.2}\n",
        io_stats.read_throughput_mib_per_second()
    ));
    out.push_str("--- END SMOKE SEEK TEST SUMMARY ---\n");

    out
}

fn format_report_summary(summary: SmokeReportSummary<'_>) -> String {
    let total_elapsed = summary.read_elapsed + summary.initial_prefill_elapsed;

    format!(
        concat!(
            "\n",
            "--- SMOKE REPORT SUMMARY ---\n",
            "read_path: {}\n",
            "read_bytes: {}\n",
            "read_blocks_requested: {}\n",
            "delivered_bytes: {}\n",
            "configured_chunk_size: {}\n",
            "effective_chunk_size: {}\n",
            "read_ahead_capacity: {}\n",
            "initial_buffer: {}\n",
            "low_watermark: {}\n",
            "high_watermark: {}\n",
            "pipeline_depth: {}\n",
            "initial_buffer_mib_s: {:.2}\n",
            "read_phase_mib_s: {:.2}\n",
            "total_mib_s: {:.2}\n",
            "slowest_block: {:?}\n",
            "refill_blocks: {}\n",
            "cached_blocks: {}\n",
            "block_latency_p95: {:?}\n",
            "block_latency_p99: {:?}\n",
            "internal_read_calls: {}\n",
            "internal_read_avg_size: {}\n",
            "internal_read_avg_latency: {:?}\n",
            "internal_summed_source_time: {:?}\n",
            "internal_summed_source_rate_mib_s: {:.2}\n",
            "--- END SMOKE REPORT SUMMARY ---\n"
        ),
        summary.path,
        summary.read_bytes,
        summary.read_blocks,
        summary.measurements.total,
        summary.stream_options.chunk_size,
        summary.stream_options.effective_chunk_size(),
        summary.stream_options.read_ahead_capacity,
        summary.media_stream_options.initial_buffer_size,
        summary.media_stream_options.worker_options.low_watermark,
        summary.media_stream_options.worker_options.high_watermark,
        summary.media_stream_options.worker_options.pipeline_depth,
        mib_per_second(
            summary.initial_prefill_bytes,
            summary.initial_prefill_elapsed,
        ),
        mib_per_second(summary.measurements.total, summary.read_elapsed),
        mib_per_second(summary.measurements.total, total_elapsed),
        summary.measurements.slowest,
        summary.measurements.refill_times.len(),
        summary.measurements.cached_times.len(),
        percentile_duration_copy(&summary.measurements.block_times, 95),
        percentile_duration_copy(&summary.measurements.block_times, 99),
        summary.stream_io_stats.read_at_calls,
        summary.stream_io_stats.average_read_size(),
        summary.stream_io_stats.average_read_latency(),
        summary.stream_io_stats.read_at_elapsed,
        summary.stream_io_stats.read_throughput_mib_per_second()
    )
}

fn save_report_summary_if_requested(
    report_path: Option<&str>,
    report_summary: &str,
) -> Result<(), Error> {
    let Some(report_path) = report_path else {
        return Ok(());
    };

    std::fs::write(report_path, report_summary).map_err(|error| {
        Error::InvalidConfig(format!(
            "failed to write SMB report summary to {report_path}: {error}"
        ))
    })?;

    println!("saved smoke report summary to {report_path}");

    Ok(())
}

async fn run_seek_smoke_test(
    cifs: &mut Cifs,
    media_stream: &mut SmbMediaStream,
    read_bytes: usize,
    timeout: Duration,
) -> Result<SmokeSeekMeasurements, Error> {
    if read_bytes == 0 {
        return Err(Error::InvalidConfig(
            "SMB seek test read size must be greater than zero".to_owned(),
        ));
    }

    let file_size = media_stream.stats().file_size;
    if file_size == 0 {
        return Err(Error::InvalidConfig(
            "SMB seek test requires a non-empty file".to_owned(),
        ));
    }

    let read_bytes = read_bytes.min(file_size.min(usize::MAX as u64) as usize);
    let quarter = file_size / 4;
    let half = file_size / 2;
    let near_end = file_size.saturating_sub(read_bytes as u64);
    let back = file_size / 10;

    let seek_points = [
        ("start", 0),
        ("quarter", quarter),
        ("half", half),
        ("near_end", near_end),
        ("back_to_10_percent", back),
    ];

    let mut steps = Vec::with_capacity(seek_points.len());

    println!("seek test: file_size={file_size} read_bytes={read_bytes}");

    for (label, requested_offset) in seek_points {
        let actual_offset = media_stream.seek(SeekFrom::Start(requested_offset))?;

        let started = Instant::now();
        let block = media_stream
            .read_block_timeout(cifs, read_bytes, timeout)
            .await?
            .unwrap_or_default();
        let elapsed = started.elapsed();

        if block.is_empty() {
            return Err(Error::InternalError(format!(
                "seek test read returned empty block at {label} offset {actual_offset}"
            )));
        }

        let stats = media_stream.stats();

        println!(
            "seek {label}: requested={} actual={} read={} in {:?} ({:.2} MiB/s) playback={} source={} buffered={}",
            requested_offset,
            actual_offset,
            block.len(),
            elapsed,
            mib_per_second(block.len(), elapsed),
            stats.playback_position,
            stats.source_position,
            stats.buffered
        );

        steps.push(SmokeSeekStep {
            label,
            requested_offset,
            actual_offset,
            read_len: block.len(),
            elapsed,
            playback_position_after: stats.playback_position,
            source_position_after: stats.source_position,
            buffered_after: stats.buffered,
        });
    }

    Ok(SmokeSeekMeasurements {
        file_size,
        read_bytes,
        steps,
    })
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

fn env_u32(name: &str, default: u32) -> u32 {
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

fn percentile_duration_copy(values: &[Duration], percentile: usize) -> Duration {
    if values.is_empty() {
        return Duration::ZERO;
    }

    let mut values = values.to_vec();
    percentile_duration(&mut values, percentile)
}

fn presentation_name(presentation: &MediaPresentation) -> &'static str {
    match presentation {
        MediaPresentation::Folder { .. } => "folder",
        MediaPresentation::MovieFolder { .. } => "movie-folder",
        MediaPresentation::PlayableFile { .. } => "playable-file",
    }
}
