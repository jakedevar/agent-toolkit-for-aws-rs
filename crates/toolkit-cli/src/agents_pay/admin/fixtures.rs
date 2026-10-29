//! Public, harmless admin fixtures (G1b, G2): in memory, or confined to
//! their own temporary directory. None of them opens a socket, reads the
//! process environment or touches a path outside its temporary root.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};

use toolkit_core::Json;

use super::{AdminApi, AdminConnector, AdminEnv, AdminFs, AdminRandom, FileMeta, Terminal};
use crate::agents_pay::agentcore::{FakePaymentsApi, PaymentError, PaymentsApi};

/// A fixed environment.
#[derive(Default)]
pub struct FakeEnv(Vec<(String, String)>);

impl FakeEnv {
    /// No variables.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// The same environment with `name=value` (the last one wins).
    #[must_use]
    pub fn with(mut self, name: &str, value: &str) -> Self {
        self.0.retain(|(n, _)| n != name);
        self.0.push((name.to_owned(), value.to_owned()));
        self
    }
}

impl AdminEnv for FakeEnv {
    fn var(&self, name: &str) -> Option<String> {
        self.0
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    }
    fn names(&self) -> Vec<String> {
        self.0.iter().map(|(n, _)| n.clone()).collect()
    }
}

/// A scripted terminal: a TTY with queued answers (EOF after the last), or
/// headless.
pub struct FakeTerminal {
    tty: bool,
    lines: RefCell<VecDeque<String>>,
    reads: Cell<usize>,
}

impl FakeTerminal {
    /// No terminal: `is_tty` is false and every read is EOF.
    #[must_use]
    pub fn headless() -> Self {
        Self {
            tty: false,
            lines: RefCell::new(VecDeque::new()),
            reads: Cell::new(0),
        }
    }
    /// A terminal that answers `lines` in order, then EOF.
    #[must_use]
    pub fn tty(lines: &[&str]) -> Self {
        Self {
            tty: true,
            lines: RefCell::new(lines.iter().map(|l| (*l).to_owned()).collect()),
            reads: Cell::new(0),
        }
    }
    /// How many lines were read (EOF included).
    #[must_use]
    pub fn reads(&self) -> usize {
        self.reads.get()
    }
}

impl Terminal for FakeTerminal {
    fn is_tty(&self) -> bool {
        self.tty
    }
    fn read_line(&self) -> Option<String> {
        self.reads.set(self.reads.get() + 1);
        self.lines.borrow_mut().pop_front()
    }
}

/// Deterministic "random" bytes: a counter.
#[derive(Default)]
pub struct CountingRandom(Cell<u8>);

impl CountingRandom {
    /// Starts at zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl AdminRandom for CountingRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), super::NoRandomness> {
        for byte in buf {
            *byte = self.0.get();
            self.0.set(self.0.get().wrapping_add(1));
        }
        Ok(())
    }
}

/// A temporary root holding `home/` and the working directory `work/`.
/// Every operation resolves its path against `work/`, normalizes it
/// lexically and refuses (records, and treats as absent or unwritable) any
/// path outside the root. The directory is removed on drop.
pub struct TempAdminFs {
    root: PathBuf,
    refused: RefCell<Vec<PathBuf>>,
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl TempAdminFs {
    /// A fresh root.
    ///
    /// # Panics
    /// If the temporary directory cannot be created.
    #[must_use]
    pub fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "agents-pay-admin-{label}-{}-{nanos}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("home")).expect("temporary home");
        std::fs::create_dir_all(root.join("work")).expect("temporary working directory");
        Self {
            root,
            refused: RefCell::new(Vec::new()),
        }
    }
    /// The temporary root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// The fixture home (`account_home`).
    #[must_use]
    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }
    /// Every path refused for being outside the root.
    #[must_use]
    pub fn refused(&self) -> Vec<PathBuf> {
        self.refused.borrow().clone()
    }
    /// Write a file under the root (relative to it), creating parents.
    ///
    /// # Panics
    /// If the file cannot be written.
    #[allow(clippy::must_use_candidate)] // A writer; callers may ignore the path.
    pub fn put(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.root.join(relative);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("parent");
        std::fs::write(&path, text).expect("fixture file");
        path
    }
    fn resolve(&self, path: &Path) -> Option<PathBuf> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd().join(path)
        };
        let normal = normalize(&absolute);
        if normal.starts_with(&self.root) {
            Some(normal)
        } else {
            self.refused.borrow_mut().push(path.to_path_buf());
            None
        }
    }
}

