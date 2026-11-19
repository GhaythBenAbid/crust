use std::fmt;

/// An error carrying a curl-compatible exit code.
#[derive(Debug)]
pub struct Error {
    pub code: u8,
    pub msg: String,
}

impl Error {
    pub fn new(code: u8, msg: impl Into<String>) -> Self {
        Error { code, msg: msg.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}) {}", self.code, self.msg)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

// curl exit codes we use.
pub const E_UNSUPPORTED_PROTOCOL: u8 = 1;
pub const E_USAGE: u8 = 2;
pub const E_URL_MALFORMAT: u8 = 3;
pub const E_COULDNT_RESOLVE_HOST: u8 = 6;
pub const E_COULDNT_CONNECT: u8 = 7;
pub const E_WEIRD_SERVER_REPLY: u8 = 8;
pub const E_PARTIAL_FILE: u8 = 18;
pub const E_HTTP_RETURNED_ERROR: u8 = 22;
pub const E_WRITE_ERROR: u8 = 23;
pub const E_READ_ERROR: u8 = 26;
pub const E_OPERATION_TIMEDOUT: u8 = 28;
pub const E_SSL_CONNECT_ERROR: u8 = 35;
pub const E_TOO_MANY_REDIRECTS: u8 = 47;
pub const E_GOT_NOTHING: u8 = 52;
pub const E_SEND_ERROR: u8 = 55;
pub const E_RECV_ERROR: u8 = 56;
pub const E_PEER_FAILED_VERIFICATION: u8 = 60;

pub fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for c in input.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
    }
}
