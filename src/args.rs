//! curl-compatible command-line parsing (hand-rolled: curl's syntax allows
//! clustered short flags like `-sSL` and attached values like `-XPOST`).

use crate::util::{Error, Result, E_READ_ERROR, E_USAGE};
use std::borrow::Cow;
use std::io::Read;
use std::time::Duration;

pub enum OutSpec {
    Stdout,
    File(String),
    /// `-O`: name the file after the last path segment of the URL.
    Remote,
}

pub struct Target {
    pub url: String,
    pub out: OutSpec,
}

pub struct Opts {
    pub targets: Vec<Target>,
    pub method: Option<String>,
    pub headers: Vec<String>,
    pub data: Option<Vec<u8>>,
    pub get: bool,
    pub follow: bool,
    pub max_redirs: u32,
    pub include: bool,
    pub head: bool,
    pub verbose: bool,
    pub silent: bool,
    pub show_error: bool,
    pub fail: bool,
    pub insecure: bool,
    pub globoff: bool,
    pub user_agent: Option<String>,
    pub user: Option<String>,
    pub referer: Option<String>,
    pub write_out: Option<String>,
    pub max_time: Option<Duration>,
    pub connect_timeout: Option<Duration>,
}

pub enum Parsed {
    Run(Opts),
    /// `--help` / `--version` already printed.
    Exit,
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn short_to_long(c: char) -> Option<&'static str> {
    Some(match c {
        'X' => "request",
        'H' => "header",
        'd' => "data",
        'o' => "output",
        'O' => "remote-name",
        'L' => "location",
        'i' => "include",
        'I' => "head",
        'v' => "verbose",
        's' => "silent",
        'S' => "show-error",
        'f' => "fail",
        'k' => "insecure",
        'A' => "user-agent",
        'w' => "write-out",
        'u' => "user",
        'e' => "referer",
        'm' => "max-time",
        'G' => "get",
        'g' => "globoff",
        'V' => "version",
        'h' => "help",
        _ => return None,
    })
}

fn takes_value(long: &str) -> bool {
    matches!(
        long,
        "request"
            | "header"
            | "data"
            | "data-ascii"
            | "data-binary"
            | "data-raw"
            | "data-urlencode"
            | "json"
            | "output"
            | "user-agent"
            | "write-out"
            | "user"
            | "referer"
            | "max-time"
            | "connect-timeout"
            | "max-redirs"
            | "url"
    )
}

fn usage(msg: impl Into<String>) -> Error {
    Error::new(E_USAGE, msg)
}

fn read_source(spec: &str) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let r = if spec == "-" {
        std::io::stdin().read_to_end(&mut buf).map(|_| ())
    } else {
        std::fs::File::open(spec).and_then(|mut f| f.read_to_end(&mut buf)).map(|_| ())
    };
    r.map_err(|e| Error::new(E_READ_ERROR, format!("Failed to open/read local data from file/application ({spec}): {e}")))?;
    Ok(buf)
}

fn urlencode_data(arg: &str) -> Result<Vec<u8>> {
    let (name, data) = if let Some((name, content)) = arg.split_once('=') {
        (name, Cow::Borrowed(content.as_bytes()))
    } else if let Some((name, path)) = arg.split_once('@') {
        (name, Cow::Owned(read_source(path)?))
    } else {
        ("", Cow::Borrowed(arg.as_bytes()))
    };
    let mut encoded = Vec::with_capacity(name.len() + data.len());
    if !name.is_empty() {
        encoded.extend_from_slice(name.as_bytes());
        encoded.push(b'=');
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in data.iter() {
        match byte {
            b' ' => encoded.push(b'+'),
            b'-' | b'.' | b'_' | b'~' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' => encoded.push(byte),
            _ => encoded.extend_from_slice(&[b'%', HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]]),
        }
    }
    Ok(encoded)
}

fn parse_secs(name: &str, v: &str) -> Result<Option<Duration>> {
    let invalid = || usage(format!("option --{name}: expected a proper numerical parameter"));
    let seconds = v.parse::<f64>().map_err(|_| invalid())?;
    if seconds == 0.0 {
        return Ok(None);
    }
    Duration::try_from_secs_f64(seconds).map(Some).map_err(|_| invalid())
}

