//! Python `urllib.parse.urlparse` compatibility (R-URLPARSE, x402 S1 #5118).
//!
//! Token material, resource binding and the origin allowlist were computed
//! with Python's `urlparse`, not a WHATWG parser; the two differ on `;params`,
//! IPv6 brackets and case, and those differences reach the hashed bytes. This
//! module reproduces the subset that matters (Python 3.12 `urlsplit` plus
//! `urlparse`'s `_splitparams`):
//!
//! - leading C0 controls and spaces are stripped and `\t`, `\r`, `\n` are
//!   removed anywhere;
//! - the scheme is the text before the first `:` when it starts with an
//!   ASCII letter and holds only letters, digits, `+`, `-` and `.`; it is
//!   lower-cased;
//! - after `//`, the netloc runs to the first `/`, `?` or `#`, kept raw;
//! - unbalanced brackets, text around a bracketed host, an IPv4 address in
//!   brackets and a netloc that NFKC-normalises into URL syntax raise
//!   `ValueError` ([`UrlError`]);
//! - the fragment, then the query, are split off; for a params-using scheme
//!   the part from the first `;` in the last path segment is cut;
//! - [`ParsedUrl::hostname`] drops the userinfo, the port and the brackets
//!   and lower-cases what is left (a zone id keeps its case);
//! - [`ParsedUrl::port`] is the decimal after the host; anything but ASCII
//!   digits, or a value above 65535, raises `ValueError`.
//!
//! These functions are only used for comparison and hashing. A request goes
//! to the validated URL as given.

use std::net::{IpAddr, Ipv6Addr};

use unicode_normalization::UnicodeNormalization;

/// Python raised `ValueError` while parsing the URL or reading its port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UrlError;

/// Schemes for which Python's `urlparse` splits `;params` off the path.
const USES_PARAMS: &[&str] = &[
    "", "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp", "rtsps", "rtspu",
    "sip", "sips", "mms", "sftp", "tel",
];

/// The six `urlparse` components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    scheme: String,
    netloc: String,
    path: String,
    params: String,
    query: String,
    fragment: String,
}

/// Parse `url` as Python's `urlparse` does.
///
/// # Errors
///
/// [`UrlError`] where Python raises `ValueError` (brackets, NFKC netloc).
pub fn urlparse(url: &str) -> Result<ParsedUrl, UrlError> {
    let trimmed = url.trim_start_matches(|c: char| c <= ' ');
    let cleaned: String = trimmed
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut rest = cleaned.as_str();

    let mut scheme = String::new();
    if let Some(colon) = rest.find(':') {
        let candidate = &rest[..colon];
        let valid = candidate
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
            && candidate
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
        if valid {
            scheme = candidate.to_ascii_lowercase();
            rest = &rest[colon + 1..];
        }
    }

    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        after[..end].clone_into(&mut netloc);
        rest = &after[end..];
        let open = netloc.contains('[');
        let close = netloc.contains(']');
        if open != close {
            return Err(UrlError);
        }
        if open {
            check_bracketed_netloc(&netloc)?;
        }
    }

    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    check_netloc(&netloc)?;

    let (path, params) = if USES_PARAMS.contains(&scheme.as_str()) && path.contains(';') {
        split_params(path)
    } else {
        (path, "")
    };

    Ok(ParsedUrl {
        scheme,
        netloc,
        path: path.to_owned(),
        params: params.to_owned(),
        query: query.to_owned(),
        fragment: fragment.to_owned(),
    })
}

/// Python `_splitparams`: cut at the first `;` of the last path segment.
fn split_params(path: &str) -> (&str, &str) {
    let cut = match path.rfind('/') {
        Some(slash) => match path[slash..].find(';') {
            Some(offset) => slash + offset,
            None => return (path, ""),
        },
        None => match path.find(';') {
            Some(index) => index,
            None => return (path, ""),
        },
    };
    (&path[..cut], &path[cut + 1..])
}

