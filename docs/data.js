// Site data. Update these numbers when benchmarks or progress change;
// the page renders everything from this object.
window.CRUST = {
  version: "0.1.0",
  updated: "2026-11-19",
  machine: "Apple M1 Pro, macOS, loopback test server, hyperfine (process launch included)",

  // Fixed order + fixed color per tool (color follows the entity).
  tools: [
    { id: "crust", name: "crust 0.1", detail: "Rust · rustls + ring" },
    { id: "macos", name: "macOS curl 8.7.1", detail: "LibreSSL" },
    { id: "brew", name: "Homebrew curl 8.18", detail: "OpenSSL 3.6" },
  ],

  // Mean wall time (ms) and peak memory (MiB). null = not supported yet.
  benchmarks: [
    { id: "startup", name: "Startup", desc: "crust --version: launch and exit", time: { crust: 3.0, macos: 8.6, brew: 34.9 }, mem: { crust: 1.9, macos: 2.6, brew: 12.8 } },
    { id: "http", name: "Small HTTP GET", desc: "13-byte response, one request", time: { crust: 3.6, macos: 9.8, brew: 19.2 }, mem: { crust: 2.0, macos: 2.8, brew: 11.3 } },
    { id: "https", name: "Small HTTPS GET", desc: "TLS 1.3 handshake + 13-byte response", time: { crust: 3.9, macos: 12.1, brew: 20.8 }, mem: { crust: 2.7, macos: 2.9, brew: 13.1 } },
    { id: "reuse", name: "100 requests, one connection", desc: "URL globbing with keep-alive", time: { crust: 8.6, macos: 19.6, brew: 30.0 }, mem: { crust: 2.3, macos: 3.0, brew: 11.5 } },
    { id: "https1g", name: "1 GiB download over HTTPS", desc: "Throughput, bound by the TLS library", time: { crust: 761, macos: 4545, brew: 798 }, mem: { crust: 3.0, macos: 3.2, brew: 13.3 } },
    { id: "http1g", name: "1 GiB download over HTTP", desc: "Throughput, plain TCP", time: { crust: 218.5, macos: 182.3, brew: 224.6 }, mem: { crust: 2.3, macos: 2.9, brew: 11.4 } },
  ],

  // Where a small HTTP GET spends its time (ms): process launch vs the request itself
  // (request time from curl's own -w timers, ~0.5 ms).
  anatomy: [
    { id: "macos", launch: 9.3, request: 0.5 },
    { id: "brew", launch: 18.6, request: 0.6 },
    { id: "crust", launch: 3.1, request: 0.5 },
  ],

  // Differential test suite: same command through curl and crust, outputs compared.
  tests: { passing: 81, total: 125 },

  roadmap: [
    { status: "done", title: "HTTP/1.1 over HTTP and HTTPS", note: "rustls + ring, verified with Mozilla roots" },
    { status: "done", title: "curl-style flags and exit codes", note: "-X -H -d -o -O -L -i -I -v -s -S -f -k -A -e -u -w -m" },
    { status: "done", title: "URL globbing and connection reuse", note: "[1-100], {a,b}, keep-alive pool" },
    { status: "done", title: "Benchmark suite", note: "hyperfine against both curls" },
    { status: "done", title: "Differential test harness", note: "125 cases comparing crust with curl" },
    { status: "progress", title: "Close compatibility gaps", note: "--max-time 0, URL encoding" },
    { status: "planned", title: "Proxies", note: "-x, http_proxy / https_proxy / no_proxy, CONNECT" },
    { status: "planned", title: "Keychain certificates and --cacert", note: "trust what macOS trusts" },
    { status: "planned", title: "Happy Eyeballs", note: "fast IPv6 → IPv4 fallback" },
    { status: "planned", title: "--compressed, cookies, -F, -T, --json", note: "the everyday power flags" },
    { status: "planned", title: "--retry and progress meter", note: "" },
    { status: "planned", title: "HTTP/2 and parallel transfers (-Z)", note: "" },
  ],
};