pub fn parse(argv: Vec<String>) -> Result<Parsed> {
    let mut o = Opts {
        targets: Vec::new(),
        method: None,
        headers: Vec::new(),
        data: None,
        get: false,
        follow: false,
        max_redirs: 50,
        include: false,
        head: false,
        verbose: false,
        silent: false,
        show_error: false,
        fail: false,
        insecure: false,
        globoff: false,
        user_agent: None,
        user: None,
        referer: None,
        write_out: None,
        max_time: None,
        connect_timeout: None,
    };
    let mut urls: Vec<String> = Vec::new();
    let mut outs: Vec<OutSpec> = Vec::new();
    let mut json = false;

    let mut it = argv.into_iter();
    while let Some(arg) = it.next() {
        // Collect (long-name, inline-value) pairs for this argv element.
        let mut flags: Vec<(String, Option<String>)> = Vec::new();
        if let Some(long) = arg.strip_prefix("--").filter(|s| !s.is_empty()) {
            flags.push((long.to_string(), None));
        } else if let Some(cluster) = arg.strip_prefix('-').filter(|s| !s.is_empty()) {
            for (i, c) in cluster.char_indices() {
                let long = short_to_long(c).ok_or_else(|| usage(format!("option -{c}: is unknown")))?;
                if takes_value(long) {
                    let rest = &cluster[i + c.len_utf8()..];
                    flags.push((long.to_string(), (!rest.is_empty()).then(|| rest.to_string())));
                    break;
                }
                flags.push((long.to_string(), None));
            }
        } else {
            urls.push(arg);
            continue;
        }

        for (name, inline) in flags {
            let val = if takes_value(&name) {
                Some(match inline {
                    Some(v) => v,
                    None => it.next().ok_or_else(|| usage(format!("option --{name}: requires parameter")))?,
                })
            } else {
                None
            };
            let v = val.unwrap_or_default();
            match name.as_str() {
                "request" => o.method = Some(v),
                "header" => {
                    if let Some(path) = v.strip_prefix('@') {
                        let bytes = read_source(path)?;
                        o.headers.extend(String::from_utf8_lossy(&bytes).lines().map(str::to_owned));
                    } else {
                        o.headers.push(v);
                    }
                }
                "data" | "data-ascii" | "data-binary" | "data-raw" | "data-urlencode" | "json" => {
                    let chunk = match (name.as_str(), v.strip_prefix('@')) {
                        ("data-urlencode", _) => urlencode_data(&v)?,
                        ("data-raw", _) | (_, None) => v.into_bytes(),
                        (flag, Some(path)) => {
                            let mut b = read_source(path)?;
                            if !matches!(flag, "data-binary" | "json") {
                                b.retain(|&c| c != b'\r' && c != b'\n');
                            }
                            b
                        }
                    };
                    let d = o.data.get_or_insert_with(Vec::new);
                    if name == "json" {
                        json = true;
                    } else if !d.is_empty() {
                        d.push(b'&');
                    }
                    d.extend_from_slice(&chunk);
                }
                "output" => outs.push(OutSpec::File(v)),
                "remote-name" => outs.push(OutSpec::Remote),
                "location" => o.follow = true,
                "max-redirs" => {
                    o.max_redirs = v.parse().map_err(|_| usage("option --max-redirs: expected a proper numerical parameter"))?
                }
                "include" => o.include = true,
                "head" => o.head = true,
                "verbose" => o.verbose = true,
                "silent" => o.silent = true,
                "show-error" => o.show_error = true,
                "fail" => o.fail = true,
                "insecure" => o.insecure = true,
                "globoff" => o.globoff = true,
                "get" => o.get = true,
                "user-agent" => o.user_agent = Some(v),
                "user" => o.user = Some(v),
                "referer" => o.referer = Some(v),
                "write-out" => {
                    o.write_out = Some(match v.strip_prefix('@') {
                        Some(path) => String::from_utf8_lossy(&read_source(path)?).into_owned(),
                        None => v,
                    })
                }
                "max-time" => o.max_time = parse_secs(&name, &v)?,
                "connect-timeout" => o.connect_timeout = parse_secs(&name, &v)?,
                "url" => urls.push(v),
                "http1.1" | "no-progress-meter" => {}
                "http2" | "http2-prior-knowledge" | "http3" => {
                    return Err(usage(format!("option --{name}: not supported yet (crust speaks HTTP/1.1)")));
                }
                "version" => {
                    println!(
                        "crust {VERSION} ({}-{}) rustls/0.23 (ring)\nProtocols: http https\nFeatures: IPv6 Largefile SSL",
                        std::env::consts::ARCH,
                        std::env::consts::OS
                    );
                    return Ok(Parsed::Exit);
                }
                "help" => {
                    print!("{HELP}");
                    return Ok(Parsed::Exit);
                }
                _ => return Err(usage(format!("option --{name}: is unknown"))),
            }
        }
    }

    if urls.is_empty() {
        return Err(usage("no URL specified"));
    }
    if json {
        o.headers.splice(0..0, ["Content-Type: application/json".into(), "Accept: application/json".into()]);
    }
    let mut outs = outs.into_iter();
    o.targets = urls
        .into_iter()
        .map(|url| Target { url, out: outs.next().unwrap_or(OutSpec::Stdout) })
        .collect();
    Ok(Parsed::Run(o))
}

