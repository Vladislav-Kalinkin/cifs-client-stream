# cifs-client-stream smoke test report

Copy this template, fill it in, and attach the full `smb_smoke` output.

## Tester

```text
Date:
Report label, optional:
```

## Client machine

```text
OS:
CPU:
RAM:
Rust version:
Wired Ethernet / Wi-Fi:
```

## SMB server

```text
Device / server:
SMB implementation, if known:
SMB version, if known:
Router/NAS model:
Firmware version:
Disk type: HDD / SSD / unknown
Disk connection: USB2 / USB3 / SATA / unknown
Filesystem, if known:
```

## Test type

```text
[ ] Root listing
[ ] Nested listing
[ ] Movie-folder scan
[ ] Sequential read 256 blocks
[ ] Sequential read 2048 blocks
[ ] Pipeline comparison
[ ] Non-AirPort Large ReadX experiment
```

## Command used

Paste the exact command, but remove or replace the real password.

```sh
SMB_HOST='...' \
SMB_SHARE='...' \
SMB_USER='...' \
SMB_PASSWORD='REDACTED' \
...
cargo run --bin smb_smoke
```

## File tested

```text
Path:
Container: mkv / mp4 / mov / other
Approx size:
Video type, if known:
Non-English path characters: yes / no
```

## Summary results

```text
configured_chunk_size:
effective_chunk_size:
pipeline_depth:
initial buffer speed:
read phase MiB/s:
total MiB/s:
refill blocks:
cached blocks:
block latency p95:
block latency p99:
slowest block:
internal read calls:
internal avg_size:
errors/timeouts:
```

## Full output

```text
paste full smb_smoke output here
```

## Notes

```text
Was the disk asleep before the test?
Did the first run differ from the second?
Any disconnects?
Any strange filenames?
Any folder that was misclassified?
Any playback-relevant concern?
```
