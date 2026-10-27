//! Tenant-scoped reads of Aurora DSQL (port of `aurora-dsql/scripts/tenant_query.py`).
//!
//! `select_by_tenant` validates the untrusted tenant with [`TENANT_SLUG`],
//! builds one `SELECT` with [`build`] and runs it on a [`SqlCursor`]. The
//! cursor is the seam: tests and `--recorded FILE` use scripted cursors, and the
//! build ships no live connection ([`NoLiveCursor`] opens no socket and reads no
//! credential). A live cursor behind a TLS-authenticated DSQL connection is a
//! follow-up (`docs/migration/relational.md`).
//!
//! Format validation confirms that the value looks like a tenant slug. It does
//! not prove the caller may read that tenant's data: authorize the caller
//! against the tenant before calling.

use std::fmt;

use toolkit_core::Json;

use super::members::{optional_array, optional_object, optional_string, py_type_name};
use super::sql::{build, ident, regex, require_read_only, UnsafeSqlError, TENANT_SLUG};

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
/// `.fetchall()`.
pub(crate) trait SqlCursor {
    /// # Errors
    ///
    /// The driver's failure.
    fn execute(&mut self, sql: &str) -> Result<(), DbError>;

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

/// The statement `select_by_tenant` runs for a validated tenant.
///
/// # Errors
///
/// `Unsafe` when the tenant is not a valid slug.
pub(crate) fn tenant_sql(tenant: &str) -> Result<String, UnsafeSqlError> {
    let table = ident("entities")?;
    let tenant_id = regex(tenant, &TENANT_SLUG, "req.tenant")?;
    build(
        "SELECT * FROM {t} WHERE tenant_id = {tid}",
        &[("t", &table), ("tid", &tenant_id)],
    )
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
    let sql = tenant_sql(tenant).map_err(TenantQueryError::Unsafe)?;
    cursor.execute(&sql).map_err(TenantQueryError::Database)?;
    cursor.fetchall().map_err(TenantQueryError::Database)
}

/// Refuses every statement that is not a single `SELECT`, then delegates.
pub(crate) struct ReadOnly<C: SqlCursor>(pub(crate) C);

impl<C: SqlCursor> SqlCursor for ReadOnly<C> {
    fn execute(&mut self, sql: &str) -> Result<(), DbError> {
        require_read_only(sql).map_err(|error| DbError(error.to_string()))?;
        self.0.execute(sql)
    }

    fn fetchall(&mut self) -> Result<Vec<Row>, DbError> {
        self.0.fetchall()
    }
}

/// The cursor of a run without `--recorded`: it opens no connection, reads no
/// credential and fails every call.
pub(crate) struct NoLiveCursor;

impl SqlCursor for NoLiveCursor {
    fn execute(&mut self, _sql: &str) -> Result<(), DbError> {
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
/// A statement matches an entry with exactly the same `sql`. `rows` and
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
            let rows = optional_array(entry, "rows").map_err(|reason| bad(&reason))?;
            let error = optional_object(entry, "error").map_err(|reason| bad(&reason))?;
            let outcome = match (rows, error) {
                (Some(_), Some(_)) => {
                    return Err(bad("\"rows\" and \"error\" are exclusive"));
                }
                (None, Some(error)) => {
                    let message =
                        optional_string(error, "message").map_err(|reason| bad(&reason))?;
                    Outcome::Failure(message.unwrap_or_default().to_string())
                }
                (rows, None) => {
                    let mut parsed = Vec::new();
                    for row in rows.into_iter().flatten() {
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
    fn execute(&mut self, sql: &str) -> Result<(), DbError> {
        self.pending = None;
        match self.statements.iter().find(|(recorded, _)| recorded == sql) {
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
