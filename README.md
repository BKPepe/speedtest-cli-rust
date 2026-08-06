# librespeed-cli (Rust)

A command line interface for [LibreSpeed](https://github.com/librespeed/speedtest), written in Rust.

LibreSpeed ships two backend implementations — one in Go
([speedtest-go](https://github.com/librespeed/speedtest-go)) and one in Rust
([speedtest-rust](https://github.com/librespeed/speedtest-rust)) — but the CLI
existed only in Go. This is a port of
[librespeed/speedtest-cli](https://github.com/librespeed/speedtest-cli) to Rust,
addressing [issue #105](https://github.com/librespeed/speedtest-cli/issues/105).

It speaks the same protocol as the Go CLI and works against any LibreSpeed
backend, Go or Rust.

## Status

Feature-complete with the Go CLI: every command line flag is implemented,
including telemetry/`--share`, JSON and CSV reports, ICMP and HTTP ping,
IPv4/IPv6 forcing, source-address, interface and firewall-mark socket binding,
custom CA bundles, and the server list filters.

`--list`, `--version` and `--csv-header` output is byte-identical to the Go
implementation; `--json`, `--csv` and `--simple` produce the same field names,
ordering and number formatting.

## Building

```sh
cargo build --release
```

The binary lands in `target/release/librespeed-cli`. Set `SOURCE_DATE_EPOCH` for
a reproducible build date in `--version`.

## Usage

```
librespeed-cli [OPTIONS]
```

Run `librespeed-cli --help` for the full list. The common ones:

```sh
# Test against the automatically selected fastest server
librespeed-cli

# Machine-readable output
librespeed-cli --json
librespeed-cli --csv --csv-header

# Pick servers explicitly
librespeed-cli --list
librespeed-cli --server 51 --server 94

# Use your own backend
librespeed-cli --server-json https://example.com/servers.json
librespeed-cli --local-json ./servers.json
cat servers.json | librespeed-cli --local-json -

# Bind the test to a specific egress path
librespeed-cli --source 192.0.2.10
librespeed-cli --interface eth1
librespeed-cli --fwmark 42          # Linux only
```

## Differences from the Go implementation

The protocol and the flags match. These behaviours were deliberately changed,
all of them fixes rather than ports of the original quirk:

- **Results survive being piped.** The Go version drives its progress spinner —
  and the ping/download/upload result lines with it — on stdout via a spinner
  library that draws nothing when stdout is not a terminal, so
  `librespeed-cli > file` silently loses those lines. Here the spinner and its
  final message go to stderr, and the final message is always printed. stdout
  stays reserved for machine-readable data (`--json`, `--csv`, `--simple`,
  `--list`, `--version`), as the Go version's own `output` package intends.
- **Sub-millisecond ping precision.** The Go version truncates every latency
  sample to whole milliseconds (`time.Duration.Milliseconds()`), so a 0.4 ms LAN
  ping reports as `0.00 ms`. Latency is kept as a float here.
- **`--csv-delimiter` works.** The Go version assigns the delimiter to
  `gocsv.TagSeparator`, which controls struct tag parsing rather than the output
  delimiter, so the flag has no effect there.
- **Scheme-less server URLs.** A server entry such as `example.com/backend` with
  no scheme becomes `http://example.com/backend`. Go's `url.URL` produces the
  malformed `http:example.com/backend` for the same input.
- **HTTP/1.1 only.** The Go client negotiates HTTP/2 over TLS when the server
  offers it, which multiplexes every concurrent stream onto a single TCP
  connection and understates throughput. This client stays on HTTP/1.1 so
  `--concurrent` means concurrent connections.
- **`--interface` works on macOS** via `IP_BOUND_IF`; the Go version supports
  interface binding on Linux only. `--fwmark` remains Linux-only (`SO_MARK`),
  since there is no equivalent elsewhere.

One behaviour is stricter: TLS verification uses rustls, which rejects a
self-signed certificate that is presented as both the leaf and its own trust
anchor even when passed via `--ca-cert`. A normal private CA (a CA certificate
plus a server certificate it signed) works as expected; use
`--skip-cert-verify` for the degenerate case.

## Testing

```sh
cargo test          # unit tests
cargo clippy --all-targets
cargo fmt --check
```

## License

GNU Lesser General Public License v3.0, the same as the Go implementation. See
[LICENSE](LICENSE).

- LibreSpeed — Copyright (C) 2016-2020 Federico Dossena
- librespeed-cli — Copyright (C) 2020 Maddie Zhan
