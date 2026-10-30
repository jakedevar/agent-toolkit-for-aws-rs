//! `toolkit neptune seed --live` (S3 of the docgraph live design, #5515;
//! R-DG-01/02/03/05/07): the seeding statements of [`seed::run`] sent over
//! one signed Gremlin WebSocket ([`GremlinConnection`]).
//!
//! The caller has already checked, from argv only and before this module
//! runs, the live selection, `--confirm-wipe` (the ambient
//! `NEPTUNE_SEED_CONFIRM_WIPE` is ignored in live mode) and `--endpoint`.
//! Only then is the `--network-policy` file read and parsed
//! ([`network_policy::parse`]; consent-shaped keys are refused), the
//! endpoint matched to the policy host and the Gremlin port, and the signing
//! region resolved and matched to the policy, all before roots, credentials,
//! DNS or any socket. The connection is opened lazily by the first statement,
//! so the output order is the script's ("Connecting to Neptune at ..."
//! first). A failed statement ends the run: nothing is retried or resent.

use std::io::Read;
use std::time::{Duration, Instant};

use toolkit_aws::sdk::database::DatabaseEndpoint;
use toolkit_aws::sdk::gremlin::{GremlinConnection, PORT};
use toolkit_aws::sdk::roots::ValidatedRoots;
use toolkit_aws::sdk::source::{GuardedCredentials, ProviderHttp};
use toolkit_aws::sdk::{SdkRuntime, SigV4Signer};
use toolkit_aws::{AwsError, ProcessEnv};
use toolkit_core::{Json, ToolkitError};

use super::network_policy::{self, MAX_POLICY_BYTES};
use super::seed::{self, GremlinSession};
use crate::aurora::args::usage;
use crate::{Invocation, Output};

/// The whole invocation's deadline (R-DG-07).
pub(super) const INVOCATION_BUDGET: Duration = Duration::from_secs(15 * 60);
/// The budget of one operation: credentials, signing, DNS, connect, upgrade.
const OPERATION_BUDGET: Duration = Duration::from_secs(60);

fn failure(error: &AwsError) -> ToolkitError {
    ToolkitError::runtime("docgraph_seed", format!("seeding failed: {error}"))
}

/// The policy file, read with its size bound; neither its path nor its
/// content is echoed.
pub(super) fn read_policy(path: &str) -> Result<DatabaseEndpoint, ToolkitError> {
    let unreadable = || {
        ToolkitError::runtime(
            "docgraph_network_policy",
            "the network policy file could not be read",
        )
    };
    let file = std::fs::File::open(path).map_err(|_| unreadable())?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(MAX_POLICY_BYTES).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable())?;
    network_policy::parse(&bytes)
        .map_err(|error| ToolkitError::runtime("docgraph_network_policy", error.to_string()))
}

/// Connects on the first statement, then sends every statement on that one
/// connection.
struct LiveSession<'a> {
    connect: Option<Box<dyn FnOnce() -> Result<GremlinConnection, AwsError> + 'a>>,
    connection: Option<GremlinConnection>,
}

impl GremlinSession for LiveSession<'_> {
    fn submit(&mut self, request: &Json) -> Result<Vec<Json>, AwsError> {
        if self.connection.is_none() {
            let connect = self.connect.take().ok_or_else(|| {
                AwsError::connect(
                    "the Gremlin connection could not be opened; no statement was sent",
                )
            })?;
            self.connection = Some(connect()?);
        }
        let gremlin = request
            .get("gremlin")
            .and_then(Json::as_str)
            .ok_or_else(|| AwsError::invalid_config("request", "a statement has no text"))?;
        match self.connection.as_mut() {
            Some(connection) => connection.submit(gremlin, request.get("bindings")),
            None => Err(AwsError::connect("the Gremlin connection is not open")),
        }
    }
}

/// Run the live seed; consent and `--endpoint` are already checked.
pub(super) fn run(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let endpoint = invocation.value("--endpoint").unwrap_or_default();
    let path = invocation
        .value("--network-policy")
        .ok_or_else(|| usage("--network-policy is required with --live"))?;
    let policy = read_policy(path)?;
    if endpoint != policy.host() {
        return Err(usage("--endpoint must be the network policy's host"));
    }
    if policy.port() != PORT {
        return Err(ToolkitError::runtime(
            "docgraph_network_policy",
            "the network policy's port must be the Gremlin port 8182",
        ));
    }
    let region = network_policy::signing_region(&policy, invocation.value("--region"), &ProcessEnv)
        .map_err(|error| failure(&error))?;
    let roots = ValidatedRoots::from_process()
        .map_err(|failure| ToolkitError::runtime("docgraph_seed", failure.message()))?;
    let runtime = SdkRuntime::new().map_err(|error| failure(&error))?;
    let http = ProviderHttp::validated_bounded(&roots).map_err(|error| failure(&error))?;
    let credentials = GuardedCredentials::new(&runtime, &region, None, http);
    let invocation_deadline = Instant::now() + INVOCATION_BUDGET;

    let mut session = LiveSession {
        connect: Some(Box::new(|| {
            let deadline = (Instant::now() + OPERATION_BUDGET).min(invocation_deadline);
            let signer = SigV4Signer::new(credentials.until(deadline));
            GremlinConnection::connect_live(
                &runtime,
                &roots,
                policy,
                &signer,
                &region,
                deadline,
                invocation_deadline,
            )
        })),
        connection: None,
    };
    let mut lines: Vec<String> = Vec::new();
    let result = seed::run(&mut session, endpoint, true, &mut |text| {
        lines.push(text.to_string());
    });
    if let Some(connection) = session.connection.take() {
        connection.close();
    }
    for line in &lines {
        out.line(line)?;
    }
    result.map_err(|error| super::seed_failure(&error))
}
