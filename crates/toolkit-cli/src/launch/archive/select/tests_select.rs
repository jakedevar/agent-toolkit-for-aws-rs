//! T-AR-SEL-1..10 (selection) and T-AR-OPEN-1..8 (race-resistant rooted
//! traversal) on the real file system. Every race case swaps entries
//! through the [`WalkHook`] seam and runs under a 2 s watchdog; a refused
//! build returns no archive, so no upload request can follow (the L5
//! `create-launch` tests re-run these through the transport fake).

use std::cell::RefCell;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::launch::archive::zip_read::{decode_entry, read_archive, Profile, ReadStats};
use crate::launch::clock::SeededRandom;
use crate::launch::tests_support::{test_git, Scratch};

/// Build `path` (absolute) with `hook`, returning the archive or the error.
fn build_with(
    path: &str,
    hook: &dyn WalkHook,
    home: Option<&Path>,
) -> Result<BuiltArchive, LaunchError> {
    build_budget(path, hook, home, SkippedBudget::PRODUCTION)
}

/// [`build_with`] under the skipped-secret budget `skipped`.
fn build_budget(
    path: &str,
    hook: &dyn WalkHook,
    home: Option<&Path>,
    skipped: SkippedBudget,
) -> Result<BuiltArchive, LaunchError> {
    let temp = Scratch::new("sel-git-tmp");
    let git = test_git();
    let random = SeededRandom::new(5);
    let cwd = std::env::temp_dir();
    let options = BuildOptions {
        git: &git,
        hook,
        limits: ArchiveLimits::PRODUCTION,
        skipped,
        home,
        cwd: &cwd,
        temp_parent: temp.path(),
        random: &random,
    };
    build_archive(path, "app", &options)
}

fn build(path: &Path) -> Result<BuiltArchive, LaunchError> {
    build_with(path.to_str().expect("utf-8"), &NoHook, None)
}

/// `(name, content)` of every entry, in archive order.
fn contents(built: &BuiltArchive) -> Vec<(String, Vec<u8>)> {
    let archive = read_archive(
        &built.bytes,
        Profile::Upload,
        &ArchiveLimits::PRODUCTION,
        &mut ReadStats::default(),
    )
    .expect("valid archive");
    archive
        .entries()
        .iter()
        .map(|entry| {
            let mut data = Vec::new();
            decode_entry(&archive, entry, &mut |piece| {
                data.extend_from_slice(piece);
                Ok(())
            })
            .expect("decodes");
            (archive.name(entry).to_string(), data)
        })
        .collect()
}

fn names(built: &BuiltArchive) -> Vec<String> {
    contents(built).into_iter().map(|(name, _)| name).collect()
}

fn changed(rel: &str) -> LaunchError {
    LaunchError::Archive(format!(
        "A file changed while the archive was being built: {rel}. Re-run create-launch."
    ))
}

fn mkfifo(path: &Path) {
    let status = Command::new("mkfifo").arg(path).status().expect("mkfifo");
    assert!(status.success());
}

/// Run `f` on another thread; fail the test if it takes more than 2 s (a
/// blocking open of a FIFO would hang forever).
fn within_2s<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(f());
    });
    receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("the walk blocked (watchdog: 2 s)")
}

/// A Unix socket named `name` in `dir`. Socket paths are limited to about
/// 108 bytes, so a long scratch path binds through a short symlink.
fn bind_socket(dir: &Path, name: &str) -> std::os::unix::net::UnixListener {
    use std::os::unix::net::UnixListener;
    if let Ok(listener) = UnixListener::bind(dir.join(name)) {
        return listener;
    }
    let mut nonce = [0_u8; 4];
    getrandom::fill(&mut nonce).expect("csprng");
    let link = PathBuf::from(format!(
        "/tmp/tl-{}-{}",
        std::process::id(),
        u32::from_le_bytes(nonce)
    ));
    symlink(dir, &link).expect("short link to the scratch directory");
    let listener = UnixListener::bind(link.join(name)).expect("socket");
    let _ = fs::remove_file(&link);
    listener
}

