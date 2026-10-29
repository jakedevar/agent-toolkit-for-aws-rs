use super::*;
use std::{fs, sync::atomic::{AtomicU64, Ordering}};

static SEQ: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("ap-pub-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&p).unwrap(); Self(p)
    }
    fn entries(&self) -> Vec<PathBuf> { fs::read_dir(&self.0).unwrap().map(|x| x.unwrap().path()).collect() }
}
impl Drop for Fixture { fn drop(&mut self) { fs::remove_dir_all(&self.0).unwrap(); } }
struct Hooks { point: &'static str, fault: &'static str, raw: i32, symlink: bool }
impl Hooks { fn new() -> Self { Self { point: "", fault: "", raw: 0, symlink: false } } fn compete(&self, point: &str, dest: &Path) { if self.point == point { if self.symlink { #[cfg(unix)] std::os::unix::fs::symlink("missing", dest).unwrap(); } else { fs::write(dest, b"competitor").unwrap(); } } } }
impl PublishHooks for Hooks {
    fn after_temp_written(&mut self, _: &Path, d: &Path) { self.compete("after_temp_written", d); }
    fn before_commit(&mut self, _: &Path, d: &Path) { self.compete("before_commit", d); }
    fn after_commit_before_unlink(&mut self, _: &Path, _: &Path) {}
    fn fault(&mut self, stage: &str) -> Option<io::Error> { (self.fault == stage).then(|| io::Error::from_raw_os_error(self.raw)) }
}

#[test]
fn ap_pub_01_02_competing_target_never_replaced() {
    for point in ["after_temp_written", "before_commit"] {
        let f = Fixture::new(); let mut h = Hooks::new(); h.point = point;
        let e = publish_new(&f.0, "x.ipynb", b"ours", &mut h).unwrap_err();
        assert_eq!(e.code, "destination_exists"); assert_eq!(fs::read(f.0.join("x.ipynb")).unwrap(), b"competitor"); assert_eq!(f.entries().len(), 1);
    }
}
#[test]
fn ap_pub_03_taken_script_number_is_reported_for_caller_retry() {
    let f = Fixture::new(); let mut h = Hooks::new(); h.point = "before_commit";
    assert_eq!(publish_new(&f.0, "script-01.py", b"ours", &mut h).unwrap_err().code, "destination_exists");
    let mut h = Hooks::new(); publish_new(&f.0, "script-02.py", b"ours", &mut h).unwrap();
    assert_eq!(fs::read(f.0.join("script-01.py")).unwrap(), b"competitor"); assert_eq!(fs::read(f.0.join("script-02.py")).unwrap(), b"ours");
}
#[test]
fn ap_pub_04_faults_remove_temp() {
    for stage in ["temp_create", "write", "sync"] { let f = Fixture::new(); let mut h = Hooks::new(); h.fault = stage; h.raw = 5;
        assert!(publish_new(&f.0, "x", b"ours", &mut h).is_err()); assert!(f.entries().is_empty(), "{stage}"); }
}
#[test]
fn ap_pub_05_no_hard_links_and_eexist() {
    for (raw, code) in [(1, "publish_no_atomic_noreplace"), (95, "publish_no_atomic_noreplace"), (17, "destination_exists")] {
        let f = Fixture::new(); let mut h = Hooks::new(); h.fault = "link"; h.raw = raw;
        assert_eq!(publish_new(&f.0, "x", b"ours", &mut h).unwrap_err().code, code); assert!(f.entries().is_empty());
    }
}
#[test]
fn ap_pub_06_unlink_failure_is_success_with_temp_path() {
    let f = Fixture::new(); let mut h = Hooks::new(); h.fault = "unlink"; h.raw = 5;
    let p = publish_new(&f.0, "x", b"ours", &mut h).unwrap();
    assert_eq!(fs::read(f.0.join("x")).unwrap(), b"ours"); assert_eq!(fs::read(p.leftover_temp.unwrap()).unwrap(), b"ours");
}
#[cfg(unix)]
#[test]
fn ap_pub_07_dangling_symlink_is_not_followed() {
    let f = Fixture::new(); let mut h = Hooks::new(); h.point = "before_commit"; h.symlink = true;
    assert_eq!(publish_new(&f.0, "x", b"ours", &mut h).unwrap_err().code, "destination_exists"); assert!(fs::symlink_metadata(f.0.join("x")).unwrap().file_type().is_symlink()); assert_eq!(f.entries().len(), 1);
}
#[test]
fn ap_pub_15_platform_refusal() { assert_eq!(unsupported_platform().code, "publish_unsupported_platform"); }
