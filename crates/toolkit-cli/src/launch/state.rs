//! Local state: `~/.launch-with-aws/session.json` and `config.json`
//! (R-TOK-STORE, R-TOK-MODE, R-TOK-MEM; Python `write_private_file`,
//! `load_session`, `save_session`, `load_saved_region`, `save_region`).
//!
//! - The directory is created 0700 and must be a real directory (a symlink in
//!   its place is refused); it is `chmod 0700` again on every write.
//! - A write goes to `<name>.<16 hex>.tmp`, created exclusively with mode
//!   0600, is `sync_all`ed and renamed over the target; any failure removes
//!   the temporary file.
//! - A read opens the file without following a symlink (a link is "no
//!   file"), requires a regular file, tightens group/other bits to 0600 on
//!   the open handle and reads at most [`MAX_STATE_FILE_BYTES`].
//! - `session.json` is byte-compatible with the Python both ways: the same
//!   nine members in the same order, `json.dumps(indent=2)` layout with
//!   `ensure_ascii`. Parsing is strict: a missing, unknown, duplicated or
//!   wrongly typed member means "no session".

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use toolkit_aws::{Env, Secret};
use toolkit_core::{parse_json, Json};

use super::clock::{random_hex, Random};
use super::config::validate_region;
use super::LaunchError;

/// The state directory name under `HOME`.
pub const STATE_DIR_NAME: &str = ".launch-with-aws";
/// The session file.
pub const SESSION_FILE_NAME: &str = "session.json";
/// The saved-region file.
pub const CONFIG_FILE_NAME: &str = "config.json";
/// Largest state file read (R-TOK-MODE).
pub const MAX_STATE_FILE_BYTES: u64 = 64 * 1024;

/// The state directory, `$HOME/.launch-with-aws`.
///
/// # Errors
/// [`LaunchError::Config`] when `HOME` is unset, empty or relative.
pub fn state_dir(env: &dyn Env) -> Result<PathBuf, LaunchError> {
    match env.var("HOME") {
        Some(home) if Path::new(&home).is_absolute() => Ok(Path::new(&home).join(STATE_DIR_NAME)),
        _ => Err(LaunchError::Config(
            "HOME must be set to an absolute path to locate the Launch with AWS state directory."
                .to_string(),
        )),
    }
}

