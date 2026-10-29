//! Unit tests of the descriptor-relative primitives.

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::process::Command;

use super::*;
use crate::launch::tests_support::Scratch;

fn c(name: &str) -> CString {
    CString::new(name).expect("no NUL")
}

#[test]
fn root_listing_and_classification() {
    let tree = Scratch::new("fsops1");
    tree.write("b.txt", b"bb");
    tree.write("a/inner", b"i");
    symlink(tree.join("b.txt"), tree.join("link")).expect("symlink");
    assert!(Command::new("mkfifo")
        .arg(tree.join("fifo"))
        .status()
        .expect("mkfifo")
        .success());
    let root = open_root(tree.path()).expect("root");
    let meta = fs::metadata(tree.path()).expect("stat");
    use std::os::unix::fs::MetadataExt;
    assert_eq!(root.identity(), (meta.dev(), meta.ino()));
    let names: Vec<String> = list_names(&root)
        .expect("list")
        .into_iter()
        .map(|name| name.into_string().expect("utf-8"))
        .collect();
    assert_eq!(names, vec!["a", "b.txt", "fifo", "link"]);
    assert_eq!(
        stat_at(&root, &c("a")).expect("stat").kind,
        EntryType::Directory
    );
    let file = stat_at(&root, &c("b.txt")).expect("stat");
    assert_eq!((file.kind, file.size), (EntryType::Regular, 2));
    assert_eq!(
        stat_at(&root, &c("link")).expect("stat").kind,
        EntryType::Symlink
    );
    assert_eq!(
        stat_at(&root, &c("fifo")).expect("stat").kind,
        EntryType::Special
    );
    assert!(
        open_root(&tree.join("b.txt")).is_err(),
        "a file is not a root"
    );
}

#[test]
fn opens_check_type_and_identity() {
    let tree = Scratch::new("fsops2");
    tree.write("f", b"data");
    tree.write("d/x", b"x");
    symlink(tree.join("f"), tree.join("lf")).expect("symlink");
    symlink(tree.join("d"), tree.join("ld")).expect("symlink");
    let root = open_root(tree.path()).expect("root");
    let f = stat_at(&root, &c("f")).expect("stat");
    let d = stat_at(&root, &c("d")).expect("stat");
    assert!(open_file_at(&root, &c("f"), &f).is_ok());
    assert!(open_dir_at(&root, &c("d"), &d).is_ok());
    assert!(matches!(
        open_file_at(&root, &c("lf"), &f),
        Err(OpenError::Changed)
    ));
    assert!(matches!(
        open_dir_at(&root, &c("ld"), &d),
        Err(OpenError::Changed)
    ));
    assert!(matches!(
        open_file_at(&root, &c("gone"), &f),
        Err(OpenError::Changed)
    ));
    assert!(matches!(
        open_file_at(&root, &c("d"), &d),
        Err(OpenError::Changed)
    ));
    let other = EntryStat {
        ino: f.ino + 1_000_000,
        ..f
    };
    assert!(matches!(
        open_file_at(&root, &c("f"), &other),
        Err(OpenError::Changed)
    ));
}

#[test]
fn state_open_refuses_links_and_special_files() {
    let tree = Scratch::new("fsops3");
    let file = tree.write("f", b"abc");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).expect("chmod");
    symlink(&file, tree.join("link")).expect("symlink");
    assert!(Command::new("mkfifo")
        .arg(tree.join("fifo"))
        .status()
        .expect("mkfifo")
        .success());
    let (_, size, mode) = open_regular_nofollow(&file)
        .expect("open")
        .expect("regular");
    assert_eq!((size, mode & 0o777), (3, 0o640));
    assert!(open_regular_nofollow(&tree.join("link"))
        .expect("open")
        .is_none());
    assert!(open_regular_nofollow(&tree.join("missing"))
        .expect("open")
        .is_none());
    assert!(open_regular_nofollow(&tree.join("fifo"))
        .expect("no block")
        .is_none());
    assert!(open_regular_nofollow(tree.path()).expect("dir").is_none());
}
