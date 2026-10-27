//! Shared AWS/API client boundaries for the Agent Toolkit Rust CLI (#4379).
//!
//! Everything a domain slice needs to talk to an AWS or AWS-style API *safely*,
//! with no network, no credential access and no third-party dependency of its
//! own (the AWS SDK binding is a separate, feature-gated adapter; see
//! `docs/migration/aws-core-contracts.md`):
//!
//! - [`config`]: region, profile and endpoint resolution into a [`ClientConfig`]
//!   with the Python precedence rules;
//! - [`endpoint`]: https-only, AWS-domain-only, public-address-only endpoint
//!   validation with an injectable DNS resolver;
//! - [`credentials`]: the [`CredentialsProvider`] seam, botocore-style provider
//!   chain semantics, redacting [`Credentials`], and rejection of credential
//!   command-line flags;
//! - [`retry`]: bounded exponential backoff with injected sleep and jitter;
//! - [`paginate`]: token pagination with page, item and loop guards;
//! - [`transport`] / [`client`]: prepared requests, the [`Transport`] and
//!   [`Signer`] seams and the [`ApiClient`] pipeline that signs, sends,
//!   classifies errors and retries;
//! - [`error`] / [`redact`]: one redacted error type, the `Failed to <action>`
//!   report format, and the secret scrubbing every message passes through;
//! - [`testing`]: offline doubles for all of the above.
#![allow(clippy::module_name_repetitions)]

pub mod client;
pub mod config;
pub mod credentials;
pub mod endpoint;
pub mod env;
pub mod error;
pub mod paginate;
pub mod redact;
pub mod retry;
pub mod testing;
pub mod transport;

pub use client::ApiClient;
pub use config::{
    endpoint_env_name, resolve_endpoint, resolve_profile, resolve_region, ClientConfig,
    ClientSettings, RegionSource, ResolvedRegion, DEFAULT_REGION,
};
pub use credentials::{
    reject_credential_arguments, Credentials, CredentialsProvider, ProviderChain,
    STANDARD_CHAIN_STAGES,
};
pub use endpoint::{
    is_valid_region, validate_endpoint, Endpoint, EndpointPolicy, HostResolver, SystemResolver,
};
pub use env::{Env, MapEnv, ProcessEnv};
pub use error::{render_failure, render_failure_with, AwsError, Guidance, ServiceError};
pub use paginate::{collect_pages, next_token_from, Page, PageLimits};
pub use redact::{redact_text, sanitize_path, Secret, REDACTED};
pub use retry::{
    run_with_retry, Attempted, FullJitter, Jitter, NoJitter, RetryFailure, RetryMode, RetryPolicy,
    Sleeper, ThreadSleeper,
};
pub use transport::{
    ApiRequest, Headers, HttpRequest, HttpResponse, Method, NoSigner, Signer, SigningScope,
    Timeouts, Transport,
};