/// A hook that runs `action` once, when `event` reaches `rel`.
struct SwapHook {
    on_open: bool,
    rel: String,
    action: RefCell<Option<Box<dyn FnOnce()>>>,
    opened: RefCell<Vec<String>>,
}

impl SwapHook {
    fn at_open(rel: &str, action: impl FnOnce() + 'static) -> Self {
        Self {
            on_open: true,
            rel: rel.to_string(),
            action: RefCell::new(Some(Box::new(action))),
            opened: RefCell::new(Vec::new()),
        }
    }

    fn at_enter(rel: &str, action: impl FnOnce() + 'static) -> Self {
        Self {
            on_open: false,
            ..Self::at_open(rel, action)
        }
    }

    fn fire(&self, rel: &str, on_open: bool) {
        if on_open == self.on_open && rel == self.rel {
            if let Some(action) = self.action.borrow_mut().take() {
                action();
            }
        }
    }
}

impl WalkHook for SwapHook {
    fn before_open(&self, rel: &str) {
        self.opened.borrow_mut().push(rel.to_string());
        self.fire(rel, true);
    }
    fn before_enter(&self, rel: &str) {
        self.fire(rel, false);
    }
}

fn tree() -> Scratch {
    let tree = Scratch::new("sel-tree");
    tree.write("a.txt", b"alpha");
    tree.write("sub/b.txt", b"bravo");
    tree.write("sub/deeper/c.txt", b"charlie");
    tree.write("z.txt", b"zulu");
    tree
}

// ------------------------------------------------------------ T-AR-SEL

#[test]
fn t_ar_sel_1_skip_directories() {
    let tree = Scratch::new("sel1");
    tree.write("keep.txt", b"k");
    for dir in SKIP_DIRS.iter().filter(|dir| **dir != ".git") {
        tree.write(&format!("{dir}/inner.txt"), b"skipped");
        tree.write(&format!("nested/{dir}/inner.txt"), b"skipped");
    }
    // Exact, case-sensitive names (Python `d not in _SKIP_DIRS`).
    tree.write("Build/kept.txt", b"k");
    tree.write("node_modules2/kept.txt", b"k");
    let built = build(tree.path()).expect("build");
    assert_eq!(
        names(&built),
        vec![
            "app/Build/kept.txt",
            "app/keep.txt",
            "app/node_modules2/kept.txt"
        ]
    );
    assert!(SKIP_DIRS.contains(&".git"));
    assert_eq!(SKIP_DIRS.len(), 28);
}

#[test]
fn t_ar_sel_2_skip_files_case_insensitively() {
    let tree = Scratch::new("sel2");
    tree.write("keep.txt", b"k");
    for (index, name) in SKIP_FILES.iter().enumerate() {
        tree.write(&format!("d{index}/{name}"), b"secret");
        tree.write(&format!("u{index}/{}", name.to_uppercase()), b"secret");
    }
    let built = build(tree.path()).expect("build");
    assert_eq!(names(&built), vec!["app/keep.txt"]);
    assert_eq!(built.skipped_secrets.total(), 2 * SKIP_FILES.len() as u64);
    assert_eq!(built.skipped_secrets.omitted(), 0);
    assert_eq!(built.files, 1);
}

#[test]
fn t_ar_sel_3_env_suffix_and_prefix_rules() {
    for name in [
        ".env.anything",
        ".ENV.prod",
        "server.PEM",
        "x.key",
        "a.p12",
        "b.pfx",
        "c.jks",
        "d.keystore",
        "id_rsa",
        "id_rsa.pub",
        "ID_ED25519_backup",
        "id_ecdsa",
        "id_dsa.old",
    ] {
        assert!(is_secret_file(name), "{name}");
    }
    for name in [
        "env.txt",
        ".envrc",
        "pem",
        "keys.txt",
        "my_id_rsa",
        "key",
        "x.keys",
        "credentials.txt",
    ] {
        assert!(!is_secret_file(name), "{name}");
    }
    let tree = Scratch::new("sel3");
    tree.write("server.pem", b"-----BEGIN");
    tree.write(".envrc", b"use nix");
    let built = build(tree.path()).expect("build");
    assert_eq!(names(&built), vec!["app/.envrc"]);
    assert_eq!(
        built.skipped_secrets.paths().collect::<Vec<_>>(),
        vec!["server.pem"]
    );
}

