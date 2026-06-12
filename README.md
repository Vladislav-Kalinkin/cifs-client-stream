# A minimal, Rust-native CIFS client library

This project was born from the need at [RE:](https://www.r-ecosystem.de/) to connect to SMBv1 shares.

As such, the implementation herein is not planned to become a fully-fledged SMB & CIFS implementation but driven by our very specific needs. Nonetheless we're open to contributions and hope that this library might help others with similar needs.

## Features

- connect to SMBv1 servers
- authenticate via NTLM using domain, username & password
- download files

## Smoke test

Run a real SMB1 read-only smoke test from macOS without tvOS or Xcode:

```sh
SMB_URI='smb://user:password@router/share/Movies' \
SMB_HOST='192.168.1.1' \
SMB_READ_PATH='Movies/Sample.mkv' \
cargo run --bin smb_smoke
```

`SMB_URI` is required. `SMB_HOST` is optional and overrides the host from `SMB_URI` when DNS lookup does not work. `SMB_READ_PATH` is optional and reads the first block of a file through the streaming path. Optional tuning:

```sh
SMB_TIMEOUT_MS=5000
SMB_READ_BYTES=262144
SMB_READ_BLOCKS=4
```

## Contributing

If you find that there's some feature not covered by this implementation, or you happen to find a bug, we'll welcome pull requests with your improvements.

In case you want to get in touch and discuss some specific aspects, feel free to use the [discussions feature at Github](https://github.com/re-gmbh/cifs/discussions/).
