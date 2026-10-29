use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "payload-hooks-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("notebooks")).unwrap();
        fs::write(
            path.join("template.py"),
            "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n",
        )
        .unwrap();
        Self(path)
    }
    fn invocation(&self) -> Invocation {
        Invocation {
            positionals: vec![self.0.join("template.py").display().to_string()],
            flags: vec!["--append".into()],
            values: vec![
                ("--mode".into(), "notebook".into()),
                ("--name".into(), "example".into()),
                ("--project-dir".into(), self.0.display().to_string()),
            ],
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
const SOURCE: &str =
    "{\"cells\":[{\"source\":[\"snapshot\"]}],\"metadata\":{},\"nbformat\":4,\"nbformat_minor\":4}";

#[cfg(unix)]
#[test]
fn ap_pub_04_05_faults_through_script_and_append_commands() {
    struct Fault(&'static str, i32);
    impl place::PublishHooks for Fault {
        fn fault(&mut self, stage: &str) -> Option<std::io::Error> {
            (stage == self.0).then(|| std::io::Error::from_raw_os_error(self.1))
        }
    }
    for mode in ["script", "notebook"] {
        for (stage, raw, expected) in [
            ("temp_create", 5, "publish_io"),
            ("write", 5, "publish_io"),
            ("sync", 5, "publish_io"),
            (
                "link",
                rustix::io::Errno::PERM.raw_os_error(),
                "publish_no_atomic_noreplace",
            ),
            (
                "link",
                rustix::io::Errno::OPNOTSUPP.raw_os_error(),
                "publish_no_atomic_noreplace",
            ),
        ] {
            let f = Fixture::new();
            let source = f.0.join("notebooks/example.ipynb");
            fs::write(&source, SOURCE).unwrap();
            let mut inv = f.invocation();
            if mode == "script" {
                inv.flags.clear();
                inv.values
                    .iter_mut()
                    .find(|(k, _)| k == "--mode")
                    .unwrap()
                    .1 = "script".into();
            }
            let mut sink = Vec::new();
            let error =
                command_with_hooks(&inv, &mut Output::new(&mut sink), &mut Fault(stage, raw))
                    .unwrap_err();
            assert_eq!(error.code(), expected, "{mode} {stage}");
            assert_eq!(fs::read_to_string(source).unwrap(), SOURCE);
            let dir = f.0.join(if mode == "script" {
                "scripts"
            } else {
                "notebooks"
            });
            assert!(fs::read_dir(dir).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("toolkit-render")));
            assert!(!f
                .0
                .join(if mode == "script" {
                    "scripts/01_example.py"
                } else {
                    "notebooks/example-01.ipynb"
                })
                .exists());
        }
    }
}
struct Hook {
    source: PathBuf,
    take_destination: bool,
}
impl place::PublishHooks for Hook {
    fn after_temp_written(&mut self, _: &Path, _: &Path) {
        if !self.take_destination {
            fs::write(&self.source, b"editor bytes").unwrap();
        }
    }
    fn before_commit(&mut self, _: &Path, dest: &Path) {
        if self.take_destination {
            fs::write(dest, b"competitor").unwrap();
        }
    }
}
#[test]
fn ap_app_03_taken_version_preserves_source_and_competitor() {
    let f = Fixture::new();
    let source = f.0.join("notebooks/example.ipynb");
    fs::write(&source, SOURCE).unwrap();
    let mut sink = Vec::new();
    let mut output = Output::new(&mut sink);
    let error = command_with_hooks(
        &f.invocation(),
        &mut output,
        &mut Hook {
            source: source.clone(),
            take_destination: true,
        },
    )
    .unwrap_err();
    assert_eq!(error.code(), "notebook_version_taken");
    assert_eq!(fs::read_to_string(source).unwrap(), SOURCE);
    assert_eq!(
        fs::read(f.0.join("notebooks/example-01.ipynb")).unwrap(),
        b"competitor"
    );
    assert!(fs::read_dir(f.0.join("notebooks"))
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("toolkit-render")));
}
#[test]
fn ap_app_04_source_edit_after_snapshot_keeps_both_versions() {
    let f = Fixture::new();
    let source = f.0.join("notebooks/example.ipynb");
    fs::write(&source, SOURCE).unwrap();
    let mut sink = Vec::new();
    let mut output = Output::new(&mut sink);
    command_with_hooks(
        &f.invocation(),
        &mut output,
        &mut Hook {
            source: source.clone(),
            take_destination: false,
        },
    )
    .unwrap();
    assert_eq!(fs::read(source).unwrap(), b"editor bytes");
    assert!(fs::read_to_string(f.0.join("notebooks/example-01.ipynb"))
        .unwrap()
        .contains("snapshot"));
    assert!(String::from_utf8(sink)
        .unwrap()
        .contains(&sha256_hex(SOURCE.as_bytes())));
}

#[test]
fn ap_app_05_06_rename_save_and_deletion_preserve_snapshot() {
    struct Edit {
        source: PathBuf,
        replace: bool,
    }
    impl place::PublishHooks for Edit {
        fn before_commit(&mut self, _: &Path, _: &Path) {
            if self.replace {
                let temporary = self.source.with_extension("editor");
                fs::write(&temporary, b"editor replacement").unwrap();
                fs::rename(temporary, &self.source).unwrap();
            } else {
                fs::remove_file(&self.source).unwrap();
            }
        }
    }
    for replace in [true, false] {
        let f = Fixture::new();
        let source = f.0.join("notebooks/example.ipynb");
        fs::write(&source, SOURCE).unwrap();
        let mut sink = Vec::new();
        let mut output = Output::new(&mut sink);
        command_with_hooks(
            &f.invocation(),
            &mut output,
            &mut Edit {
                source: source.clone(),
                replace,
            },
        )
        .unwrap();
        if replace {
            assert_eq!(fs::read(source).unwrap(), b"editor replacement");
        } else {
            assert!(!source.exists());
        }
        assert!(fs::read_to_string(f.0.join("notebooks/example-01.ipynb"))
            .unwrap()
            .contains("snapshot"));
    }
}