#[test]
fn t_ar_sel_3b_skipped_secret_ledger_is_bounded_5135() {
    // One allowed file and many secret files across directories: the
    // archive stays tiny and the skipped-path ledger keeps only its budget,
    // counting the rest (#5135, #5082).
    let tree = Scratch::new("sel3b");
    tree.write("keep.txt", b"k");
    for dir in 0..40 {
        for file in 0..3 {
            tree.write(&format!("d{dir:02}/.env.{file}"), b"secret");
        }
    }
    let budget = SkippedBudget {
        max_paths: 5,
        max_bytes: 200,
    };
    let built = build_budget(tree.path().to_str().expect("utf-8"), &NoHook, None, budget)
        .expect("skipping never fails the build");
    assert_eq!(names(&built), vec!["app/keep.txt"]);
    assert_eq!(built.files, 1);
    let ledger = &built.skipped_secrets;
    assert_eq!(ledger.total(), 120);
    assert_eq!(
        ledger.paths().collect::<Vec<_>>(),
        vec![
            "d00/.env.0",
            "d00/.env.1",
            "d00/.env.2",
            "d01/.env.0",
            "d01/.env.1"
        ]
    );
    assert_eq!(ledger.omitted(), 115);
    assert!(
        ledger.capacity() <= budget.max_bytes,
        "{}",
        ledger.capacity()
    );
    assert!(ledger
        .summary()
        .expect("summary")
        .ends_with("d01/.env.1, and 115 more not listed"));
    // A byte budget smaller than the first path keeps none of them.
    let built = build_budget(
        tree.path().to_str().expect("utf-8"),
        &NoHook,
        None,
        SkippedBudget {
            max_paths: 5,
            max_bytes: 4,
        },
    )
    .expect("build");
    assert_eq!(built.skipped_secrets.retained(), 0);
    assert_eq!(built.skipped_secrets.omitted(), 120);
    assert_eq!(built.skipped_secrets.capacity(), 0);
}

#[test]
fn t_ar_sel_4_symlinked_files_are_skipped() {
    let tree = tree();
    let outside = Scratch::new("sel4-outside");
    let secret = outside.write("secret.txt", b"OUTSIDE-SECRET");
    symlink(&secret, tree.join("link-out.txt")).expect("symlink");
    symlink(tree.join("a.txt"), tree.join("link-in.txt")).expect("symlink");
    symlink(tree.join("missing"), tree.join("dangling")).expect("symlink");
    let built = build(tree.path()).expect("build");
    assert_eq!(
        names(&built),
        vec![
            "app/a.txt",
            "app/sub/b.txt",
            "app/sub/deeper/c.txt",
            "app/z.txt"
        ]
    );
    assert!(!built
        .bytes
        .windows(14)
        .any(|window| window == b"OUTSIDE-SECRET"));
}

#[test]
fn t_ar_sel_5_symlinked_directories_are_not_entered() {
    let tree = tree();
    let outside = Scratch::new("sel5-outside");
    outside.write("inner/secret.txt", b"OUTSIDE");
    symlink(outside.join("inner"), tree.join("linked-dir")).expect("symlink");
    symlink(tree.join("sub"), tree.join("loop")).expect("symlink");
    let built = build(tree.path()).expect("build");
    assert_eq!(
        names(&built),
        vec![
            "app/a.txt",
            "app/sub/b.txt",
            "app/sub/deeper/c.txt",
            "app/z.txt"
        ]
    );
}

