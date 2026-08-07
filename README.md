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

### TLS backends

| Feature | Crypto | System dependency | Architectures |
| --- | --- | --- | --- |
| `rustls-tls` (default) | rustls + ring | none, statically linked | anything |
| `native-tls` | system OpenSSL | libopenssl | anything OpenSSL builds for |
| `vendored-openssl` | OpenSSL built from source | none, statically linked | anything OpenSSL builds for |

`ring` builds everywhere, falling back to portable C where it has no
hand-written assembly, which is everything outside x86, x86_64, aarch64 and
arm. On a core with no crypto instructions that costs throughput, so the
OpenSSL backend is the better choice there. Measured on the e500v2 in a
Turris 1.x, 8 KiB buffers:

| | ring | OpenSSL |
| --- | --- | --- |
| ChaCha20-Poly1305 | 48.6 MB/s | 46.9 MB/s |
| AES-128-GCM | 8.5 MB/s | 18.8 MB/s |

ChaCha20 in portable C already matches the assembly; AES does not, and AES is
what servers pick. An HTTPS run against a backend negotiating AES-256-GCM
measured 138 Mbps with OpenSSL against 37 Mbps with rustls. Build the OpenSSL
backend for such targets:

```sh
cargo build --release --no-default-features --features native-tls
```

### OpenWrt and Turris

The Go CLI cannot run on 32-bit PowerPC at all — Go's toolchain only targets
`ppc64` and `ppc64le`, which is why LibreSpeed's CLI is packaged for Turris
Omnia and MOX but not for Turris 1.x. Rust does reach that hardware through the
Tier 3 `powerpc-unknown-linux-muslspe` target that the OpenWrt build system
supports, so this port can go where the Go one cannot.

Build it against the OpenWrt SDK with the OpenSSL backend, for the throughput
reason above and because it links against the libopenssl already in the image
rather than carrying its own:

```sh
cargo build --release --no-default-features --features native-tls \
  --target powerpc-unknown-linux-muslspe
```

Being a Tier 3 target, `powerpc-unknown-linux-muslspe` has no prebuilt `std`, so
it needs a nightly toolchain with `-Z build-std` — which is what OpenWrt's Rust
packaging already arranges. Point the `openssl` crate at the SDK's OpenSSL via
`OPENSSL_DIR`, or let `pkg-config` find it through the SDK environment.

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
- **HTTP/1.1 by default, HTTP/2 behind `--http2`.** HTTP/2 carries every stream
  over one TCP connection, so `--concurrent` would stop meaning concurrent
  connections — and multiple connections is the standard way a speed test
  saturates a link. Go's client negotiates h2 whenever a server offers it.
  `--http2` enables it here too, with the flow-control windows raised to what
  Go's transport uses (4 MiB per stream, 1 GiB per connection); the protocol
  default of 64 KiB caps a stream at window/RTT, about 105 Mbps at 5 ms.
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
cargo test          # unit + end-to-end tests
cargo clippy --all-targets
cargo fmt --check
```

`tests/integration.rs` starts an in-process LibreSpeed backend and drives the
built binary against it, covering the whole flow — server list, ping, download,
upload, telemetry and every output mode — with no network access. The unit tests
cover the jitter estimator, Go-compatible path joining and rounding, CSV and
report rendering, server list filtering and URL scheme handling.

## License

GNU Lesser General Public License v3.0, the same as the Go implementation. See
[LICENSE](LICENSE).

- LibreSpeed — Copyright (C) 2016-2020 Federico Dossena
- librespeed-cli — Copyright (C) 2020 Maddie Zhan
