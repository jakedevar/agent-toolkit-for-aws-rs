//! Every size, work and time bound of the proxy (design section 4.5) in one
//! place. Production uses [`Limits::default`]; tests inject smaller values
//! through [`Limits::new`], which checks the bounds are consistent.

use std::time::Duration;

use toolkit_core::json::MAX_JSON_DEPTH;
use toolkit_core::{Json, JsonLimits};

const MIB: usize = 1024 * 1024;

/// Bytes charged per possible JSON node before a parse (R-MEM): twice the
/// size of an object member, which covers `Vec` growth. 112 B with today's
/// layout; a test pins it.
pub const NODE_CHARGE: usize = 2 * std::mem::size_of::<(String, Json)>();

/// Longest proxy-generated control line (a fixed template plus a bounded id
/// and tool name), newline excluded.
pub const MAX_CONTROL_LINE: usize = 1024;

/// Longest JSON-RPC id the proxy accepts from the client, in compact JSON
/// bytes. Bounding it keeps every control line within [`MAX_CONTROL_LINE`].
pub const MAX_ID_BYTES: usize = 256;

/// Bytes of an oversize stdin line kept to recover its `id`.
pub const ID_RECOVERY_PREFIX: usize = 4096;

/// The bounds. Fields are private so every instance has passed
/// [`Limits::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub(crate) max_line: usize,
    pub(crate) max_message: usize,
    pub(crate) max_sse_line: usize,
    pub(crate) max_depth: usize,
    pub(crate) max_nodes: usize,
    pub(crate) budget: usize,
    pub(crate) control_reserve: usize,
    pub(crate) max_in_flight: usize,
    pub(crate) data_queue_lines: usize,
    pub(crate) control_queue_lines: usize,
    pub(crate) max_chunk: usize,
    pub(crate) request_timeout: Duration,
    pub(crate) tool_timeout: Duration,
    pub(crate) get_reopen_interval: Duration,
}

impl Default for Limits {
    /// The section 4.5 values: 8 MiB lines, messages and SSE lines, depth 128,
    /// 262,144 nodes, a 96 MiB budget, a 1 MiB control reserve, 16 requests in
    /// flight, 64-line queues, 180 s per request, 300 s per tool call and one
    /// `GET` stream reopen per 30 s.
    fn default() -> Self {
        Self {
            max_line: 8 * MIB,
            max_message: 8 * MIB,
            max_sse_line: 8 * MIB,
            max_depth: MAX_JSON_DEPTH,
            max_nodes: 262_144,
            budget: 96 * MIB,
            control_reserve: MIB,
            max_in_flight: 16,
            data_queue_lines: 64,
            control_queue_lines: 64,
            max_chunk: 64 * 1024,
            request_timeout: Duration::from_secs(180),
            tool_timeout: Duration::from_secs(300),
            get_reopen_interval: Duration::from_secs(30),
        }
    }
}

/// Why [`Limits::new`] refused a set of bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitsError(&'static str);

impl std::fmt::Display for LimitsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for LimitsError {}

/// The injectable bounds, for [`Limits::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_field_names)]
pub struct LimitsSpec {
    /// Longest stdin line.
    pub max_line: usize,
    /// Longest upstream message (one JSON body or one SSE event).
    pub max_message: usize,
    /// Longest SSE line.
    pub max_sse_line: usize,
    /// Greatest JSON nesting.
    pub max_depth: usize,
    /// Most JSON nodes per message.
    pub max_nodes: usize,
    /// The shared byte budget (R-MEM).
    pub budget: usize,
    /// The separate control reserve.
    pub control_reserve: usize,
    /// Most requests in flight.
    pub max_in_flight: usize,
    /// Most lines in the stdout data queue.
    pub data_queue_lines: usize,
    /// Most lines in the stdout control queue.
    pub control_queue_lines: usize,
    /// Largest body chunk read at once.
    pub max_chunk: usize,
    /// Deadline of a request other than `tools/call`.
    pub request_timeout: Duration,
    /// Deadline of a `tools/call`.
    pub tool_timeout: Duration,
    /// Least time between two `GET` stream opens.
    pub get_reopen_interval: Duration,
}

