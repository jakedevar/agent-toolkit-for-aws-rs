//! Text report and orchestration of `toolkit msk size`.
//!
//! Output text matches the legacy script line for line: tables are Markdown,
//! money is `${:,.2f}`, and costs that are identical for every instance of a
//! section are hoisted into a "Shared monthly costs" block so the printed
//! columns add up.

use crate::pricing::{
    format_static_pricing_literals, pricing_provenance, resolve_pricing, PricingApi,
    STATIC_PRICING_CAPTURED,
};
use crate::pyfmt::{format_fixed, format_g, format_int_grouped, pad_left, pad_right, round_to};
use crate::sizing::{
    calculate_express_sizing, calculate_standard_sizing, recommend_per_class, BrokerGroup,
    SizingInputs, SizingResult, EBS_HEADROOM_FACTOR,
};

/// Everything `toolkit msk size` needs after argument parsing.
#[derive(Debug, Clone, PartialEq)]
pub struct SizeOptions {
    /// Region to price against.
    pub region: String,
    /// `auto`, `api` or `static`.
    pub pricing_source: String,
    /// Print the resolved prices as refresh literals instead of sizing.
    pub emit_static_pricing: bool,
    /// The workload; required unless `emit_static_pricing`.
    pub inputs: Option<SizingInputs>,
    /// PPA / EDP discount percentage in `[0, 100)`.
    pub discount_pct: f64,
    /// Selected broker classes.
    pub broker_classes: Vec<BrokerGroup>,
    /// Per-cluster broker quota used for the recommendations.
    pub broker_quota: i64,
}

/// A finished sizing run: stdout lines and stderr `WARNING:` messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizeRun {
    /// Lines for stdout, without line terminators (empty on failure).
    pub stdout: Vec<String>,
    /// Warning messages for stderr (each printed behind `WARNING: `).
    pub warnings: Vec<String>,
    /// Failure message printed behind `ERROR: ` with exit status 1.
    pub error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CostField {
    Storage,
    TieredStorage,
    Pst,
    ExpressDataIn,
    CrossAz,
}

/// Cost dimensions that may be identical across a section, in report order.
/// Broker cost is never hoisted: it scales with broker count and price.
const SHAREABLE_COST_FIELDS: [CostField; 5] = [
    CostField::Storage,
    CostField::TieredStorage,
    CostField::Pst,
    CostField::ExpressDataIn,
    CostField::CrossAz,
];

impl CostField {
    fn label(self) -> &'static str {
        match self {
            Self::Storage => "Storage",
            Self::TieredStorage => "Tiered Storage",
            Self::Pst => "PST",
            Self::ExpressDataIn => "Express data-in",
            Self::CrossAz => "Cross-AZ",
        }
    }

    fn value(self, result: &SizingResult) -> f64 {
        match self {
            Self::Storage => result.monthly_ebs_cost,
            Self::TieredStorage => result.monthly_ts_cost,
            Self::Pst => result.monthly_pst_cost,
            Self::ExpressDataIn => result.monthly_data_in_cost,
            Self::CrossAz => result.monthly_cross_az_cost,
        }
    }
}

/// Cost fields that are non-zero and identical (to 6 decimals) across every
/// result, with their rounded amount.
fn shared_costs(results: &[SizingResult]) -> Vec<(CostField, f64)> {
    let mut shared = Vec::new();
    for field in SHAREABLE_COST_FIELDS {
        let mut values = results.iter().map(|r| round_to(field.value(r), 6));
        let Some(first) = values.next() else {
            continue;
        };
        if values.all(|value| value == first) && first > 0.0 {
            shared.push((field, first));
        }
    }
    shared
}

/// Sum of the shared cost rows as displayed, so the printed column adds up.
fn shared_subtotal(shared: &[(CostField, f64)]) -> f64 {
    shared
        .iter()
        .fold(0.0, |sum, (_, value)| sum + round_to(*value, 2))
}

fn money(value: f64) -> String {
    format!("${}", format_fixed(value, 2, true))
}

fn format_summary_line(result: &SizingResult) -> String {
    format!(
        "{}: {} brokers (bottleneck: {}) \u{2192} {}/mo",
        result.instance_type,
        result.broker_count,
        result.bottleneck,
        money(result.total_monthly_cost)
    )
}