const HELP: &str = "\
Usage: crust [options...] <url>...
 -A, --user-agent <name>     Send User-Agent <name>
 -d, --data <data>           HTTP POST data (@file reads a file, - for stdin)
     --data-binary <data>    HTTP POST data, @file sent as-is
     --data-raw <data>       HTTP POST data, '@' not special
     --data-urlencode <data> HTTP POST data with form URL encoding
 -e, --referer <url>         Referer URL
 -f, --fail                  Fail silently (exit 22) on HTTP errors
 -G, --get                   Put the post data in the URL and use GET
 -g, --globoff               Disable URL globbing ([1-10], {a,b})
 -H, --header <header>       Pass a custom header or read headers from @file
 -h, --help                  Show this help
 -I, --head                  Show document info only
 -i, --include               Include response headers in output
     --json <data>           Send JSON data or read it from @file
 -k, --insecure              Skip TLS certificate verification
 -L, --location              Follow redirects
     --max-redirs <num>      Maximum number of redirects allowed (default 50)
 -m, --max-time <secs>       Maximum time allowed for the transfer
     --connect-timeout <s>   Maximum time allowed for connection
 -o, --output <file>         Write to file instead of stdout
 -O, --remote-name           Write output to a file named as the remote file
 -s, --silent                Silent mode
 -S, --show-error            Show error even when -s is used
 -u, --user <user:password>  Server user and password (Basic auth)
     --url <url>             URL to work with
 -v, --verbose               Make the operation more talkative
 -V, --version               Show version number and quit
 -w, --write-out <format>    Output format after completion, e.g. '%{http_code}\\n'
";

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Opts {
        match parse(args.iter().map(|s| s.to_string()).collect()) {
            Ok(Parsed::Run(o)) => o,
            _ => panic!("parse failed"),
        }
    }

    #[test]
    fn clusters_and_attached_values() {
        let o = p(&["-sSLk", "-XPOST", "-H", "A: b", "-dfoo", "-d", "bar", "x.com"]);
        assert!(o.silent && o.show_error && o.follow && o.insecure);
        assert_eq!(o.method.as_deref(), Some("POST"));
        assert_eq!(o.headers, vec!["A: b"]);
        assert_eq!(o.data.as_deref(), Some(&b"foo&bar"[..]));
        assert_eq!(o.targets[0].url, "x.com");
    }

    #[test]
    fn value_flag_ends_cluster() {
        let o = p(&["-so", "/dev/null", "a", "b"]);
        assert!(o.silent);
        assert!(matches!(&o.targets[0].out, OutSpec::File(f) if f == "/dev/null"));
        assert!(matches!(o.targets[1].out, OutSpec::Stdout));
    }

    #[test]
    fn zero_timeouts_clear_previous_values() {
        let o = p(&["-m", "0.25", "--connect-timeout", "0.5", "x.com"]);
        assert_eq!(o.max_time, Some(Duration::from_millis(250)));
        assert_eq!(o.connect_timeout, Some(Duration::from_millis(500)));

        let o = p(&["-m", "0.25", "-m", "0", "--connect-timeout", "0.5", "--connect-timeout", "0.0", "x.com"]);
        assert_eq!(o.max_time, None);
        assert_eq!(o.connect_timeout, None);
    }

    #[test]
    fn encodes_data_values_and_preserves_names() {
        let o = p(&["--data-urlencode", "a%20b=x y&é", "--data-urlencode", "=x/y", "x.com"]);
        assert_eq!(o.data.as_deref(), Some(&b"a%20b=x+y%26%C3%A9&x%2Fy"[..]));
    }

    #[test]
    fn json_appends_without_a_separator() {
        let o = p(&["-d", "a", "--json", "b", "--json", "c", "-d", "d", "x.com"]);
        assert_eq!(o.data.as_deref(), Some(&b"abc&d"[..]));
        assert_eq!(o.headers, ["Content-Type: application/json", "Accept: application/json"]);
    }

    #[test]
    fn errors() {
        assert!(parse(vec!["-Z".into(), "x".into()]).is_err());
        assert!(parse(vec!["-s".into()]).is_err());
        assert!(parse(vec!["x".into(), "-H".into()]).is_err());
    }
}
