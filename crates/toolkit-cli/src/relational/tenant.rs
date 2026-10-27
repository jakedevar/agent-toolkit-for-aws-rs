//! Tenant-scoped reads of Aurora DSQL (port of `aurora-dsql/scripts/tenant_query.py`).
//!
//! `select_by_tenant` validates the untrusted tenant with [`TENANT_SLUG`],
//! makes the one [`TenantRead`] statement and runs it on a [`SqlCursor`]. The
//! cursor is the seam: tests and `--recorded FILE` use scripted cursors, and the
//! build ships no live connection ([`NoLiveCursor`] opens no socket and reads no
//! credential). A live cursor behind a TLS-authenticated DSQL connection is a
//! follow-up (`docs/migration/relational.md`).
//!
//! The read-only guarantee is structural, not a filter on SQL text: a
//! [`SqlCursor`] accepts only a [`TenantRead`], and a [`TenantRead`] can only be
//! made by [`TenantRead::for_tenant`] from the fixed template
//! [`TENANT_READ_TEMPLATE`] and a tenant that passed [`TENANT_SLUG`]. There is
//! no raw-SQL path into a cursor (#4738). A general read path over arbitrary
//! SQL is out of scope: it would need a database-enforced read-only
//! transaction or role, not a statement filter.
//!
//! Format validation confirms that the value looks like a tenant slug. It does
//! not prove the caller may read that tenant's data: authorize the caller
//! against the tenant before calling.

use std::fmt;

use toolkit_core::Json;

use super::members::{array_member, object_member, py_type_name, text_member};
use super::sql::{build, ident, regex, UnsafeSqlError, TENANT_SLUG};

/// The template of the one statement this slice runs.
pub(crate) const TENANT_READ_TEMPLATE: &str = "SELECT * FROM {t} WHERE tenant_id = {tid}";

/// The tenant read `select_by_tenant` runs: [`TENANT_READ_TEMPLATE`] filled
/// with the `entities` table and a validated tenant slug.
///
/// The field is private and the type implements no conversion from a string
/// (`From`, `FromStr`, `Default`, deserialization): [`Self::for_tenant`] is its
/// only constructor, so no other SQL can be expressed as a `TenantRead`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TenantRead(String);

impl TenantRead {
    /// The tenant read for `tenant`.
    ///
    /// # Errors
    ///
    /// `Unsafe` when the tenant is not a valid slug.
    pub(crate) fn for_tenant(tenant: &str) -> Result<Self, UnsafeSqlError> {
        let table = ident("entities")?;
        let tenant_id = regex(tenant, &TENANT_SLUG, "req.tenant")?;
        build(TENANT_READ_TEMPLATE, &[("t", &table), ("tid", &tenant_id)]).map(Self)
    }

    /// The statement text, for the driver or for printing.
    pub(crate) fn sql(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TenantRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A failure reported by a cursor. The text is untrusted (a driver or the
/// recording), so callers print it through the diagnostic helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DbError(pub(crate) String);

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A row, as the driver returns it.
pub(crate) type Row = Vec<Json>;

/// What `tenant_query.py` needs of a driver cursor: `.execute(sql)` then
/// `.fetchall()`. `execute` takes a [`TenantRead`], never raw SQL: the only
/// statement a cursor of this slice can be asked to run is the tenant read.
pub(crate) trait SqlCursor {
    /// # Errors
    ///
    /// The driver's failure.
    fn execute(&mut self, statement: &TenantRead) -> Result<(), DbError>;

    /// # Errors
    ///
    /// The driver's failure, or a `fetchall` without a prior `execute`.
    fn fetchall(&mut self) -> Result<Vec<Row>, DbError>;
}

/// Why `select_by_tenant` failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TenantQueryError {
    /// The tenant failed validation (`UnsafeSQLError`).
    Unsafe(UnsafeSqlError),
    /// The tenant is not a string (`ValueError`); holds the Python type name.
    NotAString(&'static str),
    /// The cursor failed.
    Database(DbError),
}

impl fmt::Display for TenantQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsafe(error) => error.fmt(f),
            Self::NotAString(type_name) => {
                write!(f, "req.tenant must be a str, got {type_name}")
            }
            Self::Database(error) => error.fmt(f),
        }
    }
}