#[test]
fn t_ar_sel_6_a_fifo_is_skipped_without_blocking() {
    let tree = tree();
    mkfifo(&tree.join("pipe"));
    mkfifo(&tree.join("sub/pipe2"));
    let path = tree.path().to_path_buf();
    let built = within_2s(move || build(&path).map(|built| names(&built)));
    assert_eq!(
        built.expect("build"),
        vec![
            "app/a.txt",
            "app/sub/b.txt",
            "app/sub/deeper/c.txt",
            "app/z.txt"
        ]
    );
}

#[test]
fn t_ar_sel_7_non_utf8_names_are_refused() {
    let tree = tree();
    let bad = tree.path().join(OsStr::from_bytes(b"bad\xff.txt"));
    fs::write(&bad, b"x").expect("write");
    assert_eq!(
        build(tree.path()).map(|_| ()),
        Err(LaunchError::Archive(
            "Archive cannot include a file whose name is not valid UTF-8: bad\u{fffd}.txt"
                .to_string()
        ))
    );
    // A non-UTF-8 name that is a symlink is skipped before it is decoded.
    fs::remove_file(&bad).expect("rm");
    symlink(tree.join("a.txt"), &bad).expect("symlink");
    assert!(build(tree.path()).is_ok());
}

#[test]
fn t_ar_sel_8_order_is_deterministic_by_name_bytes() {
    let tree = Scratch::new("sel8");
    // `B` beside `b` is a case pair: Upload keeps Python parity and accepts it
    // (decision 1; #5504 PM ruling), so the ordering fixture keeps it.
    for name in ["b", "a.txt", "a/x", "B", "a-b", "\u{e9}", "_u", "0"] {
        tree.write(name, name.as_bytes());
    }
    let first = build(tree.path()).expect("build");
    // Per directory, names in byte order; a directory's entries follow its name.
    assert_eq!(
        names(&first),
        vec![
            "app/0",
            "app/B",
            "app/_u",
            "app/a/x",
            "app/a-b",
            "app/a.txt",
            "app/b",
            "app/\u{e9}"
        ]
    );
    let second = build(tree.path()).expect("build");
    assert_eq!(first.bytes, second.bytes, "byte-identical rebuild");
    // Explicit parity: the case pair `B`/`b` is built, not refused.
    let listed = names(&first);
    assert!(listed.contains(&"app/B".to_string()) && listed.contains(&"app/b".to_string()));
}

#[test]
fn t_ar_sel_9_tilde_expansion_and_relative_paths() {
    let home = Scratch::new("sel9-home");
    home.write("proj/main.rs", b"fn main() {}");
    let built = build_with("~/proj", &NoHook, Some(home.path())).expect("~ expands");
    assert_eq!(names(&built), vec!["app/main.rs"]);
    let built = build_with("~/proj/", &NoHook, Some(home.path())).expect("trailing /");
    assert_eq!(names(&built), vec!["app/main.rs"]);
    // `~user` is a literal path (recorded divergence), relative to the cwd.
    assert!(build_with("~nobody-here/proj", &NoHook, Some(home.path())).is_err());
    assert_eq!(
        absolute_root("~/a/../b/./c", Some(Path::new("/h")), Path::new("/cwd")),
        PathBuf::from("/h/b/c")
    );
    assert_eq!(
        absolute_root("rel/x", None, Path::new("/cwd")),
        PathBuf::from("/cwd/rel/x")
    );
    assert_eq!(
        absolute_root("/../x", None, Path::new("/cwd")),
        PathBuf::from("/x")
    );
}

