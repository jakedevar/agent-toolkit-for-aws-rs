//! The `toolkit mcp-proxy` command (#4413 S3): argument checks, the start-up
//! order, the wiring of S1 (roots, stdin isolation, guarded credentials,
//! `SigV4`, the HTTPS transport) and S2 (the protocol core), and the exit
//! status.
//!
//! Start-up order (design 4.3, R-ROOTS; 4.4.1, R-URL):
//!
//! 1. arguments (usage errors, exit 2, with the usage text);
//! 2. the signing scope (pure; refusal exit 2);
//! 3. R-ROOTS, once, before stdin is read and before any connector, provider
//!    or DNS lookup (refusal exit 2, the fixed TLS line);
//! 4. D2: stdin moves to a private descriptor and children get a null stdin.
//!    Where that mechanism does not exist (non-Unix), the proxy runs, but a
//!    selected profile that could run a `credential_process` fails closed
//!    per request (`guard_credential_process`);
//! 5. the endpoint host must resolve to public addresses only (exit 2);
//! 6. the transport and the guarded credentials are built from the startup
//!    environment and files only (R-PROV), then the protocol core runs.
//!
//! Every refusal after the arguments is one `toolkit mcp-proxy: <fixed text>`
//! line on stderr, nothing on stdout, exit 2. The proxy exits 0 when stdin
//! ends (after the session `DELETE`) and 1 when stdout fails. stdout carries
//! only the protocol core's JSON-RPC lines; diagnostics go only through the
//! allowlisted [`Diag`] sink at the `--log-level` threshold. No `tracing`
//! subscriber or `log` logger is ever installed (R-LOG).
//!
//! The process's stdout and stderr are written through private duplicates of
//! descriptors 1 and 2: `main` holds the standard handles' locks for the whole
//! run, and the proxy writes from its own threads.

use std::fs::File;
use std::io::{LineWriter, Read, Write};
use std::sync::Arc;

use toolkit_aws::diag::Diag;
use toolkit_aws::{Env, McpTarget, SigningScope, StreamingTransport};
use toolkit_core::ToolkitError;

use super::auth::Auth;
use super::cli::{self, ProxyArgs};
use super::clock::Clock;
use super::proxy::{Exit, Proxy};
use super::scope::{metadata_region, proxy_config, signing_scope};
use crate::registry::{Slice, NEW_RANK};
use crate::{CommandSpec, Invocation, Output};

/// The diagnostics component name.
pub const COMPONENT: &str = "mcp-proxy";

/// Exit status of every refused start.
pub const STARTUP_REFUSAL: u8 = 2;

/// The slice registration.
pub(crate) const SLICE: Slice = Slice {
    name: "mcp-proxy",
    commands: &[CommandSpec {
        path: &["mcp-proxy"],
        flags: cli::FLAGS,
        value_flags: cli::VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: cli::REPEATABLE_FLAGS,
        max_positionals: 1,
        passthrough: false,
        handler: mcp_proxy_command,
    }],
    synopsis: r"  toolkit mcp-proxy ENDPOINT [--service NAME] [--region REGION] [--profile NAME]
      [--metadata KEY=VALUE]... [--header NAME=VALUE]... [--skip-auth] [--read-only]
      [--retries N] [--timeout S] [--connect-timeout S] [--read-timeout S]
      [--write-timeout S] [--tool-timeout S] [--log-level LEVEL]
",
    description: r"  mcp-proxy     Bridge MCP stdio (newline-delimited JSON-RPC) to the MCP
                Streamable HTTP endpoint ENDPOINT (https, an *.api.aws or
                *.amazonaws.com host, no query), signing each request with
                AWS SigV4 from the local credential chain. --skip-auth sends
                unsigned only when no credential source is configured at all.
                --metadata is added to params._meta; --header values are signed
                and never logged; --read-only hides tools without
                readOnlyHint (a convenience filter, not authorization).
                SSL_CERT_FILE/SSL_CERT_DIR, when non-empty, replace the
                platform roots. Needs a build with --features aws-sdk. See
                docs/migration/mcp-proxy-s3.md.
",
    exit_notes: r"  mcp-proxy exits 0 when stdin ends, 1 when stdout fails and 2 for a refused
  start (arguments, endpoint, TLS roots, stdin isolation, or no aws-sdk).
",
    doc: "docs/migration/mcp-proxy-s3.md",
    synopsis_rank: NEW_RANK,
    description_rank: NEW_RANK,
};

