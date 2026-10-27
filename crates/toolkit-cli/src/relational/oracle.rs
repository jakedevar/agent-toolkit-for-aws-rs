//! Oracle connection test for an RDS instance (port of
//! `rds-oracle/scripts/test_oracle_connection.py`).
//!
//! The script connected with `python-oracledb` and printed four checks:
//! connection, a basic query, encryption status and authentication info. Here
//! every database interaction is one method of [`OracleDriver`]:
//!
//! - [`RecordedDriver`] replays a recorded session (`--recorded FILE`), so the
//!   whole report is deterministic and offline;
//! - [`NoLiveDriver`] is the driver of a run without `--recorded`. It opens no
//!   socket, reads no credential (the password is neither read from
//!   `ORACLE_PASSWORD` nor prompted for) and fails at the driver check.
//!
//! A live driver is a follow-up (`docs/migration/relational.md`): it must read
//! the password from `ORACLE_PASSWORD` or a no-echo prompt, never an argument,
//! hold it in a `toolkit_aws::Secret` and build the connection string only
//! through [`ConnectParams::dsn`].
//!
//! Security: the endpoint, port, service name, user name and DSN are shown only
//! with `--verbose`. Every database-derived or caller-derived text is redacted
//! with `toolkit_aws::redact_text` and escaped to one line before it is
//! printed.

use toolkit_aws::redact_text;
use toolkit_core::{neutralize_line, Json};

use super::members::{
    optional_array, optional_bool, optional_int, optional_object, optional_seconds, optional_string,
};
use crate::aurora::pyfmt::fmt_f;

/// A database error: the Oracle code when the driver reports one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DbError {
    pub(crate) code: Option<i64>,
    pub(crate) message: String,
}

impl DbError {
    fn recorded_gap(what: &str) -> Self {
        Self {
            code: None,
            message: format!("no recorded result for {what}"),
        }
    }
}

/// Connection parameters. The password is not part of it: only a live driver
/// needs one, and it reads it itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectParams {
    pub(crate) endpoint: String,
    pub(crate) port: u16,
    pub(crate) service_name: String,
    pub(crate) username: String,
}

impl ConnectParams {
    /// The Easy Connect string `endpoint:port/service_name`.
    pub(crate) fn dsn(&self) -> String {
        format!("{}:{}/{}", self.endpoint, self.port, self.service_name)
    }
}

/// The driver as the script reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DriverInfo {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) thin: bool,
}

/// A refused driver check: the failure and how to fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DriverMissing {
    pub(crate) reason: String,
    pub(crate) fix: String,
}

/// A failed connection attempt and how long it took.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConnectFailure {
    pub(crate) error: DbError,
    pub(crate) elapsed_seconds: f64,
}

/// A connected session.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Connected {
    pub(crate) version: String,
    pub(crate) elapsed_seconds: f64,
}

/// The four queries of the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Probe {
    Basic,
    Protocol,
    Banners,
    Auth,
}

impl Probe {
    fn key(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Protocol => "protocol",
            Self::Banners => "banners",
            Self::Auth => "auth",
        }
    }
}

/// The statements of the script, passed to a live driver by the report.
const SQL_BASIC: &str = "SELECT sysdate, SYS_CONTEXT('USERENV', 'DB_NAME'), \
SYS_CONTEXT('USERENV', 'SESSION_USER') FROM dual";
const SQL_PROTOCOL: &str = "SELECT SYS_CONTEXT('USERENV', 'NETWORK_PROTOCOL') FROM dual";
const SQL_BANNERS: &str = "SELECT network_service_banner FROM v$session_connect_info \
WHERE sid = SYS_CONTEXT('USERENV', 'SID') AND network_service_banner IS NOT NULL";
const SQL_AUTH: &str = "SELECT SYS_CONTEXT('USERENV', 'AUTHENTICATION_METHOD'), \
SYS_CONTEXT('USERENV', 'AUTHENTICATED_IDENTITY'), SYS_CONTEXT('USERENV', 'HOST') FROM dual";

/// A row of text cells; `None` is SQL `NULL`.
pub(crate) type Row = Vec<Option<String>>;

/// The database seam.
pub(crate) trait OracleDriver {
    /// `import oracledb`.
    ///
    /// # Errors
    ///
    /// The driver is not available.
    fn driver(&self) -> Result<DriverInfo, DriverMissing>;

    /// # Errors
    ///
    /// The connection failure, with its Oracle code when there is one.
    fn connect(&mut self, params: &ConnectParams) -> Result<Connected, ConnectFailure>;

    /// Runs the probe's statement on the connected session.
    ///
    /// # Errors
    ///
    /// The statement failure.
    fn query(&mut self, probe: Probe, sql: &str) -> Result<Vec<Row>, DbError>;
}