#[test]
fn t_ar_sel_10_not_a_directory_and_no_files() {
    let tree = tree();
    let file = tree.join("a.txt");
    let file_text = file.to_str().expect("utf-8").to_string();
    assert_eq!(
        build(&file).map(|_| ()),
        Err(LaunchError::Archive(format!(
            "Not a directory: {file_text}"
        )))
    );
    let missing = tree.join("missing");
    assert_eq!(
        build(&missing).map(|_| ()),
        Err(LaunchError::Archive(format!(
            "Not a directory: {}",
            missing.to_str().expect("utf-8")
        )))
    );
    let empty = Scratch::new("sel10-empty");
    empty.write("node_modules/x.js", b"skipped");
    empty.write(".env", b"secret");
    let empty_text = empty.path().to_str().expect("utf-8").to_string();
    assert_eq!(
        build(empty.path()).map(|_| ()),
        Err(LaunchError::Archive(format!(
            "No files found under {empty_text}"
        )))
    );
    // A credential-shaped path is shown redacted.
    let result = build_with("/nonexistent/AKIAIOSFODNN7EXAMPLE", &NoHook, None);
    let Err(LaunchError::Archive(text)) = result else {
        panic!("refused");
    };
    assert!(text.starts_with("Not a directory: "), "{text}");
    assert!(!text.contains("AKIAIOSFODNN7EXAMPLE"), "{text}");
}

// ----------------------------------------------------------- T-AR-OPEN

#[test]
fn t_ar_open_1_file_swapped_for_a_fifo() {
    let tree = tree();
    let path = tree.path().to_path_buf();
    let result = within_2s(move || {
        let target = path.join("sub/b.txt");
        let hook = SwapHook::at_open("sub/b.txt", move || {
            fs::remove_file(&target).expect("rm");
            mkfifo(&target);
        });
        build_with(path.to_str().expect("utf-8"), &hook, None).map(|_| ())
    });
    assert_eq!(result, Err(changed("sub/b.txt")));
}

#[test]
fn t_ar_open_2_file_swapped_for_a_same_sized_outside_symlink() {
    let tree = tree();
    let outside = Scratch::new("open2-outside");
    let secret = outside.write("secret", b"SECRT");
    assert_eq!(
        fs::metadata(&secret).expect("stat").len(),
        5,
        "same size as alpha"
    );
    let target = tree.join("a.txt");
    let hook = SwapHook::at_open("a.txt", move || {
        fs::remove_file(&target).expect("rm");
        symlink(&secret, &target).expect("symlink");
    });
    let result = build_with(tree.path().to_str().expect("utf-8"), &hook, None);
    let error = result.map(|_| ()).expect_err("refused");
    assert_eq!(error, changed("a.txt"));
    assert!(!error.message().contains("SECRT"));
}

/// R-AR-OPEN `open_file_at` identity (#5504): a different regular inode of the
/// same size under the walked name is refused. The old inode stays alive
/// (renamed away, not unlinked) so its number cannot be reused by the new file.
#[test]
fn t_ar_open_2b_same_sized_regular_file_with_another_inode_is_refused() {
    let tree = tree();
    let keep_alive = Scratch::new("open2b-old-inode");
    let target = tree.join("a.txt");
    let old = keep_alive.join("a.txt-original");
    let hook = SwapHook::at_open("a.txt", move || {
        fs::rename(&target, &old).expect("move the original inode away");
        fs::write(&target, b"SECRT").expect("same-sized regular replacement");
        assert_eq!(
            fs::metadata(&target).expect("stat").len(),
            5,
            "same size as alpha"
        );
    });
    let result = build_with(tree.path().to_str().expect("utf-8"), &hook, None);
    let error = result.map(|_| ()).expect_err("refused");
    assert_eq!(error, changed("a.txt"));
    assert!(!error.message().contains("SECRT"));
    assert_eq!(
        fs::read(keep_alive.join("a.txt-original")).expect("old inode kept"),
        b"alpha"
    );
}

#[test]
fn t_ar_open_3_ancestor_swapped_for_a_symlink_to_a_lookalike_directory() {
    let tree = tree();
    let outside = Scratch::new("open3-outside");
    outside.write("b.txt", b"OUTSIDE");
    outside.write("deeper/c.txt", b"OUTSIDE");
    let sub = tree.join("sub");
    let moved = tree.join("sub-moved-away");
    let lookalike = outside.path().to_path_buf();
    let hook = SwapHook::at_enter("sub", move || {
        fs::rename(&sub, &moved).expect("move");
        symlink(&lookalike, &sub).expect("symlink");
    });
    let result = build_with(tree.path().to_str().expect("utf-8"), &hook, None);
    assert_eq!(result.map(|_| ()), Err(changed("sub")));
}