/// Router entry point.
///
/// # Errors
/// A usage error for refused arguments; every later outcome is an exit status.
pub fn mcp_proxy_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let args = cli::parse(invocation)?;
    let status = serve(&args, &mut production_backend());
    output.set_exit_status(status);
    Ok(())
}

/// What [`Backend::build`] returns: the MCP transport and the credential seam.
pub type Built = (Arc<dyn StreamingTransport + Send + Sync>, Arc<dyn Auth>);

/// What the start-up steps and the protocol core run over. Production is
/// [`SdkBackend`] (or [`NoSdkBackend`] without the `aws-sdk` feature); tests
/// inject fakes.
pub trait Backend {
    /// The environment the scope, metadata and credentials read.
    fn env(&self) -> &dyn Env;
    /// R-ROOTS (step 3); the refusal text.
    ///
    /// # Errors
    /// The fixed refusal text.
    fn validate_roots(&mut self) -> Result<(), String>;
    /// D2 (step 4): the MCP input and whether children get a null stdin.
    ///
    /// # Errors
    /// The fixed refusal text.
    fn stdin(&mut self) -> Result<(Box<dyn Read + Send>, bool), String>;
    /// Step 5.
    ///
    /// # Errors
    /// The fixed refusal text.
    fn check_target(&self, target: &McpTarget) -> Result<(), String>;
    /// The selected profile's region, for the `AWS_REGION` metadata.
    fn profile_region(&self, profile: Option<&str>) -> Option<String>;
    /// Step 6: the transport and the credential seam.
    ///
    /// # Errors
    /// The fixed refusal text.
    fn build(
        &mut self,
        args: &ProxyArgs,
        scope: SigningScope,
        null_stdin: bool,
        diag: Diag,
    ) -> Result<Built, String>;
    /// The clock of the protocol core.
    fn clock(&self) -> Arc<dyn Clock>;
    /// The process stdout (only the JSON-RPC writer uses it).
    ///
    /// # Errors
    /// The fixed refusal text.
    fn stdout(&mut self) -> Result<Box<dyn Write + Send>, String>;
    /// A writer to the process stderr.
    fn stderr(&mut self) -> Box<dyn Write + Send>;
}

/// Run the proxy: the start-up steps, then the protocol core until stdin
/// ends or stdout fails. Returns the exit status.
pub fn serve(args: &ProxyArgs, backend: &mut dyn Backend) -> u8 {
    let mut stderr = backend.stderr();
    let mut refuse = |message: &str| {
        let _ = writeln!(stderr, "toolkit {COMPONENT}: {message}");
        let _ = stderr.flush();
        STARTUP_REFUSAL
    };
    let scope = match signing_scope(
        args.target.host(),
        args.service.as_deref(),
        args.region.as_deref(),
        backend.env(),
    ) {
        Ok(scope) => scope,
        Err(error) => return refuse(error.message()),
    };
    if let Err(message) = backend.validate_roots() {
        return refuse(&message);
    }
    let (stdin, null_stdin) = match backend.stdin() {
        Ok(stdin) => stdin,
        Err(message) => return refuse(&message),
    };
    if let Err(message) = backend.check_target(&args.target) {
        return refuse(&message);
    }
    let profile_region = backend.profile_region(args.profile.as_deref());
    let auto_region = metadata_region(args.target.host(), profile_region.as_deref(), backend.env());
    let Ok(config) = proxy_config(args, auto_region) else {
        return refuse("the proxy configuration was refused");
    };
    let stdout = match backend.stdout() {
        Ok(stdout) => stdout,
        Err(message) => return refuse(&message),
    };
    let auth_diag = Diag::new(COMPONENT, backend.stderr()).with_threshold(args.log_level);
    let (transport, auth) = match backend.build(args, scope, null_stdin, auth_diag) {
        Ok(built) => built,
        Err(message) => return refuse(&message),
    };
    let diag = Diag::new(COMPONENT, backend.stderr()).with_threshold(args.log_level);
    let proxy = Proxy::new(config, transport, auth, backend.clock());
    match proxy.run(stdin, stdout, diag) {
        Exit::StdinClosed => 0,
        Exit::StdoutClosed => 1,
    }
}