/// The driver of a run without `--recorded`: nothing is sent or read.
pub(crate) struct NoLiveDriver;

impl OracleDriver for NoLiveDriver {
    fn driver(&self) -> Result<DriverInfo, DriverMissing> {
        Err(DriverMissing {
            reason: "this build includes no live Oracle driver".to_string(),
            fix: "pass --recorded FILE to replay a recorded session".to_string(),
        })
    }

    fn connect(&mut self, _params: &ConnectParams) -> Result<Connected, ConnectFailure> {
        Err(ConnectFailure {
            error: DbError {
                code: None,
                message: "no live Oracle driver".to_string(),
            },
            elapsed_seconds: 0.0,
        })
    }

    fn query(&mut self, _probe: Probe, _sql: &str) -> Result<Vec<Row>, DbError> {
        Err(DbError {
            code: None,
            message: "no live Oracle driver".to_string(),
        })
    }
}

type ProbeResult = Result<Vec<Row>, DbError>;

/// Replays `--recorded FILE`:
///
/// ```json
/// {
///   "driver": {"installed": true, "name": "oracledb", "version": "2.5.1", "thin": true},
///   "connect": {"version": "19.0.0.0.0", "elapsed_seconds": 0.21},
///   "probes": {
///     "basic":    {"rows": [["2026-10-27 09:00:00", "ORCL", "ADMIN"]]},
///     "protocol": {"rows": [["tcps"]]},
///     "banners":  {"rows": [["Encryption service adapter"]]},
///     "auth":     {"error": {"code": 942, "message": "ORA-00942: table or view does not exist"}}
///   }
/// }
/// ```
///
/// Every member is optional. `driver` defaults to an installed thin
/// `oracledb` 0; `connect` defaults to success after 0 seconds; `connect.error`
/// (`{"code": N, "message": "..."}`) makes the connection fail. A probe is
/// `rows` (an array of arrays of strings or `null`) or `error`, not both; a
/// probe that is absent fails with "no recorded result". A present member of
/// another type, `null` included, rejects the recording.
pub(crate) struct RecordedDriver {
    driver: Result<DriverInfo, DriverMissing>,
    connect: Result<Connected, ConnectFailure>,
    probes: [Option<ProbeResult>; 4],
}

fn parse_db_error(error: &Json) -> Result<DbError, String> {
    Ok(DbError {
        code: optional_int(error, "code")?,
        message: optional_string(error, "message")?
            .unwrap_or_default()
            .to_string(),
    })
}

fn parse_cell(cell: &Json) -> Result<Option<String>, String> {
    match cell {
        Json::Null => Ok(None),
        Json::String(text) => Ok(Some(text.clone())),
        other => Err(format!(
            "every cell must be a string or null (it is {})",
            other.type_name()
        )),
    }
}

fn parse_probe(probe: &Json) -> Result<ProbeResult, String> {
    let rows = optional_array(probe, "rows")?;
    let error = optional_object(probe, "error")?;
    match (rows, error) {
        (Some(_), Some(_)) => Err("\"rows\" and \"error\" are exclusive".to_string()),
        (None, Some(error)) => Ok(Err(parse_db_error(error)?)),
        (rows, None) => {
            let mut parsed = Vec::new();
            for row in rows.into_iter().flatten() {
                let Json::Array(cells) = row else {
                    return Err("every row must be an array".to_string());
                };
                parsed.push(
                    cells
                        .iter()
                        .map(parse_cell)
                        .collect::<Result<Row, String>>()?,
                );
            }
            Ok(Ok(parsed))
        }
    }
}