/// All rows of `entities` for `tenant`, which is untrusted and may be any
/// decoded JSON value.
///
/// # Errors
///
/// `NotAString` before anything runs, `Unsafe` for an invalid slug (the cursor
/// is not touched), `Database` when the cursor fails.
pub(crate) fn select_by_tenant(
    cursor: &mut dyn SqlCursor,
    tenant: &Json,
) -> Result<Vec<Row>, TenantQueryError> {
    let Json::String(tenant) = tenant else {
        return Err(TenantQueryError::NotAString(py_type_name(tenant)));
    };
    let statement = TenantRead::for_tenant(tenant).map_err(TenantQueryError::Unsafe)?;
    cursor
        .execute(&statement)
        .map_err(TenantQueryError::Database)?;
    cursor.fetchall().map_err(TenantQueryError::Database)
}

/// The cursor of a run without `--recorded`: it opens no connection, reads no
/// credential and fails every call.
pub(crate) struct NoLiveCursor;

impl SqlCursor for NoLiveCursor {
    fn execute(&mut self, _statement: &TenantRead) -> Result<(), DbError> {
        Err(DbError(
            "this build includes no live Aurora DSQL connection; pass --recorded FILE to \
             replay recorded rows"
                .to_string(),
        ))
    }

    fn fetchall(&mut self) -> Result<Vec<Row>, DbError> {
        Err(DbError("no statement was executed".to_string()))
    }
}

enum Outcome {
    Rows(Vec<Row>),
    Failure(String),
}

/// Replays `--recorded FILE`:
///
/// ```json
/// {"statements": [
///   {"sql": "SELECT * FROM \"entities\" WHERE tenant_id = 'acme'",
///    "rows": [["e1", "acme"]]},
///   {"sql": "...", "error": {"message": "connection reset"}}
/// ]}
/// ```
///
/// A statement matches an entry with exactly the same `sql`; the recorded text is
/// only compared with a [`TenantRead`], never run. `rows` and
/// `error` are optional and exclusive; both absent means no rows. A present
/// member of another type, `null` included, rejects the recording. An
/// unmatched statement fails with a message that does not echo the SQL.
pub(crate) struct RecordedCursor {
    statements: Vec<(String, Outcome)>,
    pending: Option<Vec<Row>>,
}

impl RecordedCursor {
    /// # Errors
    ///
    /// A reason naming the entry index and the wrong member, never a value.
    pub(crate) fn from_document(document: &Json) -> Result<Self, String> {
        // recorded-shape: required member; absent, null or another type rejects the recording.
        let Some(Json::Array(entries)) = document.get("statements") else {
            return Err("the recorded rows need a \"statements\" array".to_string());
        };
        let mut statements = Vec::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            let bad = |what: &str| format!("recorded statement {index}: {what}");
            let sql = entry
                .get("sql")
                .and_then(Json::as_str)
                .ok_or_else(|| bad("\"sql\" must be a string"))?;
            let rows = array_member(entry, "rows").map_err(|reason| bad(&reason))?;
            let error = object_member(entry, "error").map_err(|reason| bad(&reason))?;
            let outcome = match (rows, error) {
                (Some(_), Some(_)) => {
                    return Err(bad("\"rows\" and \"error\" are exclusive"));
                }
                (None, Some(error)) => {
                    let message = text_member(error, "message").map_err(|reason| bad(&reason))?;
                    Outcome::Failure(message.unwrap_or_default().to_string())
                }
                (rows, None) => {
                    let mut parsed = Vec::new();
                    for row in rows.into_iter().flatten() {
                        // recorded-shape: a required row of the recording; any shape but an array rejects it.
                        let Json::Array(cells) = row else {
                            return Err(bad("every row must be an array"));
                        };
                        parsed.push(cells.clone());
                    }
                    Outcome::Rows(parsed)
                }
            };
            statements.push((sql.to_string(), outcome));
        }
        Ok(Self {
            statements,
            pending: None,
        })
    }
}

impl SqlCursor for RecordedCursor {
    fn execute(&mut self, statement: &TenantRead) -> Result<(), DbError> {
        self.pending = None;
        match self
            .statements
            .iter()
            .find(|(recorded, _)| recorded == statement.sql())
        {
            Some((_, Outcome::Rows(rows))) => {
                self.pending = Some(rows.clone());
                Ok(())
            }
            Some((_, Outcome::Failure(message))) => Err(DbError(message.clone())),
            None => Err(DbError(
                "no recorded statement matches the executed SQL".to_string(),
            )),
        }
    }

    fn fetchall(&mut self) -> Result<Vec<Row>, DbError> {
        self.pending
            .take()
            .ok_or_else(|| DbError("no statement was executed".to_string()))
    }
}
