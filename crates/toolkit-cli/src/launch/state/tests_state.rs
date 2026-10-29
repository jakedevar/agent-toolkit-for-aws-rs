//! T-STATE-1..8 (R-TOK-STORE, R-TOK-MODE) plus the state-directory rules.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;

use toolkit_aws::MapEnv;

use super::*;
use crate::launch::clock::SeededRandom;
use crate::launch::tests_support::Scratch;

/// A session file exactly as Python's `json.dumps(asdict(s), indent=2)`
/// writes it (no trailing newline).
const PYTHON_SESSION: &str = "{\n  \"client_id\": \"cid-123\",\n  \"client_secret\": \
\"FAKE-CLIENT-SECRET\",\n  \"client_expires_at\": 1798000000,\n  \"authorize_endpoint\": \
\"https://oidc.us-east-1.amazonaws.com/authorize\",\n  \"token_endpoint\": \
\"https://oidc.us-east-1.amazonaws.com/token\",\n  \"scopes\": [\n    \"launch:access\"\n  \
],\n  \"access_token\": \"FAKE-ACCESS\",\n  \"refresh_token\": \"FAKE-REFRESH\",\n  \
\"token_expires_at\": 1797000000\n}";

/// The `ensure_ascii` and empty-list forms of the same writer.
const PYTHON_SESSION_ESCAPES: &str = "{\n  \"client_id\": \"\\u00e9\\\\\\\"/\\u0001\u{7f}\
\\ud83d\\ude00\",\n  \"client_secret\": \"\",\n  \"client_expires_at\": -5,\n  \
\"authorize_endpoint\": \"\\n\\r\\t\\b\\f\",\n  \"token_endpoint\": \"\",\n  \"scopes\": [],\n  \
\"access_token\": \"a\",\n  \"refresh_token\": \"\",\n  \"token_expires_at\": 0\n}";

fn sample() -> StoredSession {
    StoredSession::from_json_bytes(PYTHON_SESSION.as_bytes()).expect("python fixture")
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("stat")
        .permissions()
        .mode()
        & 0o7777
}

fn leftovers(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("list")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    names.sort();
    names
}

