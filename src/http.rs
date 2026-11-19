//! HTTP/1.1 transfers: connection pool, request writing, response parsing,
//! body streaming, redirects and `--write-out`.

use crate::args::{Opts, OutSpec};
use crate::url::Url;
use crate::util::*;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::mem::ManuallyDrop;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::os::fd::FromRawFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Per-connection read buffer. Large reads mean fewer syscalls on big downloads.
const BUF_SIZE: usize = 256 * 1024;

/// Sites like wttr.in or ifconfig.me serve plain text only to UAs starting with
/// "curl", so we stay compatible by default.
const DEFAULT_UA: &str = concat!("curl/8 (crust/", env!("CARGO_PKG_VERSION"), ")");

// ---------------------------------------------------------------- streams

enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(s) => &s.sock,
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

struct Conn {
    stream: Stream,
    buf: Box<[u8]>,
    pos: usize,
    len: usize,
    tls: bool,
    host: String,
    port: u16,
    id: u32,
    reused: bool,
}

impl Conn {
    fn avail(&self) -> &[u8] {
        &self.buf[self.pos..self.len]
    }

    /// Read more bytes into the buffer. Returns 0 on EOF.
    fn fill(&mut self, deadline: Option<Instant>) -> Result<usize> {
        if self.pos == self.len {
            self.pos = 0;
            self.len = 0;
        } else if self.len == self.buf.len() {
            if self.pos == 0 {
                return Err(Error::new(E_RECV_ERROR, "response header too large"));
            }
            self.buf.copy_within(self.pos..self.len, 0);
            self.len -= self.pos;
            self.pos = 0;
        }
        if let Some(d) = deadline {
            let left = d.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(timed_out());
            }
            let _ = self.stream.tcp().set_read_timeout(Some(left));
        }
        loop {
            match self.stream.read(&mut self.buf[self.len..]) {
                Ok(n) => {
                    self.len += n;
                    return Ok(n);
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                // TLS peer closed without close_notify: treat as EOF, framing decides.
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(0),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Err(timed_out()),
                Err(e) => return Err(Error::new(E_RECV_ERROR, format!("Recv failure: {e}"))),
            }
        }
    }
}

fn timed_out() -> Error {
    Error::new(E_OPERATION_TIMEDOUT, "Operation timed out")
}

// ---------------------------------------------------------------- output

fn stdout_raw() -> ManuallyDrop<File> {
    // Unbuffered fd 1: std's Stdout is line-buffered, which is slow for bulk data.
    ManuallyDrop::new(unsafe { File::from_raw_fd(1) })
}

enum Sink {
    Stdout(ManuallyDrop<File>),
    /// Created on first write or successful completion.
    Pending(String),
    File(File),
}

impl Sink {
    fn new(out: &OutSpec, url: &Url) -> Result<Sink> {
        Ok(match out {
            OutSpec::Stdout => Sink::Stdout(stdout_raw()),
            OutSpec::File(p) if p == "-" => Sink::Stdout(stdout_raw()),
            OutSpec::File(p) => Sink::Pending(p.clone()),
            OutSpec::Remote => match url.file_name() {
                "" => return Err(Error::new(E_WRITE_ERROR, "Remote file name has no length")),
                n => Sink::Pending(n.to_string()),
            },
        })
    }

    fn open_pending(&mut self) -> Result<()> {
        if let Sink::Pending(p) = self {
            let f = File::create(&*p).map_err(|e| Error::new(E_WRITE_ERROR, format!("Failed to open the file {p}: {e}")))?;
            *self = Sink::File(f);
        }
        Ok(())
    }

    fn write(&mut self, b: &[u8]) -> Result<()> {
        self.open_pending()?;
        let r = match self {
            Sink::Stdout(f) => f.write_all(b),
            Sink::File(f) => f.write_all(b),
            Sink::Pending(_) => unreachable!(),
        };
        r.map_err(|_| Error::new(E_WRITE_ERROR, "Failure writing output to destination"))
    }
}