fn format_shared_costs(shared: &[(CostField, f64)]) -> Vec<String> {
    let mut lines: Vec<String> = shared
        .iter()
        .map(|(field, value)| {
            format!(
                "  {} ${}",
                pad_right(field.label(), 16),
                pad_left(&format_fixed(*value, 2, true), 14)
            )
        })
        .collect();
    lines.push(format!(
        "  {} ${}",
        pad_right("Subtotal", 16),
        pad_left(&format_fixed(shared_subtotal(shared), 2, true), 14)
    ));
    lines
}

/// One Markdown row per instance. `required brokers` is the maximum across
/// every constraint; `bottleneck` names the constraint that produced it.
fn format_broker_table(results: &[SizingResult], shared: &[(CostField, f64)]) -> Vec<String> {
    let mut headers = vec![
        "broker type".to_string(),
        "required brokers".to_string(),
        "bottleneck".to_string(),
        "broker cost / mo".to_string(),
    ];
    // Any non-zero cost dimension that varies across instances needs its own
    // column, otherwise broker cost plus shared costs would not add up.
    let per_instance_costs: Vec<CostField> = SHAREABLE_COST_FIELDS
        .iter()
        .copied()
        .filter(|field| {
            !shared.iter().any(|(shared_field, _)| shared_field == field)
                && results.iter().any(|r| field.value(r) > 0.0)
        })
        .collect();
    for field in &per_instance_costs {
        headers.push(format!("{} / mo", field.label()));
    }
    headers.push(if shared.is_empty() {
        "total cost / mo".to_string()
    } else {
        "total cost / mo incl. shared".to_string()
    });

    let mut lines = vec![
        format!("| {} |", headers.join(" | ")),
        format!("|{}|", vec!["---"; headers.len()].join("|")),
    ];
    for result in results {
        let mut cells = vec![
            result.instance_type.to_string(),
            result.broker_count.to_string(),
            result.bottleneck.to_string(),
            money(result.monthly_broker_cost),
        ];
        for field in &per_instance_costs {
            cells.push(money(field.value(result)));
        }
        cells.push(money(result.total_monthly_cost));
        lines.push(format!("| {} |", cells.join(" | ")));
    }
    lines
}

/// Appends `text` like Python's `print` (a trailing newline per call, so a
/// leading `\n` yields a blank line first).
fn print(lines: &mut Vec<String>, text: &str) {
    for line in text.split('\n') {
        lines.push(line.to_string());
    }
}

fn print_sizing_section(lines: &mut Vec<String>, title: &str, results: &[SizingResult]) {
    print(lines, &format!("\n=== {title} ==="));
    let shared = shared_costs(results);
    if !shared.is_empty() {
        print(
            lines,
            "\nShared monthly costs (same for every instance in this section):",
        );
        lines.extend(format_shared_costs(&shared));
        print(lines, "\nBroker sizing:");
    }
    lines.extend(format_broker_table(results, &shared));
}

fn print_recommendations(
    lines: &mut Vec<String>,
    results: &[SizingResult],
    broker_quota: i64,
    broker_classes: &[BrokerGroup],
) {
    let recommendations = recommend_per_class(results, broker_quota);
    print(
        lines,
        &format!(
            "\n=== Recommended pick per class (\u{2264} {broker_quota} brokers, lowest monthly cost) ==="
        ),
    );
    print(
        lines,
        "  Cost breakdown for each pick is in the sizing sections above.",
    );
    for group in broker_classes {
        for class in group.classes() {
            let recommendation = recommendations
                .iter()
                .find(|(candidate, _)| candidate == class)
                .and_then(|(_, result)| *result);
            let label = class.label();
            match recommendation {
                None => print(
                    lines,
                    &format!(
                        "  {label}: no instance fits within {broker_quota} brokers \u{2014} request a quota increase or pick a larger size"
                    ),
                ),
                Some(result) => print(lines, &format!("  {label}: {}", format_summary_line(result))),
            }
        }
    }
}

