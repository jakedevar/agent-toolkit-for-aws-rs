//! Aurora ACU, commitment and I/O pricing analyzers (#4388).
//!
//! One calculation core replaces the six near-identical Python scripts
//! (`acu_calculator.py`, `commitment_pricing_analyzer.py`,
//! `io_optimized_analyzer.py` for `Aurora MySQL` and `Aurora PostgreSQL`). The
//! engine only changes the Pricing API engine name, the RDS engine and a few
//! messages, so it is the `--engine` option of three commands:
//!
//! - `toolkit aurora acu estimate | list-instances | pricing-source`
//! - `toolkit aurora commitment [offline]`
//! - `toolkit aurora io [offline]`
//!
//! Every remote read goes through [`api::AwsApi`]; this build wires [`api::NoAws`]
//! (the "no `boto3`" behaviour of the scripts). See `docs/migration/aurora.md`
//! for the parity table and the intentional changes.
// The analyzers port floating point arithmetic one-to-one from Python, so the
// numeric casts, exact float comparisons (`> 0`, `== 0`) and long formatting
// functions of that port are deliberate.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::float_cmp,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::needless_pass_by_value
)]

use toolkit_core::ToolkitError;

use crate::{Invocation, Output};

mod acu;
mod api;
mod args;
mod commitment;
mod io;
mod pyfmt;

#[cfg(test)]
mod tests;

/// Hours in the billing month the scripts use.
pub(crate) const HOURS_PER_MONTH: f64 = 730.0;
/// I/O-Optimized compute premium.
pub(crate) const IO_OPT_COMPUTE_MULTIPLIER: f64 = 1.30;
/// Serverless v2 on-demand price, USD per ACU-hour (us-east-1, Standard).
pub(crate) const STATIC_ACU_PRICE_STANDARD: f64 = 0.12;
/// Serverless v2 I/O-Optimized price per ACU-hour.
pub(crate) const STATIC_ACU_PRICE_IO_OPTIMIZED: f64 = 0.156;
/// Standard storage, USD per GiB-month.
pub(crate) const STATIC_STORAGE_STANDARD_PER_GIB: f64 = 0.10;
/// I/O-Optimized storage, USD per GiB-month.
pub(crate) const STATIC_STORAGE_IO_OPT_PER_GIB: f64 = 0.225;

/// (instance class, CPU count, memory GiB, on-demand USD/hour) for us-east-1 Aurora
/// Standard; shared by the ACU, commitment and I/O analyzers (the three Python
/// tables are identical).
pub(crate) const STATIC_INSTANCES: &[(&str, i64, i64, f64)] = &[
    ("db.t3.medium", 2, 4, 0.082),
    ("db.t3.large", 2, 8, 0.164),
    ("db.t4g.medium", 2, 4, 0.073),
    ("db.t4g.large", 2, 8, 0.146),
    ("db.r5.large", 2, 16, 0.290),
    ("db.r5.xlarge", 4, 32, 0.580),
    ("db.r5.2xlarge", 8, 64, 1.160),
    ("db.r5.4xlarge", 16, 128, 2.320),
    ("db.r5.8xlarge", 32, 256, 4.640),
    ("db.r5.12xlarge", 48, 384, 6.960),
    ("db.r5.16xlarge", 64, 512, 9.280),
    ("db.r5.24xlarge", 96, 768, 13.920),
    ("db.r6g.large", 2, 16, 0.260),
    ("db.r6g.xlarge", 4, 32, 0.519),
    ("db.r6g.2xlarge", 8, 64, 1.038),
    ("db.r6g.4xlarge", 16, 128, 2.076),
    ("db.r6g.8xlarge", 32, 256, 4.152),
    ("db.r6g.12xlarge", 48, 384, 6.228),
    ("db.r6g.16xlarge", 64, 512, 8.304),
    ("db.r7g.large", 2, 16, 0.276),
    ("db.r7g.xlarge", 4, 32, 0.553),
    ("db.r7g.2xlarge", 8, 64, 1.106),
    ("db.r7g.4xlarge", 16, 128, 2.211),
    ("db.r7g.8xlarge", 32, 256, 4.422),
    ("db.r7g.12xlarge", 48, 384, 6.633),
    ("db.r7g.16xlarge", 64, 512, 8.844),
    ("db.r8g.large", 2, 16, 0.276),
    ("db.r8g.xlarge", 4, 32, 0.552),
    ("db.r8g.2xlarge", 8, 64, 1.104),
    ("db.r8g.4xlarge", 16, 128, 2.208),
    ("db.r8g.8xlarge", 32, 256, 4.416),
    ("db.r8g.12xlarge", 48, 384, 6.624),
    ("db.r8g.16xlarge", 64, 512, 8.832),
    ("db.r8g.24xlarge", 96, 768, 13.248),
    ("db.r8g.48xlarge", 192, 1536, 26.496),
];

/// (region code, Pricing API location, in the I/O analyzer map, in the commitment
/// analyzer map). The ACU calculator lists every row. The Python scripts
/// carry three slightly different maps; the flags keep each one exact.
const REGIONS: &[(&str, &str, bool, bool)] = &[
    ("us-east-1", "US East (N. Virginia)", true, true),
    ("us-east-2", "US East (Ohio)", true, true),
    ("us-west-1", "US West (N. California)", true, true),
    ("us-west-2", "US West (Oregon)", true, true),
    ("eu-west-1", "EU (Ireland)", true, true),
    ("eu-west-2", "EU (London)", true, true),
    ("eu-west-3", "EU (Paris)", false, false),
    ("eu-central-1", "EU (Frankfurt)", true, true),
    ("eu-north-1", "EU (Stockholm)", true, true),
    ("ap-southeast-1", "Asia Pacific (Singapore)", true, true),
    ("ap-southeast-2", "Asia Pacific (Sydney)", true, true),
    ("ap-northeast-1", "Asia Pacific (Tokyo)", true, true),
    ("ap-northeast-2", "Asia Pacific (Seoul)", false, false),
    ("ap-south-1", "Asia Pacific (Mumbai)", true, true),
    ("ca-central-1", "Canada (Central)", true, true),
    ("sa-east-1", "South America (Sao Paulo)", true, false),
];