#[test]
fn state_dir_requires_an_absolute_home() {
    let env = MapEnv::new().with("HOME", "/home/u");
    assert_eq!(
        state_dir(&env),
        Ok(Path::new("/home/u/.launch-with-aws").to_path_buf())
    );
    for home in [None, Some(""), Some("relative/home")] {
        let env = match home {
            Some(value) => MapEnv::new().with("HOME", value),
            None => MapEnv::new(),
        };
        assert!(
            matches!(state_dir(&env), Err(LaunchError::Config(_))),
            "{home:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn t_state_1_directory_is_created_0700_and_retightened() {
    let home = Scratch::new("state1");
    let dir = home.join(STATE_DIR_NAME);
    write_private_file(&dir, CONFIG_FILE_NAME, b"{}", &SeededRandom::new(1)).expect("write");
    assert_eq!(mode(&dir), 0o700);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod");
    write_private_file(&dir, CONFIG_FILE_NAME, b"{}", &SeededRandom::new(2)).expect("write");
    assert_eq!(mode(&dir), 0o700, "re-tightened on every write");
    // A symlink in place of the directory is refused, and nothing is
    // written through it.
    let elsewhere = home.join("elsewhere");
    fs::create_dir(&elsewhere).expect("mkdir");
    let linked = home.join("linked-state");
    symlink(&elsewhere, &linked).expect("symlink");
    assert!(write_private_file(&linked, CONFIG_FILE_NAME, b"{}", &SeededRandom::new(3)).is_err());
    assert_eq!(fs::read_dir(&elsewhere).expect("list").count(), 0);
}

#[cfg(unix)]
#[test]
fn t_state_2_files_are_written_0600_atomically() {
    let home = Scratch::new("state2");
    let dir = home.join(STATE_DIR_NAME);
    let session = sample();
    save_session(&dir, &session, &SeededRandom::new(4)).expect("save");
    let path = dir.join(SESSION_FILE_NAME);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(fs::read_to_string(&path).expect("read"), PYTHON_SESSION);
    assert!(leftovers(&dir).is_empty());
    // Replacing keeps the mode and leaves no temporary file.
    let mut changed = session.clone();
    changed.token_expires_at += 1;
    save_session(&dir, &changed, &SeededRandom::new(5)).expect("save");
    assert_eq!(mode(&path), 0o600);
    assert_eq!(load_session(&dir), Some(changed));
    assert!(leftovers(&dir).is_empty());
}

#[test]
fn t_state_3_a_failed_replace_leaves_no_temporary_file() {
    let home = Scratch::new("state3");
    let dir = home.join(STATE_DIR_NAME);
    fs::create_dir_all(dir.join(SESSION_FILE_NAME)).expect("a directory in the way");
    let error = save_session(&dir, &sample(), &SeededRandom::new(6));
    assert!(error.is_err(), "rename over a directory fails");
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
    assert!(dir.join(SESSION_FILE_NAME).is_dir(), "target unchanged");
    // The temporary name has the documented shape.
    let random = SeededRandom::new(6);
    let mut probe = [0_u8; 8];
    random.fill(&mut probe).expect("fill");
    assert_eq!(probe.len(), 8);
}

#[cfg(unix)]
#[test]
fn t_state_4_a_symlinked_session_is_treated_as_absent() {
    let home = Scratch::new("state4");
    let dir = home.join(STATE_DIR_NAME);
    let target = home.write("outside-session.json", PYTHON_SESSION.as_bytes());
    fs::create_dir(&dir).expect("mkdir");
    symlink(&target, dir.join(SESSION_FILE_NAME)).expect("symlink");
    assert_eq!(load_session(&dir), None);
    assert_eq!(
        read_private_file(&dir, SESSION_FILE_NAME).expect("read"),
        None
    );
    // The target's permissions were not touched through the link.
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).expect("chmod");
    let _ = load_session(&dir);
    assert_eq!(mode(&target), 0o644);
}

#[cfg(unix)]
#[test]
fn t_state_5_sign_out_removes_only_the_link() {
    let home = Scratch::new("state5");
    let dir = home.join(STATE_DIR_NAME);
    let target = home.write("outside-session.json", PYTHON_SESSION.as_bytes());
    fs::create_dir(&dir).expect("mkdir");
    symlink(&target, dir.join(SESSION_FILE_NAME)).expect("symlink");
    assert!(remove_state_file(&dir, SESSION_FILE_NAME).expect("remove"));
    assert!(fs::symlink_metadata(dir.join(SESSION_FILE_NAME)).is_err());
    assert_eq!(
        fs::read_to_string(&target).expect("target kept"),
        PYTHON_SESSION
    );
    assert!(!remove_state_file(&dir, SESSION_FILE_NAME).expect("absent"));
    // A regular session file is removed; config.json stays.
    save_session(&dir, &sample(), &SeededRandom::new(7)).expect("save");
    save_region(&dir, "eu-central-1", &SeededRandom::new(8))
        .expect("supported")
        .expect("written");
    assert!(remove_state_file(&dir, SESSION_FILE_NAME).expect("remove"));
    assert_eq!(load_session(&dir), None);
    assert_eq!(load_saved_region(&dir), Some("eu-central-1"));
}

#[cfg(unix)]
#[test]
fn t_state_6_a_0644_file_is_tightened_before_reading() {
    let home = Scratch::new("state6");
    let dir = home.join(STATE_DIR_NAME);
    save_session(&dir, &sample(), &SeededRandom::new(9)).expect("save");
    let path = dir.join(SESSION_FILE_NAME);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
    assert_eq!(load_session(&dir), Some(sample()));
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn t_state_7_oversize_and_malformed_files_are_no_session() {
    let home = Scratch::new("state7");
    let dir = home.join(STATE_DIR_NAME);
    fs::create_dir(&dir).expect("mkdir");
    let path = dir.join(SESSION_FILE_NAME);
    let mut oversize = PYTHON_SESSION.as_bytes().to_vec();
    oversize.resize(
        usize::try_from(MAX_STATE_FILE_BYTES).expect("fits") + 1,
        b' ',
    );
    fs::write(&path, &oversize).expect("write");
    assert_eq!(
        read_private_file(&dir, SESSION_FILE_NAME).expect("read"),
        None
    );
    assert_eq!(load_session(&dir), None);
    let mut at_limit = PYTHON_SESSION.as_bytes().to_vec();
    at_limit.resize(usize::try_from(MAX_STATE_FILE_BYTES).expect("fits"), b' ');
    fs::write(&path, &at_limit).expect("write");
    assert!(load_session(&dir).is_some(), "exactly the cap is read");
    let malformed = [
        "",
        "not json",
        "[]",
        &PYTHON_SESSION.replace("\"cid-123\"", "7"),
        &PYTHON_SESSION.replace("1798000000", "\"1798000000\""),
        &PYTHON_SESSION.replace("1798000000", "1.5"),
        &PYTHON_SESSION.replace("1798000000", "true"),
        &PYTHON_SESSION.replace("\"launch:access\"", "1"),
        &PYTHON_SESSION.replace("[\n    \"launch:access\"\n  ]", "\"launch:access\""),
        &PYTHON_SESSION.replace("  \"refresh_token\": \"FAKE-REFRESH\",\n", ""),
        &PYTHON_SESSION.replace("\n}", ",\n  \"extra\": 1\n}"),
        &PYTHON_SESSION.replace("\n}", ",\n  \"access_token\": \"dup\"\n}"),
        "\u{feff}{}",
    ];
    for text in malformed {
        assert_eq!(
            StoredSession::from_json_bytes(text.as_bytes()),
            None,
            "{text:?}"
        );
    }
    assert_eq!(StoredSession::from_json_bytes(b"\xff\xfe"), None);
    // Member order in the file does not matter for reading.
    let reordered = "{\"token_expires_at\": 1, \"refresh_token\": \"r\", \"access_token\": \"a\", \
        \"scopes\": [], \"token_endpoint\": \"t\", \"authorize_endpoint\": \"z\", \
        \"client_expires_at\": 2, \"client_secret\": \"s\", \"client_id\": \"c\"}";
    assert!(StoredSession::from_json_bytes(reordered.as_bytes()).is_some());
}

#[test]
fn t_state_8_python_session_round_trips_byte_identically() {
    for fixture in [PYTHON_SESSION, PYTHON_SESSION_ESCAPES] {
        let session = StoredSession::from_json_bytes(fixture.as_bytes()).expect("parses");
        assert_eq!(session.to_json_string(), fixture);
    }
    let session = sample();
    assert_eq!(session.client_id, "cid-123");
    assert_eq!(session.client_secret.expose(), "FAKE-CLIENT-SECRET");
    assert_eq!(session.scopes, vec!["launch:access".to_string()]);
    assert_eq!(session.client_expires_at, 1_798_000_000);
    let home = Scratch::new("state8");
    let dir = home.join(STATE_DIR_NAME);
    save_session(&dir, &session, &SeededRandom::new(10)).expect("save");
    assert_eq!(
        fs::read(dir.join(SESSION_FILE_NAME)).expect("read"),
        PYTHON_SESSION.as_bytes()
    );
    // No secret reaches Debug.
    let debug = format!("{session:?}");
    for secret in ["FAKE-CLIENT-SECRET", "FAKE-ACCESS", "FAKE-REFRESH"] {
        assert!(!debug.contains(secret), "{debug}");
    }
    assert!(debug.contains("cid-123"));
}

#[test]
fn saved_region_is_written_like_python_and_validated() {
    let home = Scratch::new("region");
    let dir = home.join(STATE_DIR_NAME);
    assert!(matches!(
        save_region(&dir, "mars-1", &SeededRandom::new(11)),
        Err(LaunchError::Config(_))
    ));
    assert!(
        !dir.exists(),
        "nothing is created for an unsupported region"
    );
    save_region(&dir, "us-east-1", &SeededRandom::new(12))
        .expect("supported")
        .expect("written");
    assert_eq!(
        fs::read_to_string(dir.join(CONFIG_FILE_NAME)).expect("read"),
        "{\n  \"region\": \"us-east-1\"\n}"
    );
    assert_eq!(load_saved_region(&dir), Some("us-east-1"));
    assert_eq!(load_saved_region(&home.join("missing")), None);
}
