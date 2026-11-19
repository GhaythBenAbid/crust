# Working on crust

crust is a curl-compatible HTTP client in Rust whose main selling point is
speed: a small curl call is mostly process startup, and crust keeps that cost
low. Every change must keep curl compatibility **and** startup speed.

## Layout

| Path | What it is |
|---|---|
| `src/main.rs` | Entry point: parse args, run, exit with curl's exit code |
| `src/args.rs` | Hand-rolled curl-style argument parser (`-sSL`, `-XPOST`, `-o file`). Add new flags here: `short_to_long`, `takes_value`, the `match` in `parse`, and the help text |
| `src/url.rs` | URL parsing, redirect resolution (`join`), `-O` file names |
| `src/glob.rs` | URL globbing (`[1-100]`, `{a,b}`) |
| `src/http.rs` | Connections and pool, request building, response parsing, body streaming, redirects, `--write-out` |
| `src/tls.rs` | rustls client config (ring provider, webpki roots, `-k`) |
| `src/util.rs` | `Error` type and curl exit-code constants |
| `testserver/` | Go test server: HTTP `:8080`, HTTPS `:8443`, forward proxy `:8081`; endpoints are listed at the top of `main.go` |
| `tests/compare.sh` | Differential tests: each case runs through curl and crust and compares stdout, written files and exit code |
| `bench/run.sh` | hyperfine benchmarks against curl |
| `docs/` | Project website (GitHub Pages); `docs/data.js` holds benchmark and progress numbers |

## Build and test

```sh
cargo build --release
cargo test --release                 # unit tests
tests/compare.sh                     # all differential cases (needs Go and curl)
tests/compare.sh cookie              # only cases whose name contains "cookie"
```

To benchmark (needs `hyperfine`):

```sh
(cd testserver && go run .) &
bench/run.sh /usr/bin/curl target/release/crust
```

## Rules

1. **curl is the spec.** When unsure how something should behave, run curl
   against the test server and match it: output, files written, exit code.
   Error messages use the `crust: (CODE) message` form.
2. **Startup is the product.** No async runtime, no new dependency that does
   work at startup, nothing initialized unless the request needs it (TLS is
   built lazily; keep it that way). If a change touches the hot path, run the
   `startup`, `http_small` and `https_small_h1` benchmarks before and after and
   put the numbers in the PR.
3. **Add tests with the feature.** New behaviour gets a `case_` line in
   `tests/compare.sh` (add a test-server endpoint if needed) and unit tests for
   pure logic (parsers, encoders).
4. **No `unwrap()` on user or network input.** Return `Error::new(code, msg)`
   with the curl exit code from `util.rs`.
5. Keep `cargo build` free of warnings.

## Definition of done

- [ ] `cargo build --release` has no warnings and `cargo test --release` passes
- [ ] The issue's `tests/compare.sh` cases pass, and the full suite has no new failures
- [ ] Benchmarks show no regression when the hot path was touched
- [ ] README flag list updated if flags were added
- [ ] `docs/data.js` roadmap and test count updated if a roadmap item moved
- [ ] PR description says `Closes #N` and lists anything left undone

## Avoiding conflicts

`src/http.rs` is shared by most features. Keep changes focused: put
self-contained logic in a new module (`src/cookies.rs`, `src/proxy.rs`,
`src/decode.rs`, `src/multipart.rs`, …) and keep the edits to `http.rs` small.
Rebase on `main` before opening a PR.
