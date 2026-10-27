//! The curated per-1K-token USD rate table of `bedrock_pricing.py`
//! (`STATIC_FALLBACK`), row for row and in the same order.
//!
//! Order matters: the partial-match rules of `lookup` return the first row
//! that matches. Rates come from public pricing pages, cross-checked against
//! `skills/shared/ai/bedrock-pricing-cache.md` (verified 2026-10-05). Every Geo
//! prefix (`us.`, `eu.`) carries the ~10% Geo cross-Region premium over the bare
//! and `global.` rate; do not add a Geo prefix without pricing it here and do not
//! assume an unlisted prefix falls back to Global. Proprietary `OpenAI` GPT rows
//! are the short-context (272K) tier only: bare mantle ids and Geo profiles
//! (`us.`, `in.`) are 1.10x `OpenAI`'s standard list price (data-residency tier),
//! `global.` profiles are the standard price. No `in.`-prefixed GPT-6 row is
//! published. Re-check this table against the cache whenever either moves; the
//! test `the_rate_table_matches_the_python_source` compares it with
//! `bedrock_pricing.py` until that file is retired (#4415).

/// `(model id, input USD per 1K tokens, output USD per 1K tokens)`.
pub(super) const STATIC_FALLBACK: &[(&str, f64, f64)] = &[
    ("anthropic.claude-haiku-4-5-20251001-v1:0", 0.001, 0.005),
    (
        "us.anthropic.claude-haiku-4-5-20251001-v1:0",
        0.0011,
        0.0055,
    ),
    (
        "eu.anthropic.claude-haiku-4-5-20251001-v1:0",
        0.0011,
        0.0055,
    ),
    (
        "global.anthropic.claude-haiku-4-5-20251001-v1:0",
        0.001,
        0.005,
    ),
    ("anthropic.claude-sonnet-5", 0.002, 0.010),
    ("us.anthropic.claude-sonnet-5", 0.0022, 0.011),
    ("eu.anthropic.claude-sonnet-5", 0.0022, 0.011),
    ("global.anthropic.claude-sonnet-5", 0.002, 0.010),
    ("anthropic.claude-sonnet-4-6", 0.003, 0.015),
    ("us.anthropic.claude-sonnet-4-6", 0.0033, 0.0165),
    ("eu.anthropic.claude-sonnet-4-6", 0.0033, 0.0165),
    ("global.anthropic.claude-sonnet-4-6", 0.003, 0.015),
    ("anthropic.claude-opus-4-8", 0.005, 0.025),
    ("us.anthropic.claude-opus-4-8", 0.0055, 0.0275),
    ("eu.anthropic.claude-opus-4-8", 0.0055, 0.0275),
    ("global.anthropic.claude-opus-4-8", 0.005, 0.025),
    ("amazon.nova-micro-v1:0", 0.000_035, 0.00014),
    ("amazon.nova-lite-v1:0", 0.00006, 0.00024),
    ("amazon.nova-pro-v1:0", 0.0008, 0.0032),
    ("openai.gpt-5.6-sol", 0.0044, 0.022),
    ("openai.gpt-5.6-terra", 0.0022, 0.0132),
    ("openai.gpt-5.6-luna", 0.00022, 0.00132),
    ("openai.gpt-5.5", 0.0055, 0.033),
    ("openai.gpt-5.4", 0.00275, 0.0165),
    ("us.openai.gpt-5.6-sol", 0.0044, 0.022),
    ("us.openai.gpt-5.6-terra", 0.0022, 0.0132),
    ("us.openai.gpt-5.6-luna", 0.00022, 0.00132),
    ("in.openai.gpt-5.6-terra", 0.0022, 0.0132),
    ("in.openai.gpt-5.6-luna", 0.00022, 0.00132),
    ("global.openai.gpt-5.6-sol", 0.004, 0.020),
    ("global.openai.gpt-5.6-terra", 0.002, 0.012),
    ("global.openai.gpt-5.6-luna", 0.0002, 0.0012),
    ("openai.gpt-6-astra", 0.011, 0.055),
    ("openai.gpt-6-sol", 0.0022, 0.011),
    ("openai.gpt-6-luna", 0.00011, 0.00055),
    ("openai.gpt-6.1-sol", 0.0022, 0.011),
    ("us.openai.gpt-6-astra", 0.011, 0.055),
    ("us.openai.gpt-6-sol", 0.0022, 0.011),
    ("us.openai.gpt-6-luna", 0.00011, 0.00055),
    ("us.openai.gpt-6.1-sol", 0.0022, 0.011),
    ("global.openai.gpt-6-astra", 0.010, 0.050),
    ("global.openai.gpt-6-sol", 0.0020, 0.010),
    ("global.openai.gpt-6-luna", 0.00010, 0.00050),
    ("global.openai.gpt-6.1-sol", 0.0020, 0.010),
];