impl Drop for TempAdminFs {
    fn drop(&mut self) {
        if self.root.starts_with(std::env::temp_dir()) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

impl AdminFs for TempAdminFs {
    fn cwd(&self) -> PathBuf {
        self.root.join("work")
    }
    fn account_home(&self) -> Option<PathBuf> {
        Some(self.home())
    }
    fn exists(&self, path: &Path) -> bool {
        self.resolve(path).is_some_and(|p| p.exists())
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.resolve(path).is_some_and(|p| p.is_dir())
    }
    fn read(&self, path: &Path) -> Option<String> {
        super::deployed::read_bounded(&self.resolve(path)?)
    }
    fn lstat(&self, path: &Path) -> Option<FileMeta> {
        lstat(&self.resolve(path)?)
    }
    fn uid(&self) -> u32 {
        current_uid()
    }
    fn write_config(&self, path: &Path, content: &str) -> Result<(), String> {
        let Some(resolved) = self.resolve(path) else {
            return Err("TempAdminFs refuses a path outside its root.".to_owned());
        };
        super::write::atomic_write_0600(&resolved, content)
            .map_err(|_| super::write_failure(path))
    }
    fn load_runtime_policy(&self, path: &Path) -> Result<Json, String> {
        let Some(resolved) = self.resolve(path) else {
            return Err("TempAdminFs refuses a path outside its root.".to_owned());
        };
        crate::agents_pay::config::load_config(&resolved)
    }
}

/// `lstat` of a real path.
#[cfg(unix)]
#[must_use]
pub fn lstat(path: &Path) -> Option<FileMeta> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).ok().map(|m| FileMeta {
        mode: m.mode(),
        uid: m.uid(),
    })
}

/// No `lstat` on this platform.
#[cfg(not(unix))]
#[must_use]
pub fn lstat(_path: &Path) -> Option<FileMeta> {
    None
}

/// The real uid.
#[cfg(unix)]
#[must_use]
pub fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// No uid on this platform.
#[cfg(not(unix))]
#[must_use]
pub fn current_uid() -> u32 {
    0
}

/// A recording admin client over [`FakePaymentsApi`] (fixture account
/// `111111111111` only): it counts opens, STS checks and creations.
pub struct FakeAdmin {
    api: FakePaymentsApi,
    sts_error: Option<PaymentError>,
    create_error: Option<PaymentError>,
    sts: Cell<usize>,
    opened: RefCell<Vec<Option<String>>>,
}

impl FakeAdmin {
    /// A client that answers creations with `api`'s scripted responses.
    #[must_use]
    pub fn new(api: FakePaymentsApi) -> Self {
        Self {
            api,
            sts_error: None,
            create_error: None,
            sts: Cell::new(0),
            opened: RefCell::new(Vec::new()),
        }
    }
    /// The same client whose STS check fails with `error`.
    #[must_use]
    pub fn sts_failing(mut self, error: PaymentError) -> Self {
        self.sts_error = Some(error);
        self
    }
    /// The same client whose creations fail with `error` (after recording).
    #[must_use]
    pub fn create_failing(mut self, error: PaymentError) -> Self {
        self.create_error = Some(error);
        self
    }
    /// The inner fake (its admin call log).
    #[must_use]
    pub fn api(&self) -> &FakePaymentsApi {
        &self.api
    }
    /// STS checks so far.
    #[must_use]
    pub fn sts_calls(&self) -> usize {
        self.sts.get()
    }
    /// The region of every open.
    #[must_use]
    pub fn opened(&self) -> Vec<Option<String>> {
        self.opened.borrow().clone()
    }
    /// Every recorded call: opens, STS checks and creations.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.opened.borrow().len() + self.sts.get() + self.api.admin_calls().len()
    }
}

impl AdminConnector for FakeAdmin {
    fn open<'s>(&'s self, region: Option<&str>) -> Result<Box<dyn AdminApi + 's>, String> {
        self.opened.borrow_mut().push(region.map(str::to_owned));
        Ok(Box::new(self))
    }
}

impl AdminApi for &FakeAdmin {
    fn sts_check(&self) -> Result<(), PaymentError> {
        self.sts.set(self.sts.get() + 1);
        self.sts_error.map_or(Ok(()), Err)
    }
    fn create_instrument(
        &self,
        manager: &str,
        user: &str,
        connector: &str,
        network: &str,
        email: &str,
        token: &str,
    ) -> Result<Json, PaymentError> {
        let result = self
            .api
            .create_instrument(manager, user, connector, network, email, token);
        self.create_error.map_or(result, Err)
    }
    fn create_session(
        &self,
        manager: &str,
        user: &str,
        budget: &str,
        expiry: i64,
        token: &str,
    ) -> Result<Json, PaymentError> {
        let result = self
            .api
            .create_session(manager, user, budget, expiry, token);
        self.create_error.map_or(result, Err)
    }
}