/// A private duplicate of the process stdout.
///
/// # Errors
/// When the descriptor cannot be duplicated.
pub fn private_stdout() -> std::io::Result<File> {
    duplicate(&std::io::stdout())
}

/// A private, line-buffered duplicate of the process stderr (or a sink).
#[must_use]
pub fn private_stderr() -> Box<dyn Write + Send> {
    match duplicate(&std::io::stderr()) {
        Ok(file) => Box::new(LineWriter::new(file)),
        Err(_) => Box::new(std::io::sink()),
    }
}

#[cfg(unix)]
fn duplicate(handle: &impl std::os::fd::AsFd) -> std::io::Result<File> {
    handle.as_fd().try_clone_to_owned().map(File::from)
}

#[cfg(windows)]
fn duplicate(handle: &impl std::os::windows::io::AsHandle) -> std::io::Result<File> {
    handle.as_handle().try_clone_to_owned().map(File::from)
}

#[cfg(not(any(unix, windows)))]
fn duplicate<T>(_handle: &T) -> std::io::Result<File> {
    Err(std::io::Error::other(
        "no descriptor duplication on this target",
    ))
}

/// The production backend of this build.
#[cfg(feature = "aws-sdk")]
fn production_backend() -> SdkBackend {
    SdkBackend::new()
}

/// The production backend of this build.
#[cfg(not(feature = "aws-sdk"))]
fn production_backend() -> NoSdkBackend {
    NoSdkBackend
}

/// A build without the `aws-sdk` feature: every start is refused at the
/// roots step, before stdin is read or anything is contacted.
#[derive(Debug, Default)]
pub struct NoSdkBackend;

/// The refusal of a build without the SDK.
pub const NO_SDK: &str =
    "this toolkit build has no AWS SDK support; rebuild it with --features aws-sdk";

impl Backend for NoSdkBackend {
    fn env(&self) -> &dyn Env {
        &toolkit_aws::ProcessEnv
    }

    fn validate_roots(&mut self) -> Result<(), String> {
        Err(NO_SDK.to_string())
    }

    fn stdin(&mut self) -> Result<(Box<dyn Read + Send>, bool), String> {
        Err(NO_SDK.to_string())
    }

    fn check_target(&self, _target: &McpTarget) -> Result<(), String> {
        Err(NO_SDK.to_string())
    }

    fn profile_region(&self, _profile: Option<&str>) -> Option<String> {
        None
    }

    fn build(
        &mut self,
        _args: &ProxyArgs,
        _scope: SigningScope,
        _null_stdin: bool,
        _diag: Diag,
    ) -> Result<Built, String> {
        Err(NO_SDK.to_string())
    }

    fn clock(&self) -> Arc<dyn Clock> {
        Arc::new(super::clock::SystemClock::new())
    }

    fn stdout(&mut self) -> Result<Box<dyn Write + Send>, String> {
        Err(NO_SDK.to_string())
    }

    fn stderr(&mut self) -> Box<dyn Write + Send> {
        private_stderr()
    }
}

#[cfg(feature = "aws-sdk")]
pub use sdk_backend::SdkBackend;