/// Echoes every setting that shaped the numbers. Printed once.
fn format_inputs_block(
    inputs: &SizingInputs,
    results: &[SizingResult],
    discount_pct: f64,
    region: &str,
    pricing_source: &str,
) -> Vec<String> {
    let has_storage = results
        .iter()
        .any(|r| r.bottleneck_details.iter().any(|d| d.name == "storage"));
    let has_pst = results
        .iter()
        .any(|r| r.bottleneck_details.iter().any(|d| d.name == "pst"));
    let has_standard = results.iter().any(|r| !r.instance_type.contains("Express"));
    let has_express = results.iter().any(|r| r.instance_type.contains("Express"));
    let partition_limit = if inputs.use_max_partitions {
        "maximum"
    } else {
        "recommended"
    };

    let mut rows: Vec<(String, String, String)> = vec![
        row(
            "average ingress",
            format_fixed(inputs.avg_data_in_mbs, 2, true),
            "MiB/s",
        ),
        row(
            "peak ingress",
            format_fixed(inputs.peak_data_in_mbs, 2, true),
            "MiB/s",
        ),
        row(
            "average egress",
            format_fixed(inputs.avg_data_out_mbs, 2, true),
            "MiB/s",
        ),
        row(
            "peak egress",
            format_fixed(inputs.peak_data_out_mbs, 2, true),
            "MiB/s",
        ),
        row(
            "partitions",
            format_int_grouped(inputs.num_partitions),
            &format!(
                "total including replicas, sized against the {partition_limit} per-broker limit"
            ),
        ),
        row(
            "retention",
            format_int_grouped(inputs.retention_hours),
            &format!(
                "hours total, of which {} are primary",
                format_int_grouped(inputs.primary_retention_hours)
            ),
        ),
    ];

    if has_standard {
        rows.push(row(
            "replication factor",
            inputs.replication_factor.to_string(),
            "Standard; Express always replicates 3 ways",
        ));
        rows.push(row(
            "utilization",
            format_fixed(inputs.utilization_standard, 2, false),
            "capacity buffer for headroom (Standard)",
        ));
    }
    if has_express {
        rows.push(row(
            if has_standard {
                "utilization (Express)"
            } else {
                "utilization"
            },
            format_fixed(inputs.utilization_express, 2, false),
            "capacity buffer for headroom (Express)",
        ));
    }

    if has_storage {
        let storage_gb = results
            .iter()
            .flat_map(|r| r.bottleneck_details.iter())
            .find(|d| d.name == "storage")
            .map_or(0.0, |d| d.demand);
        rows.push(row(
            "storage",
            format_fixed(storage_gb, 2, true),
            &format!(
                "GiB EBS primary (average ingress x {}h primary retention x RF{} x {} headroom)",
                inputs.primary_retention_hours,
                inputs.replication_factor,
                format_g(EBS_HEADROOM_FACTOR)
            ),
        ));
    }
    if let (true, Some(pst)) = (has_pst, inputs.pst_per_broker_mbs) {
        rows.push(row(
            "provisioned storage throughput",
            format_fixed(pst, 2, true),
            "MiB/s per broker requested, clamped to each instance's EBS max; \
             sized against average egress",
        ));
    }

    rows.push(row(
        "consumer rack awareness",
        if inputs.rack_affined_consumers {
            "yes"
        } else {
            "no"
        },
        if inputs.rack_affined_consumers {
            "consumers fetch from local-AZ replicas, so cross-AZ cost excludes consumer fetch"
        } else {
            "consumers fetch across AZs, so cross-AZ cost includes consumer fetch"
        },
    ));
    let discount_text = format_g(discount_pct);
    rows.push(if discount_pct == 0.0 {
        row(
            "pricing",
            "list".to_string(),
            &format!("{region} public on-demand, no negotiated discount applied"),
        )
    } else {
        row(
            "pricing",
            format!("-{discount_text}%"),
            &format!(
                "{region} on-demand with a {discount_text}% negotiated discount \
                 applied to every cost dimension"
            ),
        )
    });
    rows.push(row(
        "pricing source",
        pricing_source.to_string(),
        &if pricing_source == "static" {
            format!(
                "embedded {} rates captured {STATIC_PRICING_CAPTURED}; \
                 no live lookup, may be stale",
                crate::pricing::STATIC_PRICING_REGION
            )
        } else {
            format!("live AWS Pricing API lookup for {region}")
        },
    ));

    let label_width = rows
        .iter()
        .map(|(label, _, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    let value_width = rows
        .iter()
        .map(|(_, value, _)| value.chars().count())
        .max()
        .unwrap_or(0);
    rows.iter()
        .map(|(label, value, note)| {
            format!(
                "  {}  {}  {note}",
                pad_right(label, label_width),
                pad_left(value, value_width)
            )
        })
        .collect()
}

fn row(label: &str, value: impl Into<String>, note: &str) -> (String, String, String) {
    (label.to_string(), value.into(), note.to_string())
}

/// The note printed before any figure, naming the price provenance.
#[must_use]
pub fn format_pricing_banner(discount_pct: f64, region: &str, source: &str) -> String {
    let provenance = pricing_provenance(source);
    if discount_pct == 0.0 {
        format!(
            "\nNOTE: all cost figures below use {region} public on-demand pricing,\n\
             {provenance}; other regions will differ.\n\
             If your organization has a Private Pricing Agreement (PPA), Enterprise Discount\n\
             Program (EDP), or other negotiated pricing, your actual costs will differ. Contact\n\
             your AWS account team for pricing that reflects your agreements, then re-run with\n\
             --discount-pct to model them."
        )
    } else {
        format!(
            "\nNOTE: cost figures below apply a {}% negotiated discount to every cost\n\
             dimension, based on {region} on-demand pricing {provenance}.\n\
             Confirm the discount against your agreement with your AWS account team before sharing\n\
             these figures.",
            format_g(discount_pct)
        )
    }
}

/// Runs `toolkit msk size`: resolves pricing, sizes the selected broker
/// classes and renders the report (or the refresh literals).
///
/// `today` is the `YYYY-MM-DD` capture date used by `--emit-static-pricing`.
/// A pricing failure or an invalid workload is reported in [`SizeRun::error`];
/// warnings raised before the failure are kept.
#[must_use]
pub fn run_size(options: &SizeOptions, api: &dyn PricingApi, today: &str) -> SizeRun {
    let mut warnings = Vec::new();
    match run_size_inner(options, api, today, &mut warnings) {
        Ok(stdout) => SizeRun {
            stdout,
            warnings,
            error: None,
        },
        Err(message) => SizeRun {
            stdout: Vec::new(),
            warnings,
            error: Some(message),
        },
    }
}

fn run_size_inner(
    options: &SizeOptions,
    api: &dyn PricingApi,
    today: &str,
    warnings: &mut Vec<String>,
) -> Result<Vec<String>, String> {
    let pricing = resolve_pricing(
        api,
        &options.region,
        &options.broker_classes,
        &options.pricing_source,
        warnings,
    )
    .map_err(|error| error.to_string())?;

    let mut lines = Vec::new();
    if options.emit_static_pricing {
        print(&mut lines, &format_static_pricing_literals(&pricing, today));
        return Ok(lines);
    }

    let inputs = options
        .inputs
        .as_ref()
        .ok_or_else(|| "the sizing workload arguments are required".to_string())?;
    inputs.validate()?;
    let partition_mode = if inputs.use_max_partitions {
        "maximum"
    } else {
        "recommended"
    };

    let standard_results = if options.broker_classes.contains(&BrokerGroup::Standard) {
        calculate_standard_sizing(inputs, &pricing, options.discount_pct)?
    } else {
        Vec::new()
    };
    let express_results = if options.broker_classes.contains(&BrokerGroup::Express) {
        calculate_express_sizing(inputs, &pricing, options.discount_pct)?
    } else {
        Vec::new()
    };
    let mut all_results = standard_results.clone();
    all_results.extend(express_results.iter().cloned());

    print(
        &mut lines,
        &format_pricing_banner(options.discount_pct, &options.region, pricing.source),
    );
    if !all_results.is_empty() {
        print(&mut lines, "\n=== Sizing inputs ===");
        lines.extend(format_inputs_block(
            inputs,
            &all_results,
            options.discount_pct,
            &options.region,
            pricing.source,
        ));
    }

    let suffix = format!("using {partition_mode} partition limits per broker");
    if !standard_results.is_empty() {
        print_sizing_section(
            &mut lines,
            &format!("Standard Sizing ({suffix})"),
            &standard_results,
        );
    }
    if !express_results.is_empty() {
        print_sizing_section(
            &mut lines,
            &format!("Express Sizing ({suffix})"),
            &express_results,
        );
    }
    print_recommendations(
        &mut lines,
        &all_results,
        options.broker_quota,
        &options.broker_classes,
    );

    Ok(lines)
}