// ---------------------------------------------------------------- transfer info

struct Info {
    start: Instant,
    t_namelookup: Duration,
    t_connect: Duration,
    t_appconnect: Duration,
    t_pretransfer: Duration,
    t_starttransfer: Duration,
    t_total: Duration,
    code: u16,
    version: u8,
    size_download: u64,
    size_header: u64,
    size_upload: u64,
    url: String,
    url_effective: String,
    num_redirects: u32,
    content_type: String,
    remote_ip: String,
    remote_port: u16,
    method: String,
    scheme: &'static str,
    exitcode: u8,
    errormsg: String,
}

impl Info {
    fn new(url: &str) -> Info {
        Info {
            start: Instant::now(),
            t_namelookup: Duration::ZERO,
            t_connect: Duration::ZERO,
            t_appconnect: Duration::ZERO,
            t_pretransfer: Duration::ZERO,
            t_starttransfer: Duration::ZERO,
            t_total: Duration::ZERO,
            code: 0,
            version: 0,
            size_download: 0,
            size_header: 0,
            size_upload: 0,
            url: url.to_string(),
            url_effective: url.to_string(),
            num_redirects: 0,
            content_type: String::new(),
            remote_ip: String::new(),
            remote_port: 0,
            method: String::new(),
            scheme: "",
            exitcode: 0,
            errormsg: String::new(),
        }
    }
}

// ---------------------------------------------------------------- response

struct Resp {
    version: u8,
    code: u16,
    headers: Vec<(String, String)>,
    raw: Vec<u8>,
}

impl Resp {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn keep_alive(&self) -> bool {
        let c = self.header("connection").unwrap_or("").to_ascii_lowercase();
        if self.version >= 1 { !c.contains("close") } else { c.contains("keep-alive") }
    }
}

enum Framing {
    Empty,
    Length(u64),
    Chunked,
    Eof,
}

fn framing(method: &str, r: &Resp) -> Result<Framing> {
    if method == "HEAD" || r.code == 204 || r.code == 304 {
        return Ok(Framing::Empty);
    }
    if r.header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        return Ok(Framing::Chunked);
    }
    if let Some(cl) = r.header("content-length") {
        return cl
            .trim()
            .parse()
            .map(Framing::Length)
            .map_err(|_| Error::new(E_WEIRD_SERVER_REPLY, "Invalid Content-Length response header"));
    }
    Ok(Framing::Eof)
}

fn read_head(conn: &mut Conn, deadline: Option<Instant>) -> Result<Resp> {
    let mut got_any = false;
    loop {
        let parsed = {
            let mut hs = [httparse::EMPTY_HEADER; 128];
            let mut r = httparse::Response::new(&mut hs);
            match r.parse(conn.avail()) {
                Ok(httparse::Status::Complete(n)) => Some((
                    n,
                    Resp {
                        version: r.version.unwrap_or(1),
                        code: r.code.unwrap_or(0),
                        headers: r
                            .headers
                            .iter()
                            .map(|h| (h.name.to_string(), String::from_utf8_lossy(h.value).into_owned()))
                            .collect(),
                        raw: conn.avail()[..n].to_vec(),
                    },
                )),
                Ok(httparse::Status::Partial) => None,
                Err(e) => return Err(Error::new(E_WEIRD_SERVER_REPLY, format!("Weird server reply: {e}"))),
            }
        };
        match parsed {
            Some((n, resp)) => {
                conn.pos += n;
                if (100..200).contains(&resp.code) && resp.code != 101 {
                    continue; // skip 100 Continue & co.
                }
                return Ok(resp);
            }
            None => {
                if conn.fill(deadline)? == 0 {
                    return Err(if got_any {
                        Error::new(E_RECV_ERROR, "Connection closed in the middle of the response headers")
                    } else {
                        Error::new(E_GOT_NOTHING, "Empty reply from server")
                    });
                }
                got_any = true;
            }
        }
    }
}

