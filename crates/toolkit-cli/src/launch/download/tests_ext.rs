//! `download-snapshot` end to end over `FakeStreamingTransport` and a fixed
//! resolver: T-PLAN-7 (planner refusals after the download, zero creation
//! calls) and T-EXT-1..14 (R-AR-EXT), plus T-STAGE-2's "no request" half.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use toolkit_aws::testing::{FakeChunk, FakeStreamingTransport, FixedResolver};

use super::*;
use crate::launch::archive::extract::NoExtractHook;
use crate::launch::archive::fsops::creation_calls;
use crate::launch::archive::plan::{budget_message, MAX_SNAPSHOT_NODES};
use crate::launch::archive::tests_snapshot::{names, partials};
use crate::launch::archive::tests_zipfx::{Fx, FxEntry};
use crate::launch::clock::SeededRandom;
use crate::launch::http::DOWNLOAD_CAP;
use crate::launch::tests_support::Scratch;

/// A presigned-style URL whose query carries a fake signature that must
/// never appear in any message.
pub const URL: &str =
    "https://snapshots.s3.us-east-1.amazonaws.com/l-1/snapshot.zip?X-Amz-Signature=FAKE_SIG_9f3a";

pub fn resolver() -> FixedResolver {
    FixedResolver::new().answer_any(&["52.216.10.20"])
}

pub fn ok_transport(body: &[u8]) -> FakeStreamingTransport {
    let length = body.len().to_string();
    FakeStreamingTransport::new().respond(
        200,
        &[("content-length", length.as_str())],
        body.chunks(64 * 1024).map(FakeChunk::data).collect(),
    )
}

/// The knobs a case changes.
pub struct Knobs<'a> {
    pub url: Option<&'a str>,
    pub budget: usize,
    pub cap: usize,
    pub hook: &'a mut dyn ExtractHook,
}

impl Default for Knobs<'_> {
    fn default() -> Self {
        Self {
            url: Some(URL),
            budget: MAX_SNAPSHOT_NODES,
            cap: DOWNLOAD_CAP,
            hook: Box::leak(Box::new(NoExtractHook)),
        }
    }
}

/// Run `download-snapshot` to `output` (or the default under `temp`).
pub fn run(
    output: Option<&Path>,
    temp: &Path,
    transport: &dyn StreamingTransport,
    knobs: Knobs<'_>,
) -> Result<Downloaded, LaunchError> {
    let url = knobs.url.map(str::to_string);
    let source = move || Ok(url.clone());
    let resolver = resolver();
    let random = SeededRandom::new(11);
    let mut deps = DownloadDeps {
        download_url: &source,
        transport,
        resolver: &resolver,
        random: &random,
        hook: knobs.hook,
        temp_dir: temp,
        limits: ArchiveLimits::PRODUCTION,
        node_budget: knobs.budget,
        download_cap: knobs.cap,
        deadline: Instant::now() + Duration::from_secs(DOWNLOAD_SECONDS),
    };
    download_snapshot(
        &DownloadRequest {
            launch_id: "l-1",
            output,
            region: "us-east-1",
        },
        &mut deps,
    )
}

/// Nothing at `out`, nothing staged next to it.
#[track_caller]
pub fn assert_nothing_created(parent: &Path, out: &Path) {
    assert!(std::fs::symlink_metadata(out).is_err(), "{out:?} exists");
    assert!(partials(parent).is_empty(), "{:?}", names(parent));
}

fn chain(top: &str, depth: usize) -> String {
    let mut name = top.to_string();
    for _ in 0..depth {
        name.push_str("/a");
    }
    name.push_str("/f");
    name
}

#[test]
fn t_plan_7_planner_refusals_after_the_download_create_nothing() {
    let injected: Vec<FxEntry> = (0..3)
        .map(|index| FxEntry::stored(&chain(&format!("t{index}"), 39), b"x"))
        .collect();
    let production: Vec<FxEntry> = (0..101)
        .map(|index| FxEntry::stored(&chain(&format!("u{index}"), 1990), b"x"))
        .collect();
    for (entries, budget) in [(injected, 100), (production, MAX_SNAPSHOT_NODES)] {
        let bytes = Fx::of(entries).build().bytes;
        let scratch = Scratch::new("plan7");
        let out = scratch.join("out");
        let transport = ok_transport(&bytes);
        let before = creation_calls();
        let result = run(
            Some(&out),
            scratch.path(),
            &transport,
            Knobs {
                budget,
                ..Knobs::default()
            },
        );
        assert_eq!(result, Err(LaunchError::Archive(budget_message(budget))));
        assert_eq!(transport.requests().len(), 1, "refused after the download");
        assert_eq!(creation_calls(), before, "zero mkdirat and openat(O_CREAT)");
        assert_nothing_created(scratch.path(), &out);
        assert!(names(scratch.path()).is_empty());
    }
}

/// Where the default destination goes, for the cases that use it.
pub fn default_outputs(temp: &Path) -> Vec<PathBuf> {
    names(temp)
        .into_iter()
        .filter(|name| name.starts_with("launch-snapshot-"))
        .map(|name| temp.join(name))
        .collect()
}