/// Create `dir` 0700 if missing, require a real directory, `chmod 0700`.
fn prepare_dir(dir: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the state directory path is not a directory",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(dir)?;
            // A racing creator may have left a symlink: check again.
            if !fs::symlink_metadata(dir)?.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "the state directory path is not a directory",
                ));
            }
        }
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Write `bytes` to `dir/name` privately and atomically.
///
/// # Errors
/// The I/O failure; the temporary file is removed and the target unchanged.
pub fn write_private_file(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    random: &dyn Random,
) -> io::Result<()> {
    prepare_dir(dir)?;
    let nonce = random_hex(random, 8).map_err(|error| io::Error::other(error.to_string()))?;
    let temp = dir.join(format!("{name}.{nonce}.tmp"));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            fs::rename(&temp, dir.join(name))
        });
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// The bytes of `dir/name`, or `None` when it is absent, a symlink, not a
/// regular file or larger than [`MAX_STATE_FILE_BYTES`].
///
/// # Errors
/// Any other I/O failure.
pub fn read_private_file(dir: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
    let path = dir.join(name);
    let Some((file, size, mode)) = open_no_follow(&path)? else {
        return Ok(None);
    };
    if size > MAX_STATE_FILE_BYTES {
        return Ok(None);
    }
    if mode & 0o077 != 0 {
        tighten(&file)?;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    file.take(MAX_STATE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE_FILE_BYTES {
        return Ok(None);
    }
    Ok(Some(bytes))
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> io::Result<Option<(fs::File, u64, u32)>> {
    super::archive::fsops::open_regular_nofollow(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> io::Result<Option<(fs::File, u64, u32)>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => {
            let file = fs::File::open(path)?;
            Ok(Some((file, meta.len(), 0o600)))
        }
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn tighten(file: &fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn tighten(_file: &fs::File) -> io::Result<()> {
    Ok(())
}

/// Remove `dir/name` if it exists as a file or a symlink (the link itself,
/// never its target); `true` when something was removed.
///
/// # Errors
/// An I/O failure other than "not found".
pub fn remove_state_file(dir: &Path, name: &str) -> io::Result<bool> {
    let path = dir.join(name);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_dir() => Ok(false),
        Ok(_) => match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// A persisted session: the client registration and the current token pair.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredSession {
    /// OIDC client id (not secret).
    pub client_id: String,
    /// OIDC client secret.
    pub client_secret: Secret,
    /// Unix seconds when the registration expires.
    pub client_expires_at: i64,
    /// Authorize endpoint recorded at registration.
    pub authorize_endpoint: String,
    /// Token endpoint recorded at registration.
    pub token_endpoint: String,
    /// Granted scopes.
    pub scopes: Vec<String>,
    /// Bearer token.
    pub access_token: Secret,
    /// Refresh token.
    pub refresh_token: Secret,
    /// Unix seconds when the access token expires.
    pub token_expires_at: i64,
}

impl std::fmt::Debug for StoredSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredSession")
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret)
            .field("client_expires_at", &self.client_expires_at)
            .field("authorize_endpoint", &self.authorize_endpoint)
            .field("token_endpoint", &self.token_endpoint)
            .field("scopes", &self.scopes)
            .field("access_token", &self.access_token)
            .field("refresh_token", &self.refresh_token)
            .field("token_expires_at", &self.token_expires_at)
            .finish()
    }
}

/// The session members, in the Python dataclass order.
const SESSION_KEYS: [&str; 9] = [
    "client_id",
    "client_secret",
    "client_expires_at",
    "authorize_endpoint",
    "token_endpoint",
    "scopes",
    "access_token",
    "refresh_token",
    "token_expires_at",
];

impl StoredSession {
    /// Parse the session file strictly; `None` for anything but the exact
    /// nine-member schema.
    #[must_use]
    pub fn from_json_bytes(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        let Json::Object(members) = parse_json(text).ok()? else {
            return None;
        };
        if members.len() != SESSION_KEYS.len()
            || !SESSION_KEYS
                .iter()
                .all(|key| members.iter().filter(|(name, _)| name == key).count() == 1)
        {
            return None;
        }
        let get = |key: &str| {
            members
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value)
        };
        let text_of = |key: &str| match get(key)? {
            Json::String(value) => Some(value.clone()),
            _ => None,
        };
        let int_of = |key: &str| match get(key)? {
            Json::Int(value) => Some(*value),
            _ => None,
        };
        let scopes = match get("scopes")? {
            Json::Array(items) => items
                .iter()
                .map(|item| match item {
                    Json::String(value) => Some(value.clone()),
                    _ => None,
                })
                .collect::<Option<Vec<String>>>()?,
            _ => return None,
        };
        Some(Self {
            client_id: text_of("client_id")?,
            client_secret: Secret::new(text_of("client_secret")?),
            client_expires_at: int_of("client_expires_at")?,
            authorize_endpoint: text_of("authorize_endpoint")?,
            token_endpoint: text_of("token_endpoint")?,
            scopes,
            access_token: Secret::new(text_of("access_token")?),
            refresh_token: Secret::new(text_of("refresh_token")?),
            token_expires_at: int_of("token_expires_at")?,
        })
    }

    /// The file text, byte-identical to Python's
    /// `json.dumps(asdict(session), indent=2)`.
    #[must_use]
    pub fn to_json_string(&self) -> String {
        let mut out = String::from("{\n");
        let member = |out: &mut String, key: &str, value: &str, last: bool| {
            let _ = writeln!(
                out,
                "  {}: {value}{}",
                py_json_string(key),
                if last { "" } else { "," }
            );
        };
        member(
            &mut out,
            "client_id",
            &py_json_string(&self.client_id),
            false,
        );
        member(
            &mut out,
            "client_secret",
            &py_json_string(self.client_secret.expose()),
            false,
        );
        member(
            &mut out,
            "client_expires_at",
            &self.client_expires_at.to_string(),
            false,
        );
        member(
            &mut out,
            "authorize_endpoint",
            &py_json_string(&self.authorize_endpoint),
            false,
        );
        member(
            &mut out,
            "token_endpoint",
            &py_json_string(&self.token_endpoint),
            false,
        );
        let scopes = if self.scopes.is_empty() {
            "[]".to_string()
        } else {
            let items: Vec<String> = self
                .scopes
                .iter()
                .map(|scope| format!("    {}", py_json_string(scope)))
                .collect();
            format!("[\n{}\n  ]", items.join(",\n"))
        };
        member(&mut out, "scopes", &scopes, false);
        member(
            &mut out,
            "access_token",
            &py_json_string(self.access_token.expose()),
            false,
        );
        member(
            &mut out,
            "refresh_token",
            &py_json_string(self.refresh_token.expose()),
            false,
        );
        member(
            &mut out,
            "token_expires_at",
            &self.token_expires_at.to_string(),
            true,
        );
        out.push('}');
        out
    }
}

/// A JSON string as Python's `json.dumps` writes it (`ensure_ascii=True`).
fn py_json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if c.is_ascii() && u32::from(c) >= 0x20 => out.push(c),
            c => {
                let mut units = [0_u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
    out
}

/// The stored session, or `None` when there is none usable.
#[must_use]
pub fn load_session(dir: &Path) -> Option<StoredSession> {
    let bytes = read_private_file(dir, SESSION_FILE_NAME).ok()??;
    StoredSession::from_json_bytes(&bytes)
}

/// Persist `session`.
///
/// # Errors
/// The write failure.
pub fn save_session(dir: &Path, session: &StoredSession, random: &dyn Random) -> io::Result<()> {
    write_private_file(
        dir,
        SESSION_FILE_NAME,
        session.to_json_string().as_bytes(),
        random,
    )
}

/// The region saved by a previous run, if it is still supported.
#[must_use]
pub fn load_saved_region(dir: &Path) -> Option<&'static str> {
    let bytes = read_private_file(dir, CONFIG_FILE_NAME).ok()??;
    let text = std::str::from_utf8(&bytes).ok()?;
    let parsed = parse_json(text).ok()?;
    let region = parsed.get("region")?.as_str()?;
    validate_region(region).ok()
}

/// Persist the confirmed region as `{"region": ...}` (`indent=2`).
///
/// # Errors
/// [`LaunchError::Config`] for an unsupported region; the write failure as
/// `Ok(Err(..))` so `create-launch` can keep it best effort (L5).
pub fn save_region(
    dir: &Path,
    region: &str,
    random: &dyn Random,
) -> Result<io::Result<()>, LaunchError> {
    let region = validate_region(region)?;
    let text = format!("{{\n  \"region\": {}\n}}", py_json_string(region));
    Ok(write_private_file(
        dir,
        CONFIG_FILE_NAME,
        text.as_bytes(),
        random,
    ))
}

#[cfg(test)]
mod tests_state;
