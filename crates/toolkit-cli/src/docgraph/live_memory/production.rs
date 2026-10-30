//! Production assembly; tests use Backend and StreamingTransport injections.
//! Consent was checked by memory_command before any input or policy read.
use super::{
    admit, execute, memory, object, preflight, text, wire, Backend, Duration, Instant, Json, Route,
    INVALID, OPERATION_BUDGET, UNKNOWN,
};
use crate::{Invocation, Output};
use toolkit_aws::sdk::database::{DatabaseBounds, DatabaseEndpoint};
use toolkit_aws::sdk::gremlin::GremlinConnection;
use toolkit_aws::sdk::roots::ValidatedRoots;
use toolkit_aws::sdk::source::{GuardedCredentials, ProviderHttp};
use toolkit_aws::sdk::{SdkRuntime, SdkTransport, SigV4Signer};
use toolkit_aws::{resolve_region, ProcessEnv};
use toolkit_core::ToolkitError;

fn failure(message: impl Into<String>) -> ToolkitError {
    ToolkitError::runtime("docgraph_memory", message)
}
struct OneRequest(toolkit_aws::Credentials);
impl toolkit_aws::CredentialsProvider for OneRequest {
    fn name(&self) -> &'static str {
        "docgraph-operation"
    }
    fn load(&self) -> Result<toolkit_aws::Credentials, toolkit_aws::AwsError> {
        Ok(self.0.clone())
    }
}
struct LiveBackend<'a> {
    runtime: &'a SdkRuntime,
    roots: &'a ValidatedRoots,
    credentials: GuardedCredentials,
    transport: Option<SdkTransport>,
    connection: Option<GremlinConnection>,
    region: String,
    policy: Option<DatabaseEndpoint>,
    invocation_deadline: Instant,
}
impl Backend for LiveBackend<'_> {
    fn call(&mut self, route: Route, request: &Json, deadline: Instant) -> Result<Json, String> {
        if route == Route::Gremlin {
            if self.connection.is_none() {
                let signer = SigV4Signer::new(self.credentials.until(deadline));
                self.connection = Some(
                    GremlinConnection::connect_live(
                        self.runtime,
                        self.roots,
                        self.policy.clone().ok_or(INVALID)?,
                        &signer,
                        &self.region,
                        deadline,
                        self.invocation_deadline,
                    )
                    .map_err(|_| {
                        "the Gremlin connection could not be opened; no statement was sent"
                    })?,
                );
            }
            return self
                .connection
                .as_mut()
                .ok_or(INVALID)?
                .submit(text(request, "gremlin")?, request.get("bindings"))
                .map(|results| Json::object(vec![("results", Json::Array(results))]))
                .map_err(|_| UNKNOWN.into());
        }
        let resolved = self
            .credentials
            .resolve_required_until(deadline)
            .map_err(|_| "required memory credentials could not be resolved; nothing was sent")?;
        let wire = wire::encode(
            route,
            request,
            &self.region,
            self.policy.as_ref(),
            deadline.saturating_duration_since(Instant::now()),
        )?;
        let signer = SigV4Signer::new(OneRequest(resolved));
        wire::exchange(
            self.transport.as_ref().ok_or(INVALID)?,
            &signer,
            route,
            wire,
            &self.region,
            deadline,
        )
    }
}
impl Drop for LiveBackend<'_> {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            connection.close();
        }
    }
}
fn input(path: &str) -> Result<Json, ToolkitError> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| failure("the memory input file could not be read"))?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("the memory input file could not be read"))?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(failure("the memory input file is too large"));
    }
    let document = toolkit_core::parse_json_with_limits(
        std::str::from_utf8(&bytes).map_err(|_| failure("the memory input is not valid JSON"))?,
        &toolkit_core::JsonLimits {
            max_depth: 64,
            max_nodes: 1_000_000,
        },
    )
    .map_err(|_| failure("the memory input is not valid bounded JSON"))?;
    object(&document).map_err(failure)?;
    Ok(document)
}
pub(in crate::docgraph) fn run(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    // Starts before file parsing, client assembly and credentials.
    let invocation_deadline = Instant::now() + Duration::from_secs(15 * 60);
    let op = invocation
        .value("--operation")
        .ok_or_else(|| crate::aurora::args::usage("--operation is required"))?;
    let input = input(
        invocation
            .value("--input")
            .ok_or_else(|| crate::aurora::args::usage("--input is required"))?,
    )?;
    let requests = memory::plan(op, &input).map_err(failure)?;
    let needs_policy = requests
        .iter()
        .any(|r| r.get("gremlin").is_some() || r.get("graphIdentifier").is_some());
    let policy = if needs_policy {
        let path = invocation.value("--network-policy").ok_or_else(|| {
            crate::aurora::args::usage("--network-policy is required for live graph queries")
        })?;
        Some(super::super::live_seed::read_policy(path)?)
    } else {
        // A policy for another backend must never silently widen admission.
        if invocation.value("--network-policy").is_some() {
            return Err(failure(
                "--network-policy is only accepted for live graph queries",
            ));
        }
        None
    };
    preflight(&requests, policy.as_ref()).map_err(failure)?;
    let region = if let Some(p) = &policy {
        super::super::network_policy::signing_region(p, invocation.value("--region"), &ProcessEnv)
    } else {
        resolve_region(invocation.value("--region"), &ProcessEnv).map(|r| r.name().to_string())
    }
    .map_err(|_| failure("the signing region is invalid or differs from the network policy"))?;
    // Admit/encode every initial request before roots or provider assembly.
    for request in &requests {
        let route = admit(request, policy.as_ref()).map_err(failure)?;
        if route != Route::Gremlin {
            wire::encode(route, request, &region, policy.as_ref(), OPERATION_BUDGET)
                .map_err(failure)?;
        }
    }
    let roots = ValidatedRoots::from_process().map_err(|e| failure(e.message()))?;
    let runtime =
        SdkRuntime::new().map_err(|_| failure("the memory runtime could not be created"))?;
    let provider = ProviderHttp::validated_bounded(&roots)
        .map_err(|_| failure("the credential transport could not be created"))?;
    let credentials = GuardedCredentials::new(&runtime, &region, None, provider);
    let route = admit(&requests[0], policy.as_ref()).map_err(failure)?;
    let transport = match route {
        Route::Gremlin => None,
        Route::Analytics => Some(
            SdkTransport::for_database(
                &runtime,
                &roots,
                policy.clone().ok_or_else(|| failure(INVALID))?,
                DatabaseBounds::default(),
            )
            .map_err(|_| failure("the graph transport could not be created"))?,
        ),
        Route::Create | Route::Dynamo => Some(
            SdkTransport::for_docgraph_service(
                &runtime,
                &roots,
                if route == Route::Create {
                    "neptune-graph"
                } else {
                    "dynamodb"
                },
                &region,
                DatabaseBounds::default(),
            )
            .map_err(|_| failure("the service transport could not be created"))?,
        ),
    };
    let mut backend = LiveBackend {
        runtime: &runtime,
        roots: &roots,
        credentials,
        transport,
        connection: None,
        region,
        policy: policy.clone(),
        invocation_deadline,
    };
    let result = execute(
        &mut backend,
        op,
        &input,
        &requests,
        policy.as_ref(),
        invocation_deadline,
    )
    .map_err(failure)?;
    out.line(&result.to_compact_string())
}
