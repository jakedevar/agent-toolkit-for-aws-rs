//! Production wiring for `documentdb wa-review --live` (#5507).
//!
//! The command completes argv-only live selection and cluster/region/time
//! validation before entering here. No input file, environment variable or
//! configuration can select this path. AWS service requests use the public
//! transport and one immutable R-ROOTS set shared with the bounded provider
//! HTTP client. `GuardedCredentials` implements `DeadlineSigner` using
//! `until(deadline)`; the toolkit remains the sole signer and retry owner.

use toolkit_aws::sdk::roots::ValidatedRoots;
use toolkit_aws::sdk::source::{GuardedCredentials, ProviderHttp};
use toolkit_aws::sdk::{SdkRuntime, SdkTransport};
use toolkit_aws::{AwsError, FullJitter, ThreadSleeper};
use toolkit_core::ToolkitError;

use super::checks::Findings;
use super::live::LiveAwsClient;

fn failure(error: &AwsError) -> ToolkitError {
    ToolkitError::runtime("docgraph_review", error.to_string())
}

/// Construct the production adapter only after the command's preflight.
/// Tests exercise dispatch through its offline callback, never this entry.
pub(super) fn run_live_review(
    region: &str,
    cluster_id: &str,
    now: i64,
) -> Result<Findings, ToolkitError> {
    let runtime = SdkRuntime::new().map_err(|error| failure(&error))?;
    let roots = ValidatedRoots::from_process()
        .map_err(|error| ToolkitError::runtime("docgraph_review", error.message()))?;
    let transport = SdkTransport::new(&runtime, &roots).map_err(|error| failure(&error))?;
    let http = ProviderHttp::validated_bounded(&roots).map_err(|error| failure(&error))?;
    let credentials = GuardedCredentials::new(&runtime, region, None, http);
    let sleeper = ThreadSleeper;
    let jitter = FullJitter::new();
    let mut client = LiveAwsClient::new(region, &transport, &credentials, &sleeper, &jitter)
        .map_err(|error| failure(&error))?;
    Ok(super::infra::run_infra_checks(&mut client, cluster_id, now))
}
