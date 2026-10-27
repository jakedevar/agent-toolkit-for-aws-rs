//! Command classification for Serverless ECPU estimation (port of
//! `command_classifier.py`).
//!
//! Serverless bills ECPUs from vCPU time and data transferred. The split used
//! here is a heuristic, not a published AWS formula:
//!
//! - **fixed** commands cost one ECPU per call;
//! - **nonfixed** commands are estimated as `max(calls, usec / 3)` ECPUs (one
//!   ECPU is roughly three microseconds of vCPU time);
//! - **internal** commands (AUTH, MULTI, INFO, replication and cluster
//!   internals, ...) are not metered and are excluded.
//!
//! Anything in neither list is treated as nonfixed.

use super::text::{round_int, RoundError};

/// Simple O(1) commands, one ECPU per call.
pub const FIXED_COMMANDS: &[&str] = &[
    "get",
    "set",
    "hget",
    "hset",
    "incr",
    "decr",
    "incrby",
    "decrby",
    "incrbyfloat",
    "expire",
    "pexpire",
    "pexpireat",
    "expireat",
    "persist",
    "exists",
    "ttl",
    "pttl",
    "type",
    "strlen",
    "scard",
    "zcard",
    "llen",
    "xlen",
    "sismember",
    "hexists",
    "hlen",
    "hsetnx",
    "hincrby",
    "hincrbyfloat",
    "getbit",
    "setbit",
    "setnx",
    "setex",
    "psetex",
    "zscore",
    "ping",
    "del",
    "unlink",
    "select",
    "echo",
    "time",
    "quit",
    "reset",
    "watch",
    "unwatch",
    "move",
    "asking",
    "readonly",
    "readwrite",
    "acl",
    "client",
    "command",
];

/// Commands serverless does not meter.
pub const INTERNAL_COMMANDS: &[&str] = &[
    "auth",
    "multi",
    "exec",
    "hello",
    "discard",
    "subscribe",
    "unsubscribe",
    "psubscribe",
    "punsubscribe",
    "publish",
    "pubsub",
    "info",
    "config",
    "cluster",
    "replconf",
    "psync",
    "replicaof",
    "slowlog",
    "dbsize",
    "wait",
    "object",
    "debug",
    "memory",
    "latency",
    "module",
    "function",
    "swapdb",
];

/// How a command is metered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass {
    /// One ECPU per call.
    Fixed,
    /// `max(calls, usec / 3)` ECPUs.
    Nonfixed,
    /// Not metered.
    Internal,
}

impl CommandClass {
    /// The label the Python used (`fixed`, `nonfixed`, `internal`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Nonfixed => "nonfixed",
            Self::Internal => "internal",
        }
    }
}

/// Classifies a command name (case-insensitive, surrounding blanks ignored).
#[must_use]
pub fn classify_command(name: &str) -> CommandClass {
    let command = name.trim().to_lowercase();
    if INTERNAL_COMMANDS.contains(&command.as_str()) {
        CommandClass::Internal
    } else if FIXED_COMMANDS.contains(&command.as_str()) {
        CommandClass::Fixed
    } else {
        CommandClass::Nonfixed
    }
}

/// `calls` and `usec` of one command (from `INFO commandstats`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandStat {
    /// Number of calls.
    pub calls: i64,
    /// Total microseconds of CPU time.
    pub usec: i64,
}

/// One row of the per-command breakdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakdownEntry {
    /// The command as it was given.
    pub command: String,
    /// How it was metered.
    pub class: CommandClass,
    /// Number of calls.
    pub calls: i64,
    /// Microseconds of CPU time.
    pub usec: i64,
    /// Estimated ECPUs (`0` for internal commands).
    pub ecpus: i64,
}

/// Result of [`estimate_ecpus_from_commandstats`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EcpuEstimate {
    /// `round(fixed + nonfixed)`.
    pub total_ecpus: i64,
    /// ECPUs of the fixed commands.
    pub fixed_ecpus: i64,
    /// `round()` of the nonfixed estimate.
    pub nonfixed_ecpus: i64,
    /// Calls excluded because they are internal.
    pub internal_calls: i64,
    /// Per-command detail, most ECPUs first.
    pub command_breakdown: Vec<BreakdownEntry>,
}

/// Estimates ECPUs from commandstats given as `(command, stat)` in file order.
///
/// # Errors
///
/// A total that is not a finite integer (Python `round` raised).
pub fn estimate_ecpus_from_commandstats(
    commandstats: &[(String, CommandStat)],
) -> Result<EcpuEstimate, RoundError> {
    let mut fixed_ecpus = 0.0_f64;
    let mut nonfixed_ecpus = 0.0_f64;
    let mut internal_calls = 0_i64;
    let mut breakdown = Vec::new();

    for (command, stat) in commandstats {
        let class = classify_command(command);
        let ecpus = match class {
            CommandClass::Internal => {
                internal_calls = internal_calls.saturating_add(stat.calls);
                0
            }
            CommandClass::Fixed => {
                fixed_ecpus += stat.calls as f64;
                stat.calls
            }
            CommandClass::Nonfixed => {
                let calls = stat.calls as f64;
                let by_cpu = stat.usec as f64 / 3.0;
                let estimate = if by_cpu > calls { by_cpu } else { calls };
                nonfixed_ecpus += estimate;
                round_int(estimate)?
            }
        };
        breakdown.push(BreakdownEntry {
            command: command.clone(),
            class,
            calls: stat.calls,
            usec: stat.usec,
            ecpus,
        });
    }

    breakdown.sort_by(|a, b| b.ecpus.cmp(&a.ecpus));
    Ok(EcpuEstimate {
        total_ecpus: round_int(fixed_ecpus + nonfixed_ecpus)?,
        fixed_ecpus: round_int(fixed_ecpus)?,
        nonfixed_ecpus: round_int(nonfixed_ecpus)?,
        internal_calls,
        command_breakdown: breakdown,
    })
}
