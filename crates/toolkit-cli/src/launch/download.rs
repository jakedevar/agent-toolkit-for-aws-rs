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
use std::time::{Duration, Instant};

use toolkit_aws::endpoint::HostResolver;
use toolkit_aws::stream::StreamingTransport;
use toolkit_aws::transport::{Headers, HttpRequest, Method, Timeouts};

use super::archive::extract::{extract, prepare_output, ExtractHook, LedgerStats};
use super::archive::plan::{plan, PlanStats};
use super::archive::zip_read::{read_archive, Profile, ReadStats};
use super::archive::{ArchiveLimits, MIB};
use super::clock::{random_hex, Random};
use super::http::{self, BodyError, Operation};
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

/// The Python's message when `GetLaunch` returns no `downloadUrl`.
pub const NO_URL_MESSAGE: &str =
    "Download URL not available yet. Ensure the launch execution has completed.";

/// The default destination under `temp_dir`:
/// `launch-snapshot-<id-safe>-<16 hex>`. The id keeps ASCII letters, digits,
/// `-` and `_` (anything else becomes `-`), at most 64 characters.
///
/// # Errors
/// The [`Random`] failure.
pub fn default_output(
    temp_dir: &Path,
    launch_id: &str,
    random: &dyn Random,
) -> Result<PathBuf, LaunchError> {
    let safe: String = launch_id
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let safe = if safe.is_empty() { "launch".to_string() } else { safe };
    Ok(temp_dir.join(format!("launch-snapshot-{safe}-{}", random_hex(random, 8)?)))
}

fn download_error(text: impl Into<String>) -> LaunchError {
    LaunchError::Download(text.into())
}

/// The size limit as the messages name it.
fn limit_text(cap: usize) -> String {
    if cap >= MIB as usize && cap % MIB as usize == 0 {
        format!("{} MiB", cap / MIB as usize)
    } else {
        format!("{cap} bytes")
    }
}

/// One bounded `GET` of the snapshot into memory (R-API-BODY's download
/// row): no retry, no redirect, the deadline inside the transport.
fn fetch(url: &str, deps: &DownloadDeps<'_>) -> Result<Vec<u8>, LaunchError> {
    let timeout = Duration::from_secs(DOWNLOAD_SECONDS);
    let request = HttpRequest {
        method: Method::Get,
        url: url.to_string(),
        headers: Headers::new(),
        body: Vec::new(),
        timeouts: Timeouts::new(timeout, timeout)
            .map_err(|_| download_error("The snapshot download could not be started."))?,
    };
    let response = http::collect_with_ceiling(
        deps.transport,
        &request,
        Operation::Download,
        deps.deadline,
        deps.download_cap,
    )
    .map_err(|error| {
        download_error(match error {
            BodyError::TooLarge => format!(
                "The snapshot download exceeds the {} limit.",
                limit_text(deps.download_cap)
            ),
            BodyError::Deadline => format!(
                "The snapshot download did not finish within {DOWNLOAD_SECONDS} seconds."
            ),
            BodyError::InvalidResponse => {
                "The snapshot download returned an invalid response.".to_string()
            }
            BodyError::Allocation => {
                "The snapshot download could not be held in memory.".to_string()
            }
            BodyError::Transport { .. } => {
                "The snapshot download failed: the connection could not be completed."
                    .to_string()
            }
        })
    })?;
    if !(200..300).contains(&response.status) {
        return Err(download_error(format!(
            "The snapshot download failed with HTTP status {}.",
            response.status
        )));
    }
    Ok(response.body)
}

/// Download, validate, extract and publish the launch's snapshot.
///
/// # Errors
/// Every refusal of R-AR-EXT; nothing is left at the destination.
pub fn download_snapshot(
    request: &DownloadRequest<'_>,
    deps: &mut DownloadDeps<'_>,
) -> Result<Downloaded, LaunchError> {
    // 1. The output checks, before any request.
    let output = match request.output {
        Some(path) => path.to_path_buf(),
        None => default_output(deps.temp_dir, request.launch_id, deps.random)?,
    };
    let target = prepare_output(&output)?;
    // 2. The URL.
    let Some(url) = (deps.download_url)()? else {
        return Err(download_error(NO_URL_MESSAGE));
    };
    // 3. The download, into memory only.
    let body = fetch(&url, deps)?;
    // 4. The whole validation and the plan, before anything is created.
    let archive = read_archive(
        &body,
        Profile::Snapshot,
        &deps.limits,
        &mut ReadStats::default(),
    )?;
    let plan = plan(&archive, deps.node_budget, &mut PlanStats::default())?;
    // 5-7. Staging, extraction, publication; cleanup on any failure.
    let extracted = extract(
        &target,
        &archive,
        &plan,
        deps.random,
        deps.hook,
        &mut LedgerStats::default(),
    )?;
    Ok(Downloaded {
        output_dir: output,
        files: extracted.files,
        directories: extracted.directories,
        bytes: extracted.bytes,
        region: request.region.to_string(),
    })
}

#[cfg(all(test, unix))]
mod tests_ext;
