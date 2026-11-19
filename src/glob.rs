//! curl-style URL globbing: `[1-100]`, `[001-100]`, `[0-100:10]`, `[a-z]`, `{one,two}`.
//! A bracket group that isn't a valid range (e.g. an IPv6 literal) stays literal.

use crate::util::{Error, Result, E_URL_MALFORMAT};

const MAX_URLS: usize = 1_000_000;

fn range(spec: &str) -> Option<Vec<String>> {
    let (body, step) = match spec.rsplit_once(':') {
        Some((b, s)) => (b, s.parse::<u64>().ok().filter(|&s| s > 0)?),
        None => (spec, 1),
    };
    let (a, b) = body.split_once('-')?;
    if let (Ok(lo), Ok(hi)) = (a.parse::<u64>(), b.parse::<u64>()) {
        if lo > hi || (hi - lo) / step >= MAX_URLS as u64 {
            return None;
        }
        let width = if a.len() > 1 && a.starts_with('0') { a.len() } else { 0 };
        return Some((lo..=hi).step_by(step as usize).map(|n| format!("{n:0width$}")).collect());
    }
    let (ca, cb) = (a.chars().collect::<Vec<_>>(), b.chars().collect::<Vec<_>>());
    if let ([x], [y]) = (ca.as_slice(), cb.as_slice())
        && x.is_ascii_alphabetic()
        && y.is_ascii_alphabetic()
        && x <= y
    {
        return Some((*x as u8..=*y as u8).step_by(step as usize).map(|c| (c as char).to_string()).collect());
    }
    None
}

pub fn expand(url: &str) -> Result<Vec<String>> {
    let mut parts: Vec<Vec<String>> = Vec::new();
    let mut lit = String::new();
    let mut rest = url;
    while let Some(i) = rest.find(['[', '{']) {
        lit.push_str(&rest[..i]);
        let open = rest.as_bytes()[i];
        let close = if open == b'[' { ']' } else { '}' };
        let Some(j) = rest[i..].find(close).map(|j| i + j) else {
            return Err(Error::new(E_URL_MALFORMAT, format!("bad range in URL position {}: unmatched brace", url.len() - rest.len() + i + 1)));
        };
        let inner = &rest[i + 1..j];
        let alts = if open == b'{' { Some(inner.split(',').map(str::to_string).collect()) } else { range(inner) };
        match alts {
            Some(a) => {
                parts.push(vec![std::mem::take(&mut lit)]);
                parts.push(a);
            }
            None => lit.push_str(&rest[i..=j]),
        }
        rest = &rest[j + 1..];
    }
    lit.push_str(rest);
    parts.push(vec![lit]);

    let mut acc = vec![String::new()];
    for part in parts {
        if acc.len().saturating_mul(part.len()) > MAX_URLS {
            return Err(Error::new(E_URL_MALFORMAT, "too many URLs from globbing"));
        }
        acc = acc.iter().flat_map(|p| part.iter().map(move |s| format!("{p}{s}"))).collect();
    }
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use super::expand;

    #[test]
    fn globs() {
        assert_eq!(expand("h/x").unwrap(), vec!["h/x"]);
        assert_eq!(expand("h/[1-3]").unwrap(), vec!["h/1", "h/2", "h/3"]);
        assert_eq!(expand("h/[08-10]").unwrap(), vec!["h/08", "h/09", "h/10"]);
        assert_eq!(expand("h/[0-10:5]").unwrap(), vec!["h/0", "h/5", "h/10"]);
        assert_eq!(expand("h/[a-c]").unwrap(), vec!["h/a", "h/b", "h/c"]);
        assert_eq!(expand("h/{x,y}[1-2]").unwrap(), vec!["h/x1", "h/x2", "h/y1", "h/y2"]);
        assert_eq!(expand("http://[::1]:80/").unwrap(), vec!["http://[::1]:80/"]);
        assert_eq!(expand("h/?[1-100]").unwrap().len(), 100);
        assert!(expand("h/[1-2").is_err());
    }
}
