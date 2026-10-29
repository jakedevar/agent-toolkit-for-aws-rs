//! Contract tests for the serverless examples (#4397).
//!
//! The examples themselves are Cargo projects inside the skills that ship
//! them (`skills/core-skills/aws-serverless/assets/powertools-handler` and
//! `skills/specialized-skills/serverless-skills/processing-s3-uploads-with-step-functions/scripts`).
//! They are workspace members so that their own tests run with the rest of
//! the repository, but they are not part of the `toolkit` binary or its
//! command registry. This crate holds the repository-level checks of their
//! recipes (see `tests/recipes.rs`) and docs/migration/serverless-examples.md.
