//! Offline normalizer of a user-run mongo-shell capture (#5327, S5).
//!
//! RED STATE: signatures only; every behaviour is added after the tests.

#![allow(dead_code)]

use toolkit_core::Json;

/// Fixed error classes of a refused capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CaptureError {
    Unreadable,
    TooLarge,
    Permissions,
    Malformed,
}

impl CaptureError {
    pub(super) fn class(self) -> &'static str {
        match self {
            Self::Unreadable => "unreadable",
            Self::TooLarge => "too_large",
            Self::Permissions => "permissions",
            Self::Malformed => "malformed",
        }
    }
}

pub(super) struct Limits {
    pub(super) max_bytes: u64,
    pub(super) max_databases: usize,
    pub(super) max_collections: usize,
    pub(super) max_indexes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 0,
            max_databases: 0,
            max_collections: 0,
            max_indexes: 0,
        }
    }
}

pub(super) struct Normalized {
    pub(super) snapshot: Json,
    pub(super) databases: usize,
    pub(super) collections: usize,
    pub(super) incomplete: usize,
}

pub(super) enum Access {
    UnixMode(u32),
    Acl { owner_only: bool },
    Unverifiable,
}

pub(super) struct Command {
    pub(super) id: &'static str,
    pub(super) slot: &'static str,
    pub(super) command: &'static str,
    pub(super) fields: &'static [&'static str],
    pub(super) ignored: &'static [&'static str],
}

pub(super) struct Manifest {
    pub(super) format: &'static str,
    pub(super) version: i64,
    pub(super) dialect: &'static str,
    pub(super) envelope_keys: &'static [&'static str],
    pub(super) database_keys: &'static [&'static str],
    pub(super) collection_keys: &'static [&'static str],
    pub(super) error_codes: &'static [&'static str],
    pub(super) excluded_databases: &'static [&'static str],
    pub(super) excluded_collection_prefix: &'static str,
    pub(super) commands: &'static [Command],
}

pub(super) const MANIFEST: Manifest = Manifest {
    format: "",
    version: 0,
    dialect: "",
    envelope_keys: &[],
    database_keys: &[],
    collection_keys: &[],
    error_codes: &[],
    excluded_databases: &[],
    excluded_collection_prefix: "",
    commands: &[],
};

pub(super) fn admit_access(_access: Access) -> Result<(), CaptureError> {
    Err(CaptureError::Malformed)
}

pub(super) fn normalize(_document: &Json, _limits: &Limits) -> Result<Normalized, CaptureError> {
    Err(CaptureError::Malformed)
}
