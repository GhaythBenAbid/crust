//! Minimal http(s) URL handling: just what an HTTP/1.1 client needs.

use crate::util::{Error, Result, E_UNSUPPORTED_PROTOCOL, E_URL_MALFORMAT};

#[derive(Clone, Debug, PartialEq)]
pub struct Url {
    pub tls: bool,
    /// Hostname or IP, without IPv6 brackets.
    pub host: String,
    pub port: u16,
    /// Path plus query, always starting with '/'.
    pub path: String,
    pub userinfo: Option<String>,
}

impl Url {
    pub fn parse(s: &str) -> Result<Url> {
        let (scheme, rest) = match s.find("://") {
            Some(i) => (s[..i].to_ascii_lowercase(), &s[i + 3..]),
            None => ("http".to_string(), s), // curl guesses http
        };
        let tls = match scheme.as_str() {
            "http" => false,
            "https" => true,
            _ => return Err(Error::new(E_UNSUPPORTED_PROTOCOL, format!("Protocol \"{scheme}\" not supported"))),
        };
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (auth, tail) = rest.split_at(end);
        let (userinfo, hostport) = match auth.rfind('@') {
            Some(i) => (Some(auth[..i].to_string()), &auth[i + 1..]),
            None => (None, auth),
        };
        let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
            let close = v6.find(']').ok_or_else(|| bad("bad IPv6 address"))?;
            let after = &v6[close + 1..];
            let port = match after {
                "" => None,
                p => Some(p.strip_prefix(':').ok_or_else(|| bad("bad port"))?),
            };
            (&v6[..close], port)
        } else {
            match hostport.rsplit_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (hostport, None),
            }
        };
        if host.is_empty() {
            return Err(bad("No host part in the URL"));
        }
        let default_port = if tls { 443 } else { 80 };
        let port = match port {
            None | Some("") => default_port,
            Some(p) => p.parse().map_err(|_| bad("Port number was not a decimal number between 0 and 65535"))?,
        };
        let tail = tail.split('#').next().unwrap_or("");
        let path = if tail.starts_with('/') { tail.to_string() } else { format!("/{tail}") };
        Ok(Url { tls, host: host.to_ascii_lowercase(), port, path, userinfo })
    }

    pub fn scheme(&self) -> &'static str {
        if self.tls { "https" } else { "http" }
    }

    /// Value for the Host header: brackets for IPv6, port only if non-default.
    pub fn host_header(&self) -> String {
        let h = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if self.port == if self.tls { 443 } else { 80 } { h } else { format!("{h}:{}", self.port) }
    }

    pub fn to_string(&self) -> String {
        format!("{}://{}{}", self.scheme(), self.host_header(), self.path)
    }

    /// Last path segment, used by `-O`.
    pub fn file_name(&self) -> &str {
        let p = self.path.split('?').next().unwrap_or("");
        p.rsplit('/').next().unwrap_or("")
    }

    /// Resolve a redirect `Location` against this URL.
    pub fn join(&self, loc: &str) -> Result<Url> {
        let loc = loc.trim();
        if let Some(i) = loc.find("://")
            && !loc[..i].contains(['/', '?'])
        {
            return Url::parse(loc);
        }
        if let Some(rest) = loc.strip_prefix("//") {
            return Url::parse(&format!("{}://{rest}", self.scheme()));
        }
        let loc = loc.split('#').next().unwrap_or("");
        let base = self.path.split('?').next().unwrap_or("/");
        let path = if loc.starts_with('/') {
            loc.to_string()
        } else if loc.starts_with('?') {
            format!("{base}{loc}")
        } else if loc.is_empty() {
            self.path.clone()
        } else {
            let dir = &base[..base.rfind('/').map_or(0, |i| i + 1)];
            format!("{dir}{loc}")
        };
        Ok(Url { path, userinfo: None, ..self.clone() })
    }
}

fn bad(msg: &str) -> Error {
    Error::new(E_URL_MALFORMAT, format!("URL rejected: {msg}"))
}

#[cfg(test)]
mod tests {
    use super::Url;

    #[test]
    fn parse() {
        let u = Url::parse("https://User:pw@Example.com:8443/a/b?q=1#frag").unwrap();
        assert!(u.tls);
        assert_eq!((u.host.as_str(), u.port, u.path.as_str()), ("example.com", 8443, "/a/b?q=1"));
        assert_eq!(u.userinfo.as_deref(), Some("User:pw"));
        assert_eq!(u.host_header(), "example.com:8443");

        let u = Url::parse("example.com").unwrap();
        assert_eq!((u.tls, u.port, u.path.as_str(), u.host_header()), (false, 80, "/", "example.com".into()));

        let u = Url::parse("http://[::1]:8080?x").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str()), ("::1", 8080, "/?x"));
        assert_eq!(u.host_header(), "[::1]:8080");

        assert!(Url::parse("ftp://x").is_err());
        assert!(Url::parse("http://:80/").is_err());
        assert!(Url::parse("http://h:99999/").is_err());
    }

    #[test]
    fn join() {
        let u = Url::parse("http://h/a/b?q").unwrap();
        assert_eq!(u.join("/x").unwrap().path, "/x");
        assert_eq!(u.join("c").unwrap().path, "/a/c");
        assert_eq!(u.join("?z").unwrap().path, "/a/b?z");
        assert_eq!(u.join("https://o/p").unwrap().to_string(), "https://o/p");
        assert_eq!(u.join("//o2/p").unwrap().to_string(), "http://o2/p");
    }

    #[test]
    fn file_name() {
        assert_eq!(Url::parse("http://h/a/f.tar.gz?x=1").unwrap().file_name(), "f.tar.gz");
        assert_eq!(Url::parse("http://h/").unwrap().file_name(), "");
    }
}