/// Python `_check_bracketed_netloc`: nothing before `[`, only `:port` after
/// `]`, and a valid bracketed host.
fn check_bracketed_netloc(netloc: &str) -> Result<(), UrlError> {
    let host_and_port = netloc.rsplit_once('@').map_or(netloc, |(_, host)| host);
    let Some((before, bracketed)) = host_and_port.split_once('[') else {
        return Ok(());
    };
    if !before.is_empty() {
        return Err(UrlError);
    }
    let (host, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
    if !after.is_empty() && !after.starts_with(':') {
        return Err(UrlError);
    }
    check_bracketed_host(host)
}

/// Python `_check_bracketed_host`: an `IPvFuture` literal (`v<hex>.<text>`) or
/// an IPv6 address, optionally with a `%zone`; never IPv4.
fn check_bracketed_host(host: &str) -> Result<(), UrlError> {
    if let Some(future) = host.strip_prefix('v') {
        let (version, text) = future.split_once('.').ok_or(UrlError)?;
        let valid = !version.is_empty()
            && version.bytes().all(|b| b.is_ascii_hexdigit())
            && !text.is_empty();
        return if valid { Ok(()) } else { Err(UrlError) };
    }
    let address = match host.split_once('%') {
        Some((address, zone)) if !zone.is_empty() && !zone.contains('%') => address,
        Some(_) => return Err(UrlError),
        None => host,
    };
    address
        .parse::<Ipv6Addr>()
        .map(|_| ())
        .map_err(|_| UrlError)
}

/// Python `_checknetloc`: a non-ASCII netloc must not normalise (NFKC) into
/// one of `/?#@:` that was not already there.
fn check_netloc(netloc: &str) -> Result<(), UrlError> {
    if netloc.is_ascii() {
        return Ok(());
    }
    let stripped: String = netloc
        .chars()
        .filter(|c| !matches!(c, '@' | ':' | '#' | '?'))
        .collect();
    let normalized: String = stripped.nfkc().collect();
    if normalized == stripped {
        return Ok(());
    }
    if normalized.contains(['/', '?', '#', '@', ':']) {
        return Err(UrlError);
    }
    Ok(())
}

impl ParsedUrl {
    /// The lower-cased scheme, or `""`.
    #[must_use]
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// The raw network location (case and userinfo kept).
    #[must_use]
    pub fn netloc(&self) -> &str {
        &self.netloc
    }

    /// The path without `;params`.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The `;params` cut from the last path segment.
    #[must_use]
    pub fn params(&self) -> &str {
        &self.params
    }

    /// The query, without `?`.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The fragment, without `#`.
    #[must_use]
    pub fn fragment(&self) -> &str {
        &self.fragment
    }

    /// Python `_hostinfo`: the raw host and the raw port text, if any.
    fn hostinfo(&self) -> (&str, Option<&str>) {
        let info = self
            .netloc
            .rsplit_once('@')
            .map_or(self.netloc.as_str(), |(_, host)| host);
        let (host, port) = match info.split_once('[') {
            Some((_, bracketed)) => {
                let (host, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (host, after.split_once(':').map_or("", |(_, port)| port))
            }
            None => info.split_once(':').unwrap_or((info, "")),
        };
        (host, (!port.is_empty()).then_some(port))
    }

    /// Python `hostname`: `None` when empty; otherwise lower-cased, with a
    /// `%zone` kept as written.
    #[must_use]
    pub fn hostname(&self) -> Option<String> {
        let (host, _) = self.hostinfo();
        if host.is_empty() {
            return None;
        }
        Some(match host.split_once('%') {
            Some((address, zone)) => format!("{}%{zone}", address.to_lowercase()),
            None => host.to_lowercase(),
        })
    }

    /// Python `port`.
    ///
    /// # Errors
    ///
    /// [`UrlError`] for anything but ASCII digits or a value above 65535.
    pub fn port(&self) -> Result<Option<u16>, UrlError> {
        let (_, port) = self.hostinfo();
        let Some(port) = port else {
            return Ok(None);
        };
        if !port.bytes().all(|b| b.is_ascii_digit()) {
            return Err(UrlError);
        }
        let significant = port.trim_start_matches('0');
        if significant.is_empty() {
            return Ok(Some(0));
        }
        if significant.len() > 5 {
            return Err(UrlError);
        }
        significant
            .parse::<u32>()
            .ok()
            .and_then(|value| u16::try_from(value).ok())
            .map(Some)
            .ok_or(UrlError)
    }

    /// Python's truthiness of `username or password`: userinfo with a
    /// non-empty user or a non-empty password.
    #[must_use]
    pub fn has_credentials(&self) -> bool {
        let Some((userinfo, _)) = self.netloc.rsplit_once('@') else {
            return false;
        };
        let (user, password) = userinfo.split_once(':').unwrap_or((userinfo, ""));
        !user.is_empty() || !password.is_empty()
    }

    /// The legacy `_canonical_origin`: `scheme://host`, plus `:port` when the
    /// port is set and is neither 0 nor 443.
    ///
    /// # Errors
    ///
    /// [`UrlError`] when the port raises.
    pub fn canonical_origin(&self) -> Result<String, UrlError> {
        let host = self.hostname().unwrap_or_default().to_lowercase();
        Ok(match self.port()? {
            Some(port) if port != 0 && port != 443 => format!("{}://{host}:{port}", self.scheme),
            _ => format!("{}://{host}", self.scheme),
        })
    }

    /// `scheme://netloc` plus the path, trailing `/` stripped: the form
    /// R-RESBIND compares.
    #[must_use]
    pub fn binding_base(&self) -> String {
        format!("{}://{}{}", self.scheme, self.netloc, self.path)
            .trim_end_matches('/')
            .to_owned()
    }

    /// The host as an address literal, when it is one.
    #[must_use]
    pub fn address_literal(&self) -> Option<IpAddr> {
        self.hostname()?.parse().ok()
    }
}

/// The legacy `_canonical_origin(url)`.
///
/// # Errors
///
/// [`UrlError`] where Python's `urlparse` or `port` raises.
pub fn canonical_origin(url: &str) -> Result<String, UrlError> {
    urlparse(url)?.canonical_origin()
}
