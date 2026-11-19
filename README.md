# crust

A fast, curl-compatible HTTP client written in Rust.

```sh
cargo build --release
./target/release/crust -sL https://example.com
```

## Why

For everyday requests, most of `curl`'s time goes to process startup and
dynamic library loading, not networking. crust is a single static binary with
no async runtime and lazily initialized TLS (rustls + ring).

Measured on an M1 Pro against a local server (mean wall time incl. process launch):

| Workload | macOS curl 8.7.1 | Homebrew curl 8.18 | crust 0.1 |
|---|---|---|---|
| Startup | 8.6 ms | 34.9 ms | **3.0 ms** |
| Small HTTP GET | 9.8 ms | 19.2 ms | **3.6 ms** |
| Small HTTPS GET | 12.1 ms | 20.8 ms | **3.9 ms** |
| 100 GETs, one connection | 19.6 ms | 30.0 ms | **8.6 ms** |
| 1 GiB over HTTPS | 4.55 s | 0.80 s | **0.76 s** |
| 1 GiB over HTTP | **182 ms** | 225 ms | 219 ms |

## Status

Early (0.1). HTTP/1.1 over HTTP and HTTPS. Supported flags so far:
`-X -H -d --data-binary --data-raw -G -o -O -L --max-redirs -i -I -v -s -S -f -k
-A -e -u -w -m --connect-timeout -g`, URL globbing (`[1-100]`, `{a,b}`) and
connection reuse across URLs. Not yet: HTTP/2, `-Z`, proxies, cookies, `-F`,
`-T`, `--compressed`, Keychain certificates. Work in progress.

## Releases

Prebuilt binaries for macOS (Apple Silicon, Intel) and Linux (x86_64, ARM64)
are attached to every [GitHub Release](https://github.com/GhaythBenAbid/crust/releases).

To cut a release: bump `version` in `Cargo.toml`, commit, then tag and push:

```sh
git tag v0.1.1 && git push origin v0.1.1
```

The release workflow checks the tag matches `Cargo.toml`, runs the tests,
builds all targets and publishes the release with checksums and notes.

## Development

```sh
cargo test                      # unit tests
tests/compare.sh                # differential tests vs curl (needs Go)
(cd testserver && go run .) &   # local test/bench server
bench/run.sh /usr/bin/curl target/release/crust   # benchmarks (needs hyperfine)
```