impl RecordedDriver {
    /// # Errors
    ///
    /// A reason naming the wrong member, never a value.
    pub(crate) fn from_document(document: &Json) -> Result<Self, String> {
        if !matches!(document, Json::Object(_)) {
            return Err("the recorded session must be a JSON object".to_string());
        }
        let driver = match optional_object(document, "driver")? {
            None => Ok(DriverInfo {
                name: "oracledb".to_string(),
                version: "0".to_string(),
                thin: true,
            }),
            Some(info) => {
                let in_driver = |reason: String| format!("driver {reason}");
                let installed = optional_bool(info, "installed").map_err(in_driver)?;
                let name = optional_string(info, "name").map_err(in_driver)?;
                let version = optional_string(info, "version").map_err(in_driver)?;
                let thin = optional_bool(info, "thin").map_err(in_driver)?;
                if installed.unwrap_or(true) {
                    Ok(DriverInfo {
                        name: name.unwrap_or("oracledb").to_string(),
                        version: version.unwrap_or("0").to_string(),
                        thin: thin.unwrap_or(true),
                    })
                } else {
                    Err(DriverMissing {
                        reason: "the Oracle driver is not installed".to_string(),
                        fix: "install the Oracle driver and retry".to_string(),
                    })
                }
            }
        };
        let connect = match optional_object(document, "connect")? {
            None => Ok(Connected {
                version: String::new(),
                elapsed_seconds: 0.0,
            }),
            Some(info) => {
                let in_connect = |reason: String| format!("connect {reason}");
                let version = optional_string(info, "version").map_err(in_connect)?;
                let elapsed = optional_seconds(info, "elapsed_seconds").map_err(in_connect)?;
                match optional_object(info, "error").map_err(in_connect)? {
                    Some(error) => Err(ConnectFailure {
                        error: parse_db_error(error).map_err(in_connect)?,
                        elapsed_seconds: elapsed.unwrap_or(0.0),
                    }),
                    None => Ok(Connected {
                        version: version.unwrap_or_default().to_string(),
                        elapsed_seconds: elapsed.unwrap_or(0.0),
                    }),
                }
            }
        };
        let mut probes: [Option<ProbeResult>; 4] = [None, None, None, None];
        if let Some(recorded) = optional_object(document, "probes")? {
            for (slot, probe) in [Probe::Basic, Probe::Protocol, Probe::Banners, Probe::Auth]
                .into_iter()
                .enumerate()
            {
                if let Some(entry) = optional_object(recorded, probe.key())
                    .map_err(|reason| format!("probes {reason}"))?
                {
                    probes[slot] = Some(
                        parse_probe(entry)
                            .map_err(|reason| format!("probe \"{}\": {reason}", probe.key()))?,
                    );
                }
            }
        }
        Ok(Self {
            driver,
            connect,
            probes,
        })
    }
}

impl OracleDriver for RecordedDriver {
    fn driver(&self) -> Result<DriverInfo, DriverMissing> {
        self.driver.clone()
    }

    fn connect(&mut self, _params: &ConnectParams) -> Result<Connected, ConnectFailure> {
        self.connect.clone()
    }

    fn query(&mut self, probe: Probe, _sql: &str) -> Result<Vec<Row>, DbError> {
        let slot = match probe {
            Probe::Basic => 0,
            Probe::Protocol => 1,
            Probe::Banners => 2,
            Probe::Auth => 3,
        };
        self.probes[slot]
            .clone()
            .unwrap_or_else(|| Err(DbError::recorded_gap(probe.key())))
    }
}

/// Hint for a connection error code, as in the script.
fn hint(code: i64) -> Option<&'static str> {
    match code {
        12170 => Some("TNS connect timeout \u{2014} check security groups, VPC routing, endpoint"),
        12541 => Some("No listener \u{2014} check endpoint and port are correct"),
        1017 => Some("Invalid username/password \u{2014} check credentials"),
        12514 => Some("Listener does not know of service \u{2014} check service name"),
        12505 => Some("Listener does not know of SID \u{2014} try SERVICE_NAME instead of SID"),
        28000 => Some("Account is locked \u{2014} unlock the user in the database"),
        _ => None,
    }
}

/// Untrusted text (a caller value or a database message) made safe for one
/// output line: secrets redacted, then every control character escaped.
fn safe(text: &str) -> String {
    neutralize_line(&redact_text(text)).into_owned()
}

fn cell(row: &Row, index: usize) -> String {
    match row.get(index) {
        Some(Some(text)) => safe(text),
        _ => "None".to_string(),
    }
}

/// `ORA-{code}: {message}` as the script printed a database error.
fn ora(error: &DbError) -> String {
    match error.code {
        Some(code) => format!("ORA-{code}: {}", safe(&error.message)),
        None => safe(&error.message),
    }
}

/// The result of a run: the lines of stdout and the exit status.
pub(crate) struct Report {
    pub(crate) lines: Vec<String>,
    pub(crate) status: u8,
}

