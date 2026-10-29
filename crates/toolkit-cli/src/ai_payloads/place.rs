//! Atomic publication of rendered artifacts.
use std::{io, path::{Path, PathBuf}};

pub(crate) trait PublishHooks {
    fn after_temp_written(&mut self, _temp: &Path, _dest: &Path) {}
    fn before_commit(&mut self, _temp: &Path, _dest: &Path) {}
    fn after_commit_before_unlink(&mut self, _temp: &Path, _dest: &Path) {}
    fn fault(&mut self, _stage: &str) -> Option<io::Error> { None }
}

#[derive(Debug)]
pub(crate) struct PublishError { pub code: &'static str }
#[derive(Debug)]
pub(crate) struct Published { pub leftover_temp: Option<PathBuf> }
pub(crate) fn unsupported_platform() -> PublishError { PublishError { code: "publish_unsupported_platform" } }
pub(crate) fn publish_new(_dir: &Path, _name: &str, _bytes: &[u8], _hooks: &mut dyn PublishHooks) -> Result<Published, PublishError> { Err(PublishError { code: "unimplemented" }) }

#[cfg(test)]
#[path = "place_tests.rs"]
mod tests;