/// The analyzer a Pricing API location is looked up for (each Python script
/// carried its own region map).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    Acu,
    Commitment,
    Io,
}

/// Pricing API location name for `region`, as the given tool's map lists it.
pub(crate) fn location(tool: Tool, region: &str) -> Option<&'static str> {
    REGIONS
        .iter()
        .find(|(code, _, in_io, in_commitment)| {
            *code == region
                && match tool {
                    Tool::Acu => true,
                    Tool::Io => *in_io,
                    Tool::Commitment => *in_commitment,
                }
        })
        .map(|(_, name, _, _)| *name)
}

/// Region codes the ACU calculator lists, sorted.
pub(crate) fn acu_regions() -> Vec<&'static str> {
    let mut codes: Vec<&'static str> = REGIONS.iter().map(|(code, ..)| *code).collect();
    codes.sort_unstable();
    codes
}

/// Static on-demand hourly price of an instance class.
pub(crate) fn static_price(instance_type: &str) -> Option<f64> {
    STATIC_INSTANCES
        .iter()
        .find(|(name, ..)| *name == instance_type)
        .map(|(_, _, _, price)| *price)
}

/// The Aurora engine an invocation analyzes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Engine {
    Mysql,
    Postgresql,
}

impl Engine {
    /// Reads the required `--engine mysql|postgresql`.
    pub(crate) fn from_invocation(invocation: &Invocation) -> Result<Self, ToolkitError> {
        match invocation.value("--engine") {
            Some("mysql") => Ok(Self::Mysql),
            Some("postgresql") => Ok(Self::Postgresql),
            Some(other) => Err(args::usage(format!(
                "argument --engine: invalid choice: {} (choose from 'mysql', 'postgresql')",
                crate::manifests::py_repr_str(other)
            ))),
            None => Err(args::usage(
                "the following arguments are required: --engine",
            )),
        }
    }

    /// Pricing API / Savings Plans `databaseEngine` value.
    pub(crate) fn pricing_name(self) -> &'static str {
        match self {
            Self::Mysql => "Aurora MySQL",
            Self::Postgresql => "Aurora PostgreSQL",
        }
    }

    /// RDS API engine name.
    pub(crate) fn rds_engine(self) -> &'static str {
        match self {
            Self::Mysql => "aurora-mysql",
            Self::Postgresql => "aurora-postgresql",
        }
    }

    /// Skip reason of a cluster without instances (commitment analyzer).
    pub(crate) fn commitment_empty_reason(self) -> &'static str {
        match self {
            Self::Mysql => {
                "Cluster has no DB instances — no compute to price. \
                 RI and DSP commitment analysis does not apply. \
                 This typically indicates the last instance was deleted, a paused \
                 cluster, or a cluster mid-migration."
            }
            Self::Postgresql => {
                "Cluster has no DB instances — no compute to price. \
                 RI and DSP commitment analysis does not apply. \
                 This typically indicates Aurora Limitless, a paused cluster, \
                 or a cluster mid-migration."
            }
        }
    }

    /// Skip reason of a cluster without instances (I/O analyzer).
    pub(crate) fn io_empty_reason(self) -> &'static str {
        match self {
            Self::Mysql => {
                "Cluster has no DB instances — no compute to analyze. \
                 This usually means the cluster's last writer/reader instance \
                 was deleted. The Standard vs I/O-Optimized comparison does not \
                 apply until the cluster has a running instance."
            }
            Self::Postgresql => {
                "Cluster has no DB instances — no compute to analyze. \
                 This usually means the cluster's last writer/reader instance \
                 was deleted, or it is an Aurora Limitless cluster (locked to \
                 I/O-Optimized, different pricing model). The Standard vs \
                 I/O-Optimized comparison does not apply."
            }
        }
    }

    /// Table cell of a skipped cluster in the I/O fleet table and its width.
    pub(crate) fn io_skipped_cell(self) -> (&'static str, usize) {
        match self {
            Self::Mysql => ("skipped (no instances)", 22),
            Self::Postgresql => ("skipped (limitless)", 20),
        }
    }
}

/// `toolkit aurora acu estimate`.
pub(crate) fn acu_estimate_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    acu::estimate_command(invocation, output, &api::NoAws)
}

/// `toolkit aurora acu list-instances`.
pub(crate) fn acu_list_instances_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    acu::list_instances_command(invocation, output, &api::NoAws)
}

/// `toolkit aurora acu pricing-source`.
pub(crate) fn acu_pricing_source_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    acu::pricing_source_command(invocation, output, &api::NoAws)
}

/// `toolkit aurora acu` without a subcommand prints the help.
pub(crate) fn acu_help_command(
    _invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    output.line(crate::USAGE)
}

/// `toolkit aurora commitment`.
pub(crate) fn commitment_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    commitment::command(invocation, output, &api::NoAws)
}

/// `toolkit aurora io`.
pub(crate) fn io_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    io::command(invocation, output, &api::NoAws, io::system_now())
}