fn read_line(conn: &mut Conn, deadline: Option<Instant>) -> Result<Vec<u8>> {
    loop {
        if let Some(i) = conn.avail().iter().position(|&b| b == b'\n') {
            let mut line = conn.avail()[..i].to_vec();
            conn.pos += i + 1;
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(line);
        }
        if conn.fill(deadline)? == 0 {
            return Err(Error::new(E_RECV_ERROR, "Connection closed in chunked body"));
        }
    }
}

fn copy_exact(
    conn: &mut Conn,
    mut left: u64,
    sink: &mut Option<&mut Sink>,
    deadline: Option<Instant>,
    count: &mut u64,
) -> Result<()> {
    while left > 0 {
        if conn.pos == conn.len && conn.fill(deadline)? == 0 {
            return Err(Error::new(E_PARTIAL_FILE, format!("transfer closed with {left} bytes remaining to read")));
        }
        let n = ((conn.len - conn.pos) as u64).min(left) as usize;
        if let Some(s) = sink.as_mut() {
            s.write(&conn.buf[conn.pos..conn.pos + n])?;
        }
        conn.pos += n;
        left -= n as u64;
        *count += n as u64;
    }
    Ok(())
}

/// Stream the body to `sink` (or discard it). Returns whether the connection
/// ended at a clean message boundary (i.e. may be reused).
fn read_body(
    conn: &mut Conn,
    f: &Framing,
    mut sink: Option<&mut Sink>,
    deadline: Option<Instant>,
    count: &mut u64,
) -> Result<bool> {
    match *f {
        Framing::Empty => Ok(true),
        Framing::Length(n) => copy_exact(conn, n, &mut sink, deadline, count).map(|_| true),
        Framing::Chunked => loop {
            let line = read_line(conn, deadline)?;
            let hex = line.split(|&b| b == b';').next().unwrap_or(&[]);
            let size = std::str::from_utf8(hex)
                .ok()
                .and_then(|s| u64::from_str_radix(s.trim(), 16).ok())
                .ok_or_else(|| Error::new(E_RECV_ERROR, "Illegal or missing hexadecimal sequence in chunked-encoding"))?;
            if size == 0 {
                while !read_line(conn, deadline)?.is_empty() {} // trailers
                return Ok(true);
            }
            copy_exact(conn, size, &mut sink, deadline, count)?;
            if !read_line(conn, deadline)?.is_empty() {
                return Err(Error::new(E_RECV_ERROR, "Malformed chunked-encoding"));
            }
        },
        Framing::Eof => loop {
            if conn.pos == conn.len && conn.fill(deadline)? == 0 {
                return Ok(false);
            }
            if let Some(s) = sink.as_mut() {
                s.write(&conn.buf[conn.pos..conn.len])?;
            }
            *count += (conn.len - conn.pos) as u64;
            conn.pos = conn.len;
        },
    }
}

fn verbose_lines(prefix: char, raw: &[u8]) {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::new();
    for line in text.split("\r\n") {
        out.push(prefix);
        if !line.is_empty() {
            out.push(' ');
            out.push_str(line);
        }
        out.push('\n');
    }
    if out.ends_with(&format!("{prefix}\n{prefix}\n")) {
        out.truncate(out.len() - 2); // headers end with a double CRLF
    }
    let _ = io::stderr().write_all(out.as_bytes());
}

fn tls_error(e: io::Error) -> Error {
    if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) {
        return Error::new(E_OPERATION_TIMEDOUT, "TLS handshake timed out");
    }
    match e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) {
        Some(re @ rustls::Error::InvalidCertificate(_)) => {
            Error::new(E_PEER_FAILED_VERIFICATION, format!("SSL certificate problem: {re}"))
        }
        Some(re) => Error::new(E_SSL_CONNECT_ERROR, format!("TLS connect error: {re}")),
        None => Error::new(E_SSL_CONNECT_ERROR, format!("TLS connect error: {e}")),
    }
}