#[test]
fn t_ar_open_4_directory_replaced_by_another_directory() {
    let tree = tree();
    let sub = tree.join("sub");
    let moved = tree.join("sub-old");
    let hook = SwapHook::at_enter("sub", move || {
        fs::rename(&sub, &moved).expect("move");
        fs::create_dir(&sub).expect("new dir");
        fs::write(sub.join("b.txt"), b"bravo").expect("same content");
    });
    let result = build_with(tree.path().to_str().expect("utf-8"), &hook, None);
    assert_eq!(result.map(|_| ()), Err(changed("sub")));
}

#[test]
fn t_ar_open_5_file_grown_or_shrunk_between_walk_and_read() {
    for grow in [true, false] {
        let tree = tree();
        let target = tree.join("z.txt");
        let hook = SwapHook::at_open("z.txt", move || {
            let contents: &[u8] = if grow { b"zulu-and-more" } else { b"zu" };
            fs::write(&target, contents).expect("rewrite in place");
        });
        let result = build_with(tree.path().to_str().expect("utf-8"), &hook, None);
        assert_eq!(result.map(|_| ()), Err(changed("z.txt")), "grow {grow}");
    }
}

#[test]
fn t_ar_open_6_fifo_and_socket_at_walk_time_are_never_opened() {
    let tree = tree();
    mkfifo(&tree.join("fifo"));
    let _listener = bind_socket(tree.path(), "sock");
    let path = tree.path().to_path_buf();
    let (names_seen, opened) = within_2s(move || {
        let hook = SwapHook::at_open("never", || {});
        let built = build_with(path.to_str().expect("utf-8"), &hook, None).expect("build");
        let opened = hook.opened.borrow().clone();
        (names(&built), opened)
    });
    assert_eq!(
        names_seen,
        vec![
            "app/a.txt",
            "app/sub/b.txt",
            "app/sub/deeper/c.txt",
            "app/z.txt"
        ]
    );
    assert_eq!(
        opened,
        vec!["a.txt", "sub/b.txt", "sub/deeper/c.txt", "z.txt"]
    );
}

#[test]
fn t_ar_open_7_ordinary_tree_byte_exact_in_name_order() {
    let tree = tree();
    let tool = tree.write("sub/run.sh", b"#!/bin/sh\n");
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).expect("chmod");
    let built = build(tree.path()).expect("build");
    assert_eq!(
        contents(&built),
        vec![
            ("app/a.txt".to_string(), b"alpha".to_vec()),
            ("app/sub/b.txt".to_string(), b"bravo".to_vec()),
            ("app/sub/deeper/c.txt".to_string(), b"charlie".to_vec()),
            ("app/sub/run.sh".to_string(), b"#!/bin/sh\n".to_vec()),
            ("app/z.txt".to_string(), b"zulu".to_vec()),
        ]
    );
    assert_eq!(built.files, 5);
    let archive = read_archive(
        &built.bytes,
        Profile::Upload,
        &ArchiveLimits::PRODUCTION,
        &mut ReadStats::default(),
    )
    .expect("upload profile");
    assert_eq!(archive.entries()[3].unix_mode, Some(0o100_755));
    assert_eq!(archive.entries()[0].unix_mode, Some(0o100_644));
}

#[test]
fn t_ar_open_8_a_symlinked_root_is_followed() {
    let tree = tree();
    let links = Scratch::new("open8-links");
    let root_link = links.join("root-link");
    symlink(tree.path(), &root_link).expect("symlink");
    let built = build(&root_link).expect("the root symlink is followed (os.path.isdir parity)");
    assert_eq!(
        names(&built),
        vec![
            "app/a.txt",
            "app/sub/b.txt",
            "app/sub/deeper/c.txt",
            "app/z.txt"
        ]
    );
}