impl From<Limits> for LimitsSpec {
    fn from(limits: Limits) -> Self {
        Self {
            max_line: limits.max_line,
            max_message: limits.max_message,
            max_sse_line: limits.max_sse_line,
            max_depth: limits.max_depth,
            max_nodes: limits.max_nodes,
            budget: limits.budget,
            control_reserve: limits.control_reserve,
            max_in_flight: limits.max_in_flight,
            data_queue_lines: limits.data_queue_lines,
            control_queue_lines: limits.control_queue_lines,
            max_chunk: limits.max_chunk,
            request_timeout: limits.request_timeout,
            tool_timeout: limits.tool_timeout,
            get_reopen_interval: limits.get_reopen_interval,
        }
    }
}

impl Limits {
    /// Checked bounds.
    ///
    /// # Errors
    /// [`LimitsError`] when a bound is zero, the depth exceeds
    /// [`MAX_JSON_DEPTH`], the control reserve cannot hold one control line,
    /// or the budget cannot hold the largest line or message with its parse
    /// charge (the proxy could never make progress).
    pub fn new(spec: LimitsSpec) -> Result<Self, LimitsError> {
        let positive = [
            spec.max_line,
            spec.max_message,
            spec.max_sse_line,
            spec.max_depth,
            spec.max_nodes,
            spec.budget,
            spec.control_reserve,
            spec.max_in_flight,
            spec.data_queue_lines,
            spec.control_queue_lines,
            spec.max_chunk,
        ];
        if positive.contains(&0)
            || spec.request_timeout.is_zero()
            || spec.tool_timeout.is_zero()
            || spec.get_reopen_interval.is_zero()
        {
            return Err(LimitsError("every proxy limit must be positive"));
        }
        if spec.max_depth > MAX_JSON_DEPTH {
            return Err(LimitsError(
                "the JSON depth limit is above the parser's maximum",
            ));
        }
        if spec.control_reserve < MAX_CONTROL_LINE + 1 {
            return Err(LimitsError(
                "the control reserve cannot hold one control line",
            ));
        }
        let limits = Self {
            max_line: spec.max_line,
            max_message: spec.max_message,
            max_sse_line: spec.max_sse_line,
            max_depth: spec.max_depth,
            max_nodes: spec.max_nodes,
            budget: spec.budget,
            control_reserve: spec.control_reserve,
            max_in_flight: spec.max_in_flight,
            data_queue_lines: spec.data_queue_lines,
            control_queue_lines: spec.control_queue_lines,
            max_chunk: spec.max_chunk,
            request_timeout: spec.request_timeout,
            tool_timeout: spec.tool_timeout,
            get_reopen_interval: spec.get_reopen_interval,
        };
        let largest = spec.max_line.max(spec.max_message);
        if limits.peak_charge(largest) > spec.budget {
            return Err(LimitsError(
                "the byte budget cannot hold the largest message and its parse charge",
            ));
        }
        Ok(limits)
    }

    /// The JSON parser limits for one message.
    #[must_use]
    pub fn json(&self) -> JsonLimits {
        JsonLimits {
            max_depth: self.max_depth,
            max_nodes: self.max_nodes,
        }
    }

    /// The parse charge of a `bytes`-long message (R-MEM): `NODE_CHARGE`
    /// for each node it could hold (`bytes / 2 + 1`, at most the node cap)
    /// plus `bytes` for its owned strings.
    #[must_use]
    pub fn parse_charge(&self, bytes: usize) -> usize {
        let nodes = self.max_nodes.min(bytes / 2 + 1);
        NODE_CHARGE.saturating_mul(nodes).saturating_add(bytes)
    }

    /// The most a `bytes`-long compact message holds at once: its parse
    /// charge plus its output line (`bytes` and a newline). The raw bytes
    /// (`bytes`) are released after the parse, before the output line is
    /// reserved, so they never add to the output.
    #[must_use]
    pub fn peak_charge(&self, bytes: usize) -> usize {
        self.parse_charge(bytes)
            .saturating_add(bytes)
            .saturating_add(1)
    }

    /// The shared byte budget.
    #[must_use]
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// The control reserve.
    #[must_use]
    pub fn control_reserve(&self) -> usize {
        self.control_reserve
    }

    /// The deadline of a `tools/call`.
    #[must_use]
    pub fn tool_timeout(&self) -> Duration {
        self.tool_timeout
    }

    /// The deadline of any other request.
    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }
}