// ---------------------------------------------------------------- client

struct Client<'a> {
    o: &'a Opts,
    tls: Option<Arc<ClientConfig>>,
    pool: Vec<Conn>,
    next_id: u32,
}

pub fn run(o: &Opts) -> u8 {
    let mut c = Client { o, tls: None, pool: Vec::new(), next_id: 0 };
    let mut rc = 0;
    for t in &o.targets {
        let urls = if o.globoff { Ok(vec![t.url.clone()]) } else { crate::glob::expand(&t.url) };
        match urls {
            Ok(urls) => {
                for u in urls {
                    if let Err(e) = c.transfer(&u, &t.out) {
                        rc = e.code;
                    }
                }
            }
            Err(e) => {
                c.report(&e);
                rc = e.code;
            }
        }
    }
    rc
}

impl Client<'_> {
    fn report(&self, e: &Error) {
        if !self.o.silent || self.o.show_error {
            eprintln!("crust: {e}");
        }
    }

    fn transfer(&mut self, raw: &str, out: &OutSpec) -> Result<()> {
        let mut info = Info::new(raw);
        let r = self.follow(raw, out, &mut info);
        info.t_total = info.start.elapsed();
        if let Err(e) = &r {
            info.exitcode = e.code;
            info.errormsg = e.msg.clone();
            self.report(e);
        }
        if let Some(fmt) = &self.o.write_out {
            let _ = stdout_raw().write_all(format_write_out(fmt, &info).as_bytes());
        }
        r
    }

    /// One logical transfer, following redirects if `-L`.
    fn follow(&mut self, raw: &str, out: &OutSpec, info: &mut Info) -> Result<()> {
        let o = self.o;
        let mut url = Url::parse(raw)?;
        let first_host = url.host.clone();
        let mut body: Option<Vec<u8>> = None;
        let mut method = "GET";
        if let Some(d) = &o.data {
            if o.get {
                let sep = if url.path.contains('?') { '&' } else { '?' };
                url.path = format!("{}{sep}{}", url.path, String::from_utf8_lossy(d));
            } else {
                body = Some(d.clone());
                method = "POST";
            }
        }
        if o.head {
            method = "HEAD";
        }
        let deadline = o.max_time.map(|d| info.start + d);
        let mut sink = Sink::new(out, &url)?;

        loop {
            info.url_effective = url.to_string();
            info.scheme = if url.tls { "HTTPS" } else { "HTTP" };
            let request_method = o.method.as_deref().unwrap_or(method);
            info.method = request_method.to_string();
            info.size_upload = body.as_ref().map_or(0, |b| b.len() as u64);

            let req = self.build_request(&url, request_method, body.as_deref(), url.host == first_host);
            let (mut conn, resp) = self.send(&url, &req, deadline, info)?;
            info.code = resp.code;
            info.version = resp.version;
            info.size_header += resp.raw.len() as u64;
            info.content_type = resp.header("content-type").unwrap_or("").to_string();
            if o.verbose {
                verbose_lines('<', &resp.raw);
            }
            if o.include || o.head {
                sink.write(&resp.raw)?;
            }
            let fr = framing(request_method, &resp)?;
            let keep = resp.keep_alive();

            if o.follow
                && matches!(resp.code, 301 | 302 | 303 | 307 | 308)
                && let Some(loc) = resp.header("location")
            {
                let next = url.join(loc)?;
                let mut discarded = 0;
                let clean = read_body(&mut conn, &fr, None, deadline, &mut discarded)?;
                self.release(conn, keep && clean);
                info.num_redirects += 1;
                if info.num_redirects > o.max_redirs {
                    return Err(Error::new(E_TOO_MANY_REDIRECTS, format!("Maximum ({}) redirects followed", o.max_redirs)));
                }
                let to_get = (resp.code == 303 && method != "HEAD") || (matches!(resp.code, 301 | 302) && method == "POST");
                // -X overrides the wire method while redirects still update the request body.
                if to_get {
                    method = "GET";
                    body = None;
                }
                if o.verbose {
                    eprintln!("* Issue another request to this URL: '{}'", next.to_string());
                }
                url = next;
                continue;
            }

            if o.fail && resp.code >= 400 {
                return Err(Error::new(E_HTTP_RETURNED_ERROR, format!("The requested URL returned error: {}", resp.code)));
            }
            let clean = read_body(&mut conn, &fr, Some(&mut sink), deadline, &mut info.size_download)?;
            self.release(conn, keep && clean);
            return sink.open_pending();
        }
    }

    fn release(&mut self, conn: Conn, reusable: bool) {
        if reusable {
            if self.o.verbose {
                eprintln!("* Connection #{} to host {} left intact", conn.id, conn.host);
            }
            self.pool.push(conn);
        } else if self.o.verbose {
            eprintln!("* Closing connection #{}", conn.id);
        }
    }

    fn build_request(&self, url: &Url, method: &str, body: Option<&[u8]>, same_host: bool) -> Vec<u8> {
        let o = self.o;
        let mut h: Vec<(String, String)> = vec![
            ("Host".into(), url.host_header()),
            ("User-Agent".into(), o.user_agent.clone().unwrap_or_else(|| DEFAULT_UA.into())),
            ("Accept".into(), "*/*".into()),
        ];
        if let Some(cred) = o.user.as_ref().or(url.userinfo.as_ref()).filter(|_| same_host) {
            let cred = if cred.contains(':') { cred.clone() } else { format!("{cred}:") };
            h.push(("Authorization".into(), format!("Basic {}", base64(cred.as_bytes()))));
        }
        if let Some(r) = &o.referer {
            h.push(("Referer".into(), r.clone()));
        }
        if let Some(b) = body {
            h.push(("Content-Length".into(), b.len().to_string()));
            h.push(("Content-Type".into(), "application/x-www-form-urlencoded".into()));
        }
        for raw in &o.headers {
            // "Name: value" sets, "Name:" removes, "Name;" sends an empty header.
            let (name, value, empty_ok) = match raw.split_once(':') {
                Some((n, v)) => (n.trim(), v.trim(), false),
                None => match raw.strip_suffix(';') {
                    Some(n) => (n.trim(), "", true),
                    None => continue,
                },
            };
            if name.is_empty()
                || (!same_host && (name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("cookie")))
            {
                continue;
            }
            let pos = h.iter().position(|(k, _)| k.eq_ignore_ascii_case(name));
            if value.is_empty() && !empty_ok {
                if let Some(i) = pos {
                    h.remove(i);
                }
                continue;
            }
            match pos {
                Some(i) => h[i] = (name.into(), value.into()),
                None => h.push((name.into(), value.into())),
            }
        }

        let mut req = Vec::with_capacity(256 + body.map_or(0, <[u8]>::len));
        req.extend_from_slice(format!("{method} {} HTTP/1.1\r\n", url.path).as_bytes());
        for (k, v) in &h {
            req.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
        }
        req.extend_from_slice(b"\r\n");
        if let Some(b) = body {
            req.extend_from_slice(b);
        }
        req
    }

    /// Send a request and read the response head, retrying once on a fresh
    /// connection if a pooled one turns out to be dead.
    fn send(&mut self, url: &Url, req: &[u8], deadline: Option<Instant>, info: &mut Info) -> Result<(Conn, Resp)> {
        let mut conn = self.connect(url, info, deadline)?;
        match self.exchange(&mut conn, req, deadline, info) {
            Ok(r) => Ok((conn, r)),
            Err(e) if conn.reused && matches!(e.code, E_GOT_NOTHING | E_SEND_ERROR) => {
                if self.o.verbose {
                    eprintln!("* Connection #{} died, retrying with a fresh connection", conn.id);
                }
                drop(conn);
                let mut conn = self.connect_new(url, info, deadline)?;
                let r = self.exchange(&mut conn, req, deadline, info)?;
                Ok((conn, r))
            }
            Err(e) => Err(e),
        }
    }

    fn exchange(&self, conn: &mut Conn, req: &[u8], deadline: Option<Instant>, info: &mut Info) -> Result<Resp> {
        if self.o.verbose {
            let head_end = req.windows(4).position(|w| w == b"\r\n\r\n").map_or(req.len(), |i| i + 4);
            verbose_lines('>', &req[..head_end]);
        }
        info.t_pretransfer = info.start.elapsed();
        conn.stream
            .write_all(req)
            .and_then(|_| conn.stream.flush())
            .map_err(|e| Error::new(E_SEND_ERROR, format!("Send failure: {e}")))?;
        let r = read_head(conn, deadline)?;
        info.t_starttransfer = info.start.elapsed();
        Ok(r)
    }

    fn connect(&mut self, url: &Url, info: &mut Info, deadline: Option<Instant>) -> Result<Conn> {
        if let Some(i) = self.pool.iter().position(|c| c.tls == url.tls && c.port == url.port && c.host == url.host) {
            let mut c = self.pool.swap_remove(i);
            c.reused = true;
            if self.o.verbose {
                eprintln!("* Re-using existing connection #{} with host {}", c.id, c.host);
            }
            let now = info.start.elapsed();
            info.t_namelookup = now;
            info.t_connect = now;
            info.t_appconnect = if url.tls { now } else { Duration::ZERO };
            return Ok(c);
        }
        self.connect_new(url, info, deadline)
    }

    fn connect_new(&mut self, url: &Url, info: &mut Info, deadline: Option<Instant>) -> Result<Conn> {
        let o = self.o;
        let resolve_err = || Error::new(E_COULDNT_RESOLVE_HOST, format!("Could not resolve host: {}", url.host));
        let addrs: Vec<SocketAddr> =
            (url.host.as_str(), url.port).to_socket_addrs().map_err(|_| resolve_err())?.collect();
        if addrs.is_empty() {
            return Err(resolve_err());
        }
        info.t_namelookup = info.start.elapsed();

        let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        let timeout = match (o.connect_timeout, left) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
        .map(|t| t.max(Duration::from_millis(1)));

        let mut last_err = None;
        let mut found = None;
        for a in &addrs {
            if o.verbose {
                eprintln!("*   Trying {a}...");
            }
            let r = match timeout {
                Some(t) => TcpStream::connect_timeout(a, t),
                None => TcpStream::connect(a),
            };
            match r {
                Ok(s) => {
                    found = Some((s, *a));
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        let Some((sock, addr)) = found else {
            let ms = info.start.elapsed().as_millis();
            let timed_out = last_err.is_some_and(|e| e.kind() == ErrorKind::TimedOut);
            return Err(if timed_out {
                Error::new(
                    E_OPERATION_TIMEDOUT,
                    format!("Failed to connect to {} port {} after {ms} ms: Timeout was reached", url.host, url.port),
                )
            } else {
                Error::new(
                    E_COULDNT_CONNECT,
                    format!("Failed to connect to {} port {} after {ms} ms: Couldn't connect to server", url.host, url.port),
                )
            });
        };
        let _ = sock.set_nodelay(true);
        info.t_connect = info.start.elapsed();
        info.remote_ip = addr.ip().to_string();
        info.remote_port = addr.port();
        if o.verbose {
            eprintln!("* Connected to {} ({}) port {}", url.host, addr.ip(), addr.port());
        }
        let id = self.next_id;
        self.next_id += 1;

        let stream = if url.tls {
            let cfg = self.tls.get_or_insert_with(|| crate::tls::config(o.insecure)).clone();
            let name = ServerName::try_from(url.host.clone())
                .map_err(|_| Error::new(E_URL_MALFORMAT, format!("invalid TLS server name: {}", url.host)))?;
            let mut tls = ClientConnection::new(cfg, name).map_err(|e| Error::new(E_SSL_CONNECT_ERROR, e.to_string()))?;
            let mut sock = sock;
            if let Some(d) = deadline {
                let _ = sock.set_read_timeout(Some(d.saturating_duration_since(Instant::now()).max(Duration::from_millis(1))));
            }
            while tls.is_handshaking() {
                tls.complete_io(&mut sock).map_err(tls_error)?;
            }
            if o.verbose {
                let suite = tls.negotiated_cipher_suite().map(|s| format!("{:?}", s.suite())).unwrap_or_default();
                let ver = tls.protocol_version().map(|v| format!("{v:?}")).unwrap_or_default();
                eprintln!("* SSL connection using {ver} / {suite}");
                if let Some(p) = tls.alpn_protocol() {
                    eprintln!("* ALPN: server accepted {}", String::from_utf8_lossy(p));
                }
            }
            info.t_appconnect = info.start.elapsed();
            Stream::Tls(Box::new(StreamOwned::new(tls, sock)))
        } else {
            Stream::Plain(sock)
        };

        Ok(Conn {
            stream,
            buf: vec![0u8; BUF_SIZE].into_boxed_slice(),
            pos: 0,
            len: 0,
            tls: url.tls,
            host: url.host.clone(),
            port: url.port,
            id,
            reused: false,
        })
    }
}

// ---------------------------------------------------------------- --write-out

fn format_write_out(fmt: &str, i: &Info) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(x) => {
                    out.push('\\');
                    out.push(x);
                }
                None => out.push('\\'),
            },
            '%' if chars.peek() == Some(&'%') => {
                chars.next();
                out.push('%');
            }
            '%' if chars.peek() == Some(&'{') => {
                chars.next();
                let name: String = chars.by_ref().take_while(|&c| c != '}').collect();
                match write_out_var(i, &name) {
                    Some(v) => out.push_str(&v),
                    None => eprintln!("crust: unknown --write-out variable: '{name}'"),
                }
            }
            c => out.push(c),
        }
    }
    out
}

