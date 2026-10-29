//! `launch download-snapshot` (R-AR-EXT, Q1): the snapshot download into
//! memory and its safe extraction.
//!
//! Order: the output checks of R-AR-STAGE before any request; the download
//! URL from `GetLaunch include=download_url`; the URL rules (https, no
//! userinfo, host allowlist `.amazonaws.com`/`.api.aws`, public addresses);
//! one bounded `GET` through the R-API-BODY collector (no redirect, 300 s
//! deadline, 500 MiB cap); the whole Snapshot-profile validation and the
//! materialisation plan; then staging, extraction and exclusive publication.
//! The archive only ever exists in memory: no temporary archive file. The URL
//! is never part of an error message.

use std::path::{Path, PathBuf};
use std::time::Instant;

use toolkit_aws::endpoint::HostResolver;
use toolkit_aws::stream::StreamingTransport;

use super::archive::extract::ExtractHook;
use super::archive::ArchiveLimits;
use super::clock::Random;
use super::LaunchError;

/// The download's operation deadline (R-API-BODY).
pub const DOWNLOAD_SECONDS: u64 = 300;

/// What `download-snapshot` was asked.
#[derive(Debug, Clone, Copy)]
pub struct DownloadRequest<'a> {
    /// The launch id.
    pub launch_id: &'a str,
    /// `--output`, or `None` for the default destination.
    pub output: Option<&'a Path>,
    /// The resolved region, echoed in the output.
    pub region: &'a str,
}

/// The seams of [`download_snapshot`].
pub struct DownloadDeps<'a> {
    /// `GetLaunch include=download_url`: `None` when the launch has no URL.
    pub download_url: &'a dyn Fn() -> Result<Option<String>, LaunchError>,
    /// The bounded streaming transport.
    pub transport: &'a dyn StreamingTransport,
    /// DNS for the public-address guard.
    pub resolver: &'a dyn HostResolver,
    /// Staging and default-destination names.
    pub random: &'a dyn Random,
    /// The R-AR-STAGE race seam.
    pub hook: &'a mut dyn ExtractHook,
    /// The default destination's parent (`std::env::temp_dir()`).
    pub temp_dir: &'a Path,
    /// Archive limits.
    pub limits: ArchiveLimits,
    /// The R-AR-PLAN node budget.
    pub node_budget: usize,
    /// The download body cap (500 MiB; tests inject small caps, #5082).
    pub download_cap: usize,
    /// The absolute download deadline.
    pub deadline: Instant,
}

/// The command's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    /// Where the snapshot was published.
    pub output_dir: PathBuf,
    /// Files created.
    pub files: usize,
    /// Directories created.
    pub directories: usize,
    /// Bytes written.
    pub bytes: u64,
    /// The region.
    pub region: String,
}

/// Download, validate, extract and publish the launch's snapshot.
///
/// # Errors
/// Every refusal of R-AR-EXT; nothing is left at the destination.
pub fn download_snapshot(
    request: &DownloadRequest<'_>,
    deps: &mut DownloadDeps<'_>,
) -> Result<Downloaded, LaunchError> {
    let _ = (request, deps);
    todo!("R-AR-EXT")
}

#[cfg(all(test, unix))]
mod tests_ext;