/// Runs the four checks against `driver` and renders the report.
pub(crate) fn run_checks(
    driver: &mut dyn OracleDriver,
    params: &ConnectParams,
    verbose: bool,
) -> Report {
    let mut lines: Vec<String> = Vec::new();
    lines.push("=== Oracle Connection Test ===".to_string());
    // SECURITY: connection metadata is hidden unless --verbose, so it does not
    // reach persisted logs. The password is never printed (never read).
    if verbose {
        lines.push(format!("Endpoint:     {}", safe(&params.endpoint)));
        lines.push(format!("Port:         {}", params.port));
        lines.push(format!("Service Name: {}", safe(&params.service_name)));
        lines.push(format!("Username:     {}", safe(&params.username)));
        lines.push(format!("DSN:          {}", safe(&params.dsn())));
    } else {
        lines.push(
            "(connection metadata hidden; pass --verbose to show endpoint/DSN/username)"
                .to_string(),
        );
    }
    lines.push(String::new());

    let info = match driver.driver() {
        Ok(info) => info,
        Err(missing) => {
            lines.push(format!("FAIL: {}", safe(&missing.reason)));
            lines.push(format!("  Fix: {}", safe(&missing.fix)));
            return Report { lines, status: 1 };
        }
    };
    lines.push(format!(
        "Driver:       {} {}",
        safe(&info.name),
        safe(&info.version)
    ));
    lines.push(format!(
        "Mode:         {}",
        if info.thin { "Thin" } else { "Thick" }
    ));
    lines.push(String::new());

    lines.push("--- Connection Test ---".to_string());
    match driver.connect(params) {
        Ok(connected) => {
            lines.push(format!(
                "PASS: Connected in {}s",
                fmt_f(connected.elapsed_seconds, 2)
            ));
            lines.push(format!("  Database version: {}", safe(&connected.version)));
        }
        Err(failure) => {
            lines.push(format!(
                "FAIL: Connection failed after {}s",
                fmt_f(failure.elapsed_seconds, 2)
            ));
            lines.push(format!("  {}", ora(&failure.error)));
            if let Some(text) = failure.error.code.and_then(hint) {
                lines.push(format!("  Hint: {text}"));
            }
            return Report { lines, status: 1 };
        }
    }
    lines.push(String::new());

    lines.push("--- Query Test ---".to_string());
    match driver.query(Probe::Basic, SQL_BASIC) {
        Ok(rows) => match rows.first() {
            Some(row) => {
                lines.push("PASS: Query succeeded".to_string());
                lines.push(format!("  Server time:  {}", cell(row, 0)));
                lines.push(format!("  Database:     {}", cell(row, 1)));
                lines.push(format!("  Session user: {}", cell(row, 2)));
            }
            None => lines.push("FAIL: Query failed \u{2014} the query returned no row".to_string()),
        },
        Err(error) => lines.push(format!("FAIL: Query failed \u{2014} {}", ora(&error))),
    }
    lines.push(String::new());

    lines.push("--- Encryption Status ---".to_string());
    encryption_status(driver, &mut lines);
    lines.push(String::new());

    lines.push("--- Authentication Info ---".to_string());
    match driver.query(Probe::Auth, SQL_AUTH) {
        Ok(rows) => match rows.first() {
            Some(row) => {
                lines.push(format!("  Auth method:  {}", cell(row, 0)));
                lines.push(format!("  Identity:     {}", cell(row, 1)));
                lines.push(format!("  Client host:  {}", cell(row, 2)));
            }
            None => lines.push("  WARN: Cannot check auth info".to_string()),
        },
        Err(error) => match error.code {
            Some(code) => lines.push(format!(
                "  WARN: Cannot check auth info \u{2014} ORA-{code}"
            )),
            None => lines.push("  WARN: Cannot check auth info".to_string()),
        },
    }
    lines.push(String::new());
    // The script prints this line whatever the query checks reported; only a
    // failed connection (or driver) exits non-zero.
    lines.push("=== All tests passed ===".to_string());
    Report { lines, status: 0 }
}

fn encryption_status(driver: &mut dyn OracleDriver, lines: &mut Vec<String>) {
    let warn = |lines: &mut Vec<String>, error: &DbError| {
        lines.push(format!(
            "  WARN: Cannot check encryption \u{2014} {}",
            ora(error)
        ));
        lines.push("  (This may require additional privileges)".to_string());
    };
    let protocol = match driver.query(Probe::Protocol, SQL_PROTOCOL) {
        Ok(rows) => rows
            .first()
            .and_then(|row| row.first())
            .cloned()
            .unwrap_or(None),
        Err(error) => {
            warn(lines, &error);
            return;
        }
    };
    let shown = protocol.as_deref().map_or_else(|| "None".to_string(), safe);
    lines.push(format!("  Network protocol: {shown}"));
    match protocol.as_deref().map(str::to_lowercase).as_deref() {
        Some("tcps") => {
            lines.push("  PASS: Connection is SSL/TLS encrypted (TCPS)".to_string());
        }
        Some("tcp") => {
            lines.push("  INFO: Connection is TCP (check if NNE is active below)".to_string());
        }
        _ => lines.push(format!("  INFO: Protocol is '{shown}'")),
    }
    match driver.query(Probe::Banners, SQL_BANNERS) {
        Ok(banners) if !banners.is_empty() => {
            for banner in &banners {
                lines.push(format!("  Banner: {}", cell(banner, 0)));
                let lowered = cell(banner, 0).to_lowercase();
                if lowered.contains("encryption") || lowered.contains("crypto") {
                    lines.push("  PASS: Encryption is active (NNE or SSL)".to_string());
                }
            }
        }
        Ok(_) => lines.push(
            "  INFO: No encryption banners found \u{2014} connection may be unencrypted"
                .to_string(),
        ),
        Err(error) => warn(lines, &error),
    }
}