#[cfg(feature = "aws-sdk")]
mod sdk_backend {
    use std::io::{Read, Write};
    use std::sync::Arc;

    use toolkit_aws::diag::Diag;
    use toolkit_aws::preflight::platform_isolates_stdin;
    use toolkit_aws::sdk::roots::{RootsFailure, ValidatedRoots};
    use toolkit_aws::sdk::source::{GuardedCredentials, ProviderHttp, RequestCredentials};
    use toolkit_aws::sdk::stdin::isolate_stdin;
    use toolkit_aws::sdk::{SdkRuntime, SdkTransport, SigV4Signer};
    use toolkit_aws::{
        AwsError, Credentials, CredentialsProvider, Env, HostResolver, McpTarget, ProcessEnv,
        SelectedProfile, Signer, SigningScope, StreamingTransport, SystemResolver,
    };

    use super::super::auth::{CredentialSource, Resolved, SignWith, SourceAuth};
    use super::super::cli::ProxyArgs;
    use super::super::clock::{Clock, SystemClock};
    use super::{private_stderr, private_stdout, Backend, Built};

    /// The production backend: the process environment and files, the real
    /// roots, stdin isolation, system DNS, the SDK transport and S1's guarded
    /// credentials. Tests may replace the DNS resolver, the MCP transport and
    /// the provider-plane client (never the decisions).
    pub struct SdkBackend {
        roots: Option<ValidatedRoots>,
        resolver: Arc<dyn HostResolver + Send + Sync>,
        transport: Option<Arc<dyn StreamingTransport + Send + Sync>>,
        provider_http: Option<ProviderHttp>,
        credentials: Option<Arc<GuardedCredentials>>,
        record_builds: bool,
    }

    impl Default for SdkBackend {
        fn default() -> Self {
            Self::new()
        }
    }

    impl SdkBackend {
        /// The production wiring.
        #[must_use]
        pub fn new() -> Self {
            Self {
                roots: None,
                resolver: Arc::new(SystemResolver),
                transport: None,
                provider_http: None,
                credentials: None,
                record_builds: false,
            }
        }

        /// Replace the start-up DNS resolver (tests).
        #[must_use]
        pub fn with_resolver(mut self, resolver: Arc<dyn HostResolver + Send + Sync>) -> Self {
            self.resolver = resolver;
            self
        }

        /// Replace the MCP-plane transport (tests).
        #[must_use]
        pub fn with_transport(
            mut self,
            transport: Arc<dyn StreamingTransport + Send + Sync>,
        ) -> Self {
            self.transport = Some(transport);
            self
        }

        /// Replace the provider-plane client (tests: the recorder).
        #[must_use]
        pub fn with_provider_http(mut self, http: ProviderHttp) -> Self {
            self.provider_http = Some(http);
            self
        }

        /// Record the guarded credentials' provider builds (a bounded test
        /// instrument, #5112; production never sets it).
        #[must_use]
        pub fn with_build_recording(mut self) -> Self {
            self.record_builds = true;
            self
        }

        /// The roots validated at start (after [`Backend::validate_roots`]).
        #[must_use]
        pub fn roots(&self) -> Option<&ValidatedRoots> {
            self.roots.as_ref()
        }

        /// The guarded credentials built at start (tests read the build log).
        #[must_use]
        pub fn credentials(&self) -> Option<Arc<GuardedCredentials>> {
            self.credentials.clone()
        }
    }

    /// Fixed credentials for one signature.
    struct Resolved1(Credentials);

    impl CredentialsProvider for Resolved1 {
        fn name(&self) -> &'static str {
            "mcp-proxy"
        }

