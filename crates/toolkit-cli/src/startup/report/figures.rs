//! The cost-figure cross-check: every `data-cost-key` anchor of the report
//! must render the dollar amount `estimation-infra.json` records for it.

use toolkit_core::Json;

use super::rx::{group, rx};
use super::values::{chain, py_float, untrusted};
use super::walk::{cost_anchor_matches, section_html};
use crate::manifests::py_repr;

/// A `data-cost-key` anchor and the estimate path whose figure it must equal.
pub(super) type CostAnchor = (&'static str, [&'static str; 2]);

/// Which estimate figure each `data-cost-key` anchor must equal.
const COST_ANCHORS: [CostAnchor; 4] = [
    (
        "aws_monthly_balanced",
        ["projected_costs", "aws_monthly_balanced"],
    ),
    (
        "aws_monthly_premium",
        ["projected_costs", "aws_monthly_premium"],
    ),
    (
        "aws_monthly_optimized",
        ["projected_costs", "aws_monthly_optimized"],
    ),
    ("current_monthly", ["current_costs", "gcp_monthly"]),
];

/// Figures that must be anchored in `exec-costs` when the estimate has them.
const REQUIRED_COST_KEYS: [&str; 2] = ["aws_monthly_balanced", "current_monthly"];

/// `_canonical_money`: monthly-scale totals (at least two dollars once
/// rounded, halves to even) are whole dollars, smaller ones keep two decimals.
/// `None` when the value is not a finite number.
pub(super) fn canonical_money(value: f64) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    let rounded = value.round_ties_even();
    if rounded.abs() >= 2.0 {
        Some(format!("{rounded:.0}"))
    } else {
        Some(format!("{value:.2}"))
    }
}

/// `_normalize_money`: the first dollar amount of a rendered string in its
/// canonical form, or `None` when there is none.
pub(super) fn normalize_money(text: &str) -> Option<String> {
    let groups = rx!(r"\$\s*([0-9][0-9,]*(?:\.[0-9]+)?)").captures(text)?;
    let digits = group(text, &groups, 1).replace(',', "");
    canonical_money(digits.parse::<f64>().ok()?)
}

/// `_dig`: the member at the path, `None` when a step is missing.
fn dig<'a>(value: &'a Json, path: &[&str]) -> Option<&'a Json> {
    chain(value, path).filter(|found| !matches!(found, Json::Null))
}

/// The figures of the report against the estimate (P1-C).
pub(super) fn cost_figures(html: &str, infra: Option<&Json>, require_anchors: bool) -> Vec<String> {
    let exec_costs = if require_anchors {
        section_html(html, "exec-costs")
    } else {
        None
    };
    check_cost_figures(
        html,
        infra,
        &COST_ANCHORS,
        &REQUIRED_COST_KEYS,
        exec_costs.as_deref(),
    )
}

/// [`cost_figures`] for any anchor table: every rendered anchor of `html`
/// must equal its figure, and each `required` key with a figure in the estimate
/// must be anchored inside `exec_costs` (the `exec-costs` section the caller
/// found, `None` to skip that requirement).
pub(super) fn check_cost_figures(
    html: &str,
    infra: Option<&Json>,
    anchors: &[CostAnchor],
    required: &[&str],
    exec_costs: Option<&str>,
) -> Vec<String> {
    let Some(infra) = infra.filter(|value| matches!(value, Json::Object(_))) else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    for (key, text) in cost_anchor_matches(html) {
        let Some((_, path)) = anchors.iter().find(|(name, _)| *name == key) else {
            continue;
        };
        let Some(expected) = dig(infra, path) else {
            continue;
        };
        let dotted = path.join(".");
        let Some(expected_dollars) = py_float(expected).and_then(canonical_money) else {
            errors.push(format!(
                "estimation-infra.json {dotted} is not a numeric dollar amount: {}",
                untrusted(&py_repr(expected))
            ));
            continue;
        };
        let Some(rendered) = normalize_money(&text) else {
            errors.push(format!(
                "data-cost-key=\"{key}\" element renders no dollar amount (expected \
                 ${expected_dollars} from {dotted})"
            ));
            continue;
        };
        if expected_dollars != rendered {
            errors.push(format!(
                "cost figure mismatch: data-cost-key=\"{key}\" renders \"${rendered}\" but \
                 estimation-infra.json {dotted} = ${expected_dollars}"
            ));
        }
    }
    // Required figures are anchored inside <section id="exec-costs">.
    let Some(exec_costs) = exec_costs else {
        return errors;
    };
    let anchored: Vec<String> = cost_anchor_matches(exec_costs)
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    for key in required {
        let Some((_, path)) = anchors.iter().find(|(name, _)| name == key) else {
            continue;
        };
        if dig(infra, path).is_none() || anchored.iter().any(|anchor| anchor == key) {
            continue;
        }
        let dotted = path.join(".");
        errors.push(format!(
            "missing data-cost-key=\"{key}\" anchor inside <section id=\"exec-costs\">; cannot \
             confirm the rendered figure matches estimation-infra.json {dotted} (wrap that \
             figure in <span data-cost-key=\"{key}\">...</span> inside exec-costs)"
        ));
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_is_rounded_to_even_dollars_or_kept_in_cents() {
        assert_eq!(canonical_money(112.90).as_deref(), Some("113"));
        assert_eq!(canonical_money(112.4).as_deref(), Some("112"));
        assert_eq!(canonical_money(0.5).as_deref(), Some("0.50"));
        assert_eq!(canonical_money(2.5).as_deref(), Some("2"));
        assert_eq!(canonical_money(1.999).as_deref(), Some("2"));
        assert_eq!(canonical_money(0.4).as_deref(), Some("0.40"));
        assert_eq!(canonical_money(f64::NAN), None);
        assert_eq!(canonical_money(f64::INFINITY), None);
        assert_eq!(normalize_money("$1,415/mo").as_deref(), Some("1415"));
        assert_eq!(normalize_money("about $ 112.90").as_deref(), Some("113"));
        assert_eq!(normalize_money("no price"), None);
    }
}
