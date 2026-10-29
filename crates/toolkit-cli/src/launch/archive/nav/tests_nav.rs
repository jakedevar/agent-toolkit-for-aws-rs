//! Direct Navigator coverage: each step's identity comparison alone stops a
//! walk into a real directory (not a symlink, which O_NOFOLLOW already
//! refuses) that differs from the recorded one.

use std::fs;

use super::*;
use crate::launch::archive::fsops::open_root;
use crate::launch::tests_support::Scratch;

/// `d`, `d/a` and `d/b`.
struct Three;

impl Tree for Three {
    fn parent(&self, node: u32) -> u32 {
        if node == 0 {
            ROOT
        } else {
            0
        }
    }

    fn depth(&self, node: u32) -> usize {
        if node == 0 {
            1
        } else {
            2
        }
    }

    fn name(&self, node: u32) -> CString {
        CString::new(["d", "a", "b"][node as usize]).unwrap()
    }
}

fn layout(label: &str) -> (Scratch, Navigator, Vec<Option<Ident>>) {
    let dir = Scratch::new(label);
    fs::create_dir_all(dir.join("d/a")).unwrap();
    fs::create_dir_all(dir.join("d/b")).unwrap();
    let mut nav = Navigator::new(open_root(dir.path()).unwrap());
    let mut idents = vec![None; 3];
    nav.goto(&Three, 2, &mut idents).unwrap();
    nav.goto(&Three, 1, &mut idents).unwrap();
    assert!(idents.iter().all(Option::is_some), "every node recorded");
    (dir, nav, idents)
}

#[test]
fn a_downward_step_into_another_real_directory_is_refused() {
    let (dir, mut nav, mut idents) = layout("nav-down");
    nav.goto(&Three, ROOT, &mut idents).unwrap();
    // A fresh `d/a` under a fresh `d`: same names, other identities.
    fs::rename(dir.join("d"), dir.join("d-away")).unwrap();
    fs::create_dir_all(dir.join("d/a")).unwrap();
    assert!(matches!(
        nav.goto(&Three, 1, &mut idents),
        Err(OpenError::Changed)
    ));
}

#[test]
fn an_upward_step_into_another_real_directory_is_refused() {
    let (dir, mut nav, mut idents) = layout("nav-up");
    // The held `d/a` and the real `d/b` move under `other`: `..` from the
    // held `a` is now `other`, and `b` below it is the recorded `b`, so only
    // the upward comparison with the recorded `d` can stop the step.
    fs::create_dir(dir.join("other")).unwrap();
    fs::rename(dir.join("d/a"), dir.join("other/a")).unwrap();
    fs::rename(dir.join("d/b"), dir.join("other/b")).unwrap();
    assert!(matches!(
        nav.goto(&Three, 2, &mut idents),
        Err(OpenError::Changed)
    ));
}