        fn load(&self) -> Result<Credentials, AwsError> {
            Ok(self.0.clone())
        }
    }

    /// `SigV4` with credentials the guarded source resolved.
    pub fn sigv4() -> SignWith {
        Box::new(|credentials, request, scope| {
            SigV4Signer::new(Resolved1(credentials.clone())).sign(request, scope)
        })
    }

    /// S1's guarded credentials as the proxy's credential source.
    pub struct GuardedSource(pub Arc<GuardedCredentials>);

    impl CredentialSource for GuardedSource {
        fn resolve(&self, skip_auth: bool, force_rebuild: bool) -> Resolved {
            match self.0.resolve(skip_auth, force_rebuild) {
                RequestCredentials::Sign(credentials) => Resolved::Sign(credentials),
                RequestCredentials::Unsigned => Resolved::Unsigned,
                RequestCredentials::Refused(refusal) => Resolved::Refused(refusal),
            }
        }

        fn end_session(&self) {
            self.0.end_session();
        }
    }

    impl Backend for SdkBackend {
        fn env(&self) -> &dyn Env {
            &ProcessEnv
        }

        fn validate_roots(&mut self) -> Result<(), String> {
            let roots = ValidatedRoots::from_process().map_err(RootsFailure::message)?;
            self.roots = Some(roots);
            Ok(())
        }

        fn stdin(&mut self) -> Result<(Box<dyn Read + Send>, bool), String> {
            match isolate_stdin() {
                Ok(file) => Ok((Box::new(file), true)),
                // No mechanism on this target: run, but a profile that could
                // start a credential process fails closed per request.
                Err(_) if !platform_isolates_stdin() => Ok((Box::new(std::io::stdin()), false)),
                Err(_) => Err("stdin could not be isolated from child processes".to_string()),
            }
        }

        fn check_target(&self, target: &McpTarget) -> Result<(), String> {
            target
                .check_public(self.resolver.as_ref())
                .map_err(|error| error.message().to_string())
        }

        fn profile_region(&self, profile: Option<&str>) -> Option<String> {
            let selected = SelectedProfile::resolve(profile, &ProcessEnv);
            toolkit_aws::sdk::profile::profile_region(&ProcessEnv, &selected)
        }

        fn build(
            &mut self,
            args: &ProxyArgs,
            scope: SigningScope,
            null_stdin: bool,
            mut diag: Diag,
        ) -> Result<Built, String> {
            let roots = self
                .roots
                .clone()
                .ok_or_else(|| "TLS trust roots were not validated".to_string())?;
            roots.report_to(&mut diag);
            let refused = |_| "the HTTPS client could not be built".to_string();
            let transport: Arc<dyn StreamingTransport + Send + Sync> =
                if let Some(transport) = &self.transport {
                    Arc::clone(transport)
                } else {
                    let runtime = SdkRuntime::new().map_err(refused)?;
                    Arc::new(SdkTransport::new(&runtime, &roots).map_err(refused)?)
                };
            let http = match &self.provider_http {
                Some(http) => http.clone(),
                None => ProviderHttp::validated(&roots).map_err(refused)?,
            };
            let runtime = SdkRuntime::new().map_err(refused)?;
            let mut guarded =
                GuardedCredentials::new(&runtime, &scope.region, args.profile.as_deref(), http)
                    .with_null_stdin(null_stdin);
            if self.record_builds {
                guarded = guarded.with_build_recording();
            }
            let credentials = Arc::new(guarded);
            self.credentials = Some(Arc::clone(&credentials));
            let auth = SourceAuth::new(
                Box::new(GuardedSource(credentials)),
                sigv4(),
                scope,
                args.skip_auth,
                diag,
            );
            Ok((transport, Arc::new(auth)))
        }

        fn clock(&self) -> Arc<dyn Clock> {
            Arc::new(SystemClock::new())
        }

        fn stdout(&mut self) -> Result<Box<dyn Write + Send>, String> {
            private_stdout()
                .map(|file| Box::new(file) as Box<dyn Write + Send>)
                .map_err(|_| "stdout could not be opened".to_string())
        }

        fn stderr(&mut self) -> Box<dyn Write + Send> {
            private_stderr()
        }
    }
}
