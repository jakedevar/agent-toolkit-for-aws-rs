//! Tests of the telemetry port (#4398). All offline: an isolated home, an
//! injected environment, clock and stdin, and a recording sink; no network,
//! credentials, telemetry endpoint or Python.

mod attributes;
mod bundles;
mod client;
mod consent;
mod fx;
mod migration;
mod registration;
