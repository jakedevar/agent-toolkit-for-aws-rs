//! Secret-bearing callback configuration and helper process handshake.
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;
use toolkit_aws::Secret;
use toolkit_core::{parse_json, Json};

const LIMIT: u64 = 64 * 1024;

/// The child configuration is sent only on its stdin, never argv or environment.
#[derive(Clone)]
pub struct HelperConfig {
    pub client_id: String,
    pub client_secret: Secret,
    pub client_expires_at: i64,
    pub scopes: Vec<String>,
    pub code_verifier: Secret,
    pub state: Secret,
    pub deadline: i64,
}

impl HelperConfig {
    /// Strictly decode a bounded stdin object, rejecting duplicate and unknown fields.
    pub fn read(mut input: impl Read) -> io::Result<Self> {
        let mut bytes = Vec::new();
        input.by_ref().take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > LIMIT {
            return Err(io::Error::other("invalid helper configuration"));
        }
        let invalid = || io::Error::other("invalid helper configuration");
        let Json::Object(members) = parse_json(std::str::from_utf8(&bytes).map_err(|_| invalid())?)
            .map_err(|_| invalid())?
        else {
            return Err(invalid());
        };
        let keys = [
            "client_id",
            "client_secret",
            "client_expires_at",
            "scopes",
            "code_verifier",
            "state",
            "deadline",
        ];
        if members.len() != keys.len()
            || !keys
                .iter()
                .all(|key| members.iter().filter(|(name, _)| name == key).count() == 1)
        {
            return Err(invalid());
        }
        let get = |key: &str| {
            members
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value)
        };
        let string = |key: &str| match get(key) {
            Some(Json::String(value)) if !value.is_empty() => Ok(value.clone()),
            _ => Err(invalid()),
        };
        let integer = |key: &str| match get(key) {
            Some(Json::Int(value)) => Ok(*value),
            _ => Err(invalid()),
        };
        let scopes = match get("scopes") {
            Some(Json::Array(values)) => values
                .iter()
                .map(|v| match v {
                    Json::String(s) if !s.is_empty() => Ok(s.clone()),
                    _ => Err(invalid()),
                })
                .collect::<io::Result<Vec<_>>>()?,
            _ => return Err(invalid()),
        };
        Ok(Self {
            client_id: string("client_id")?,
            client_secret: Secret::new(string("client_secret")?),
            client_expires_at: integer("client_expires_at")?,
            scopes,
            code_verifier: Secret::new(string("code_verifier")?),
            state: Secret::new(string("state")?),
            deadline: integer("deadline")?,
        })
    }
    fn to_bytes(&self) -> Vec<u8> {
        serde_json::json!({"client_id":self.client_id,"client_secret":self.client_secret.expose(),"client_expires_at":self.client_expires_at,"scopes":self.scopes,"code_verifier":self.code_verifier.expose(),"state":self.state.expose(),"deadline":self.deadline}).to_string().into_bytes()
    }
}

/// Launch a detached helper, returning its pid and announced port.
///
/// # Errors
/// A fixed error if startup, bounded stdin or the port handshake fails.
#[cfg(unix)]
pub fn spawn(config: &HelperConfig) -> io::Result<(u32, u16)> {
    use std::os::unix::process::CommandExt;
    let fail = || io::Error::other("The sign-in helper failed to start.");
    let bytes = config.to_bytes();
    if bytes.len() as u64 > LIMIT {
        return Err(fail());
    }
    let mut child = Command::new(std::env::current_exe().map_err(|_| fail())?)
        .args(["launch", "auth-callback"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| fail())?;
    let started = (|| {
        let mut stdin = child.stdin.take().ok_or_else(fail)?;
        stdin.write_all(&bytes).map_err(|_| fail())?;
        drop(stdin);
        let stdout = child.stdout.take().ok_or_else(fail)?;
        let (send, recv) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut line = Vec::new();
            let _ = BufReader::new(stdout.take(17)).read_until(b'\n', &mut line);
            let _ = send.send(line);
        });
        let line = recv
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| fail())?;
        if line.len() > 16 || !line.ends_with(b"\n") {
            return Err(fail());
        }
        let text = std::str::from_utf8(&line[..line.len() - 1]).map_err(|_| fail())?;
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(fail());
        }
        let port = text.parse::<u16>().map_err(|_| fail())?;
        if port == 0 {
            return Err(fail());
        }
        Ok((child.id(), port))
    })();
    if started.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    started
}