fn write_out_var(i: &Info, name: &str) -> Option<String> {
    let secs = |d: Duration| format!("{:.6}", d.as_secs_f64());
    Some(match name {
        "http_code" | "response_code" => format!("{:03}", i.code),
        "http_version" => match (i.code, i.version) {
            (0, _) => "0".into(),
            (_, 0) => "1.0".into(),
            _ => "1.1".into(),
        },
        "size_download" => i.size_download.to_string(),
        "size_header" => i.size_header.to_string(),
        "size_upload" => i.size_upload.to_string(),
        "time_namelookup" => secs(i.t_namelookup),
        "time_connect" => secs(i.t_connect),
        "time_appconnect" => secs(i.t_appconnect),
        "time_pretransfer" => secs(i.t_pretransfer),
        "time_starttransfer" => secs(i.t_starttransfer),
        "time_total" => secs(i.t_total),
        "speed_download" => {
            let t = i.t_total.as_secs_f64();
            (if t > 0.0 { (i.size_download as f64 / t) as u64 } else { 0 }).to_string()
        }
        "url" => i.url.clone(),
        "url_effective" => i.url_effective.clone(),
        "num_redirects" => i.num_redirects.to_string(),
        "content_type" => i.content_type.clone(),
        "remote_ip" => i.remote_ip.clone(),
        "remote_port" => i.remote_port.to_string(),
        "method" => i.method.clone(),
        "scheme" => i.scheme.into(),
        "exitcode" => i.exitcode.to_string(),
        "errormsg" => i.errormsg.clone(),
        _ => return None,
    })
}
