//! Amazon Bedrock on-demand token price lookup (`bedrock_pricing.py`).
//!
//! The curated [`STATIC_FALLBACK`] table and the region/profile-aware
//! Opus 5.5 rates are the primary sources. The live AWS Pricing API of the
//! Python module is replaced by a recorded `get_products` response
//! (`--price-list FILE`, see [`PriceList`]); without one a model that is not in
//! the table is simply unavailable. A lookup never fails: it reports
//! `available: false` and a note instead.

use toolkit_core::{parse_json, Json};
use toolkit_msk::pyfmt::py_float_parse;

use super::identity::is_decimal;
use super::pricing_table::STATIC_FALLBACK;
use crate::safe_text::safe_text;

/// Result of one lookup, in the key order of the Python dict it replaces:
/// the table and the live parser put the rates first, `unavailable` and the
/// Opus 5.5 rule put `available` first.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Price {
    pub(crate) available: bool,
    pub(crate) input_per_1k_usd: Option<f64>,
    pub(crate) output_per_1k_usd: Option<f64>,
    pub(crate) note: String,
    rates_first: bool,
}

impl Price {
    pub(crate) fn to_json(&self) -> Json {
        let rate = |value: Option<f64>| value.map_or(Json::Null, Json::Float);
        let rates = [
            ("input_per_1k_usd", rate(self.input_per_1k_usd)),
            ("output_per_1k_usd", rate(self.output_per_1k_usd)),
        ];
        let tail = [
            ("available", Json::Bool(self.available)),
            ("note", Json::string(self.note.clone())),
        ];
        let pairs: Vec<(&str, Json)> = if self.rates_first {
            rates.into_iter().chain(tail).collect()
        } else {
            let [available, note] = tail;
            let [input, output] = rates;
            vec![available, input, output, note]
        };
        Json::object(pairs)
    }
}

/// `unavailable(note)`: a banner, never an exception.
pub(crate) fn unavailable(note: &str) -> Price {
    Price {
        available: false,
        input_per_1k_usd: None,
        output_per_1k_usd: None,
        note: format!("Pricing unavailable: {note}"),
        rates_first: false,
    }
}

/// `(input, output)` USD per 1K tokens of one `PriceList` item, base input and
/// output token dimensions only (cache read/write, batch and other extended
/// dimensions are skipped).
///
/// # Errors
///
/// A structurally unexpected item (a member that is not an object, a rate that
/// is not a number). The message never repeats a value of the item.
pub(crate) fn parse_price_dimensions(item: &Json) -> Result<(Option<f64>, Option<f64>), String> {
    const BAD: &str = "the price item has an unexpected structure";
    let members = |value: &Json, key: &str| -> Result<Vec<Json>, String> {
        match value.get(key) {
            None => Ok(Vec::new()),
            Some(Json::Object(pairs)) => {
                Ok(pairs.iter().map(|(_, member)| member.clone()).collect())
            }
            Some(_) => Err(BAD.to_string()),
        }
    };
    let section = |value: &Json, key: &str| -> Result<Json, String> {
        match value.get(key) {
            None => Ok(Json::Object(Vec::new())),
            Some(object @ Json::Object(_)) => Ok(object.clone()),
            Some(_) => Err(BAD.to_string()),
        }
    };
    if !matches!(item, Json::Object(_)) {
        return Err(BAD.to_string());
    }
    let (mut input, mut output) = (None, None);
    let on_demand = section(&section(item, "terms")?, "OnDemand")?;
    let Json::Object(terms) = on_demand else {
        return Err(BAD.to_string());
    };
    for (_, term) in &terms {
        for dimension in members(term, "priceDimensions")? {
            let usd = dimension_usd(&dimension)?;
            let description = match dimension.get("description") {
                None => String::new(),
                Some(Json::String(text)) => text.to_lowercase(),
                Some(_) => return Err(BAD.to_string()),
            };
            if ["cache", "read", "write", "batch"]
                .iter()
                .any(|skip| description.contains(skip))
            {
                continue;
            }
            if description.contains("input") && description.contains("token") {
                input = Some(usd);
            } else if description.contains("output") && description.contains("token") {
                output = Some(usd);
            }
        }
    }
    Ok((input, output))
}

/// `float(dim.get("pricePerUnit", {}).get("USD", "0") or 0)`.
fn dimension_usd(dimension: &Json) -> Result<f64, String> {
    const BAD: &str = "the price item has an unexpected structure";
    if !matches!(dimension, Json::Object(_)) {
        return Err(BAD.to_string());
    }
    let per_unit = match dimension.get("pricePerUnit") {
        None => return Ok(0.0),
        Some(object @ Json::Object(_)) => object,
        Some(_) => return Err(BAD.to_string()),
    };
    match per_unit.get("USD") {
        None | Some(Json::Null | Json::Bool(false)) => Ok(0.0),
        Some(Json::String(text)) if text.is_empty() => Ok(0.0),
        Some(Json::String(text)) => {
            py_float_parse(text).ok_or_else(|| "the price item has an invalid USD rate".to_string())
        }
        Some(Json::Bool(true)) => Ok(1.0),
        #[allow(clippy::cast_precision_loss)]
        Some(Json::Int(number)) => Ok(*number as f64),
        Some(Json::Float(number)) => Ok(*number),
        Some(_) => Err(BAD.to_string()),
    }
}

/// `OpenAI`'s proprietary GPT models (GPT-5.5/5.4/5.6 and GPT-6), which the AWS
/// `PriceList` API does not carry. The open-weight gpt-oss models are in the
/// API and do not match.
pub(crate) fn is_mantle_gpt(model_id: &str) -> bool {
    let id = model_id.to_lowercase();
    (id.starts_with("openai.gpt-5") || id.starts_with("openai.gpt-6")) && !id.contains("oss")
}

// AWS Price List API, AmazonBedrockFoundationModels, verified 2026-09-24.
// Runtime uses Geo/Global profiles, or the bare ID in eu-west-2 (verified 2026-10-03).
const OPUS55_ID: &str = "anthropic.claude-opus-5-5";
const OPUS55_PROFILE_REGIONS: &[(&str, &[&str])] = &[
    (
        "us",
        &[
            "us-east-1",
            "us-east-2",
            "us-west-1",
            "us-west-2",
            "ca-central-1",
            "ca-west-1",
            "us-gov-east-1",
            "us-gov-west-1",
        ],
    ),
    (
        "eu",
        &[
            "eu-central-1",
            "eu-central-2",
            "eu-north-1",
            "eu-south-1",
            "eu-south-2",
            "eu-west-1",
            "eu-west-2",
            "eu-west-3",
        ],
    ),
    ("au", &["ap-southeast-2", "ap-southeast-4"]),
    ("jp", &["ap-northeast-1", "ap-northeast-3"]),
    (
        "global",
        &[
            "af-south-1",
            "ap-east-2",
            "ap-northeast-1",
            "ap-northeast-2",
            "ap-northeast-3",
            "ap-south-1",
            "ap-south-2",
            "ap-southeast-1",
            "ap-southeast-2",
            "ap-southeast-3",
            "ap-southeast-4",
            "ap-southeast-5",
            "ap-southeast-6",
            "ap-southeast-7",
            "ca-central-1",
            "ca-west-1",
            "eu-central-1",
            "eu-central-2",
            "eu-north-1",
            "eu-south-1",
            "eu-south-2",
            "eu-west-1",
            "eu-west-2",
            "eu-west-3",
            "il-central-1",
            "me-central-1",
            "me-south-1",
            "mx-central-1",
            "sa-east-1",
            "us-east-1",
            "us-east-2",
            "us-west-1",
            "us-west-2",
        ],
    ),
];
const OPUS55_MANTLE_REGIONS: [&str; 3] = ["us-east-1", "ap-southeast-4", "us-gov-west-1"];
const OPUS55_RUNTIME_IN_REGION_REGIONS: [&str; 1] = ["eu-west-2"];

fn opus55_pricing(region: &str, model_id: &str) -> Price {
    let (prefix, valid) = if model_id == OPUS55_ID {
        let in_region = OPUS55_RUNTIME_IN_REGION_REGIONS.contains(&region);
        (
            if in_region {
                "runtime-in-region"
            } else {
                "mantle"
            },
            in_region || OPUS55_MANTLE_REGIONS.contains(&region),
        )
    } else {
        let (prefix, base) = model_id.split_once('.').unwrap_or((model_id, ""));
        let listed = OPUS55_PROFILE_REGIONS
            .iter()
            .find(|(name, _)| *name == prefix)
            .is_some_and(|(_, regions)| regions.contains(&region));
        (prefix, base == OPUS55_ID && listed)
    };
    if !valid {
        return unavailable(&format!(
            "no verified Opus 5.5 rate for {} in {}; confirm the exact model ID, endpoint and inference profile",
            safe_text(model_id),
            safe_text(region)
        ));
    }
    let (input, output) = if region.starts_with("us-gov-") {
        (0.0048, 0.024)
    } else if prefix == "global" {
        (0.004, 0.020)
    } else {
        (0.0044, 0.022)
    };
    let verified = if prefix == "runtime-in-region" {
        "2026-10-03"
    } else {
        "2026-09-24"
    };
    Price {
        available: true,
        input_per_1k_usd: Some(input),
        output_per_1k_usd: Some(output),
        note: format!(
            "AWS Price List API AmazonBedrockFoundationModels; verified {verified}; Standard tier, {}, {prefix} inference; no Batch",
            safe_text(region)
        ),
        rates_first: false,
    }
}

fn table_price(input: f64, output: f64, note: String) -> Price {
    Price {
        available: true,
        input_per_1k_usd: Some(input),
        output_per_1k_usd: Some(output),
        note,
        rates_first: true,
    }
}

/// Python `re.match(r"^(us|in|global)\.openai\.gpt-[56]", text)`.
fn is_geo_gpt_profile(model_id: &str) -> bool {
    ["us.", "in.", "global."]
        .iter()
        .filter_map(|prefix| model_id.strip_prefix(prefix))
        .filter_map(|rest| rest.strip_prefix("openai.gpt-"))
        .any(|rest| rest.starts_with(['5', '6']))
}

/// Python `re.match(r"^[^.]+\.openai\.gpt-[56]", text)`.
fn is_any_gpt_profile(model_id: &str) -> bool {
    model_id
        .split_once('.')
        .filter(|(first, _)| !first.is_empty())
        .and_then(|(_, rest)| rest.strip_prefix("openai.gpt-"))
        .is_some_and(|rest| rest.starts_with(['5', '6']))
}

/// One trailing newline separated from `text`: a Python `$` also matches
/// before it.
fn split_final_newline(text: &str) -> (&str, &str) {
    match text.strip_suffix('\n') {
        Some(body) => (body, "\n"),
        None => (text, ""),
    }
}

/// `re.sub(r"-\d{8}$", "", text)`.
fn without_date_stamp(text: &str) -> String {
    let (body, newline) = split_final_newline(text);
    let tail: Vec<(usize, char)> = body.char_indices().rev().take(9).collect();
    if tail.len() == 9 && tail[8].1 == '-' && tail[..8].iter().all(|(_, c)| is_decimal(*c)) {
        format!("{}{newline}", &body[..tail[8].0])
    } else {
        text.to_string()
    }
}

/// `re.sub(r"-v\d+$", "", text)`.
fn without_v_revision(text: &str) -> String {
    let (body, newline) = split_final_newline(text);
    let digits = body.chars().rev().take_while(|c| is_decimal(*c)).count();
    if digits == 0 {
        return text.to_string();
    }
    let digits_start = body
        .char_indices()
        .rev()
        .nth(digits - 1)
        .map_or(body.len(), |(index, _)| index);
    match body[..digits_start].strip_suffix("-v") {
        Some(head) => format!("{head}{newline}"),
        None => text.to_string(),
    }
}

/// The static table and its partial-match rules. `None` when the id is not
/// priced there (Opus 5.5 always is, region-aware, elsewhere).
pub(crate) fn static_fallback(model_id: &str) -> Option<Price> {
    if model_id.contains("claude-opus-5") {
        return None; // Opus 5.5 requires exact, region-aware pricing.
    }
    if let Some((_, input, output)) = STATIC_FALLBACK.iter().find(|(key, _, _)| *key == model_id) {
        return Some(table_price(
            *input,
            *output,
            "static fallback (PriceList API had no entry)".to_string(),
        ));
    }
    // Proprietary GPT ids require an EXACT match in every form: tier names
    // differ only by suffix at very different price points, the inference
    // options by prefix at a 10% spread.
    if is_mantle_gpt(model_id) || is_geo_gpt_profile(model_id) {
        return None;
    }
    // Strip the version suffix for a partial match. Bidirectional: a dateless
    // query matches a dated key and a date-pinned query a dateless family key
    // (the separator guard keeps ...opus-4-85 from matching ...opus-4-8).
    let base = model_id
        .rfind("-v")
        .map_or(model_id, |index| &model_id[..index]);
    let matched = |key: &str, input: f64, output: f64| {
        table_price(input, output, format!("static fallback (matched {key})"))
    };
    for (key, input, output) in STATIC_FALLBACK {
        if key.starts_with(base)
            || base == *key
            || base
                .strip_prefix(key)
                .is_some_and(|rest| rest.starts_with('-'))
        {
            return Some(matched(key, *input, *output));
        }
    }
    // A trailing date stamp is stripped so a dated ID still matches the
    // undated table keys.
    let dateless = without_date_stamp(base);
    if dateless != base {
        for (key, input, output) in STATIC_FALLBACK {
            if key.starts_with(dateless.as_str()) {
                return Some(matched(key, *input, *output));
            }
        }
    }
    None
}

/// Python `str.isdigit()`: decimal digits plus the superscript, subscript and
/// enclosed digits.
fn is_py_digit(c: char) -> bool {
    is_decimal(c)
        || matches!(
            c,
            '\u{b2}' | '\u{b3}' | '\u{b9}' | '\u{2070}' | '\u{2074}'..='\u{2079}'
                | '\u{2080}'..='\u{2089}' | '\u{2460}'..='\u{2468}' | '\u{2474}'..='\u{247c}'
                | '\u{2488}'..='\u{2490}' | '\u{24ea}' | '\u{24f5}'..='\u{24fd}' | '\u{24ff}'
                | '\u{2776}'..='\u{277e}' | '\u{2780}'..='\u{2788}' | '\u{278a}'..='\u{2792}'
        )
}

/// Python `re.match(r"^\d[\d.]*$", text)`.
fn is_version_text(text: &str) -> bool {
    let (body, _) = split_final_newline(text);
    let mut chars = body.chars();
    chars.next().is_some_and(is_decimal) && chars.all(|c| is_decimal(c) || c == '.')
}

/// Python `str.capitalize()` (first character upper-cased, the rest lower).
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first
            .to_uppercase()
            .chain(chars.as_str().to_lowercase().chars())
            .collect()
    })
}

/// A Pricing-API display-name guess from a Bedrock model id:
/// `us.anthropic.claude-haiku-4-5-20251001-v1:0` is `Claude Haiku 4.5`. The
/// API's `model` attribute holds display names, not model ids.
pub(crate) fn display_name_guess(model_id: &str) -> String {
    let base = model_id.split(':').next().unwrap_or(model_id);
    let base = ["us.", "eu.", "apac.", "global."]
        .iter()
        .find_map(|prefix| base.strip_prefix(prefix))
        .unwrap_or(base);
    let base = base.split_once('.').map_or(base, |(_, rest)| rest);
    let base = without_v_revision(base);
    let base = without_date_stamp(&base);
    let mut words: Vec<String> = Vec::new();
    for token in base.split('-') {
        if !token.is_empty() && token.chars().all(is_py_digit) {
            match words.last_mut() {
                Some(last) if is_version_text(last) => *last = format!("{last}.{token}"),
                _ => words.push(token.to_string()),
            }
        } else {
            words.push(capitalize(token));
        }
    }
    words.join(" ")
}

/// A recorded `pricing.get_products` answer for one display name and region.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RecordedPriceList {
    pub(crate) region: String,
    pub(crate) model: String,
    /// The `PriceList` items; the live call asked for `MaxResults=1`.
    pub(crate) items: Vec<Json>,
}

/// The recorded Pricing API answers a lookup may consult.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PriceList(pub(crate) Vec<RecordedPriceList>);

/// Largest accepted recorded price list.
pub(crate) const MAX_PRICE_LIST_ENTRIES: usize = 4096;

impl PriceList {
    /// Reads `[{"region": R, "model": DISPLAY_NAME, "price_list": [ITEM_JSON_TEXT, ...]}, ...]`.
    ///
    /// # Errors
    ///
    /// A document of another shape; the message never repeats a value of it.
    pub(crate) fn from_json(document: &Json) -> Result<Self, String> {
        let Json::Array(entries) = document else {
            return Err("a recorded price list must be a JSON array".to_string());
        };
        if entries.len() > MAX_PRICE_LIST_ENTRIES {
            return Err(format!(
                "a recorded price list holds at most {MAX_PRICE_LIST_ENTRIES} entries"
            ));
        }
        let mut recorded = Vec::with_capacity(entries.len());
        for entry in entries {
            let text = |key: &str| entry.get(key).and_then(Json::as_str).map(str::to_string);
            let (Some(region), Some(model), Some(Json::Array(items))) =
                (text("region"), text("model"), entry.get("price_list"))
            else {
                return Err(
                    "every recorded price list entry needs text region and model and an array price_list"
                        .to_string(),
                );
            };
            recorded.push(RecordedPriceList {
                region,
                model,
                items: items.clone(),
            });
        }
        Ok(Self(recorded))
    }

    fn first_item(&self, region: &str, model: &str) -> Option<&Json> {
        self.0
            .iter()
            .find(|entry| entry.region == region && entry.model == model)
            .and_then(|entry| entry.items.first())
    }
}

/// Best-effort lookup of a model that is not in the static table in the
/// recorded Pricing API answers.
fn recorded_lookup(region: &str, model_id: &str, price_list: Option<&PriceList>) -> Price {
    let name = display_name_guess(model_id);
    let Some(price_list) = price_list else {
        return unavailable(&format!(
            "not in the static table, and no recorded Pricing API response was given for display name '{}' in {} (pass --price-list FILE; this build has no live Pricing API client)",
            safe_text(&name),
            safe_text(region)
        ));
    };
    let Some(item) = price_list.first_item(region, &name) else {
        return unavailable(&format!(
            "not in static table and no PriceList entry for display name '{}' in {}",
            safe_text(&name),
            safe_text(region)
        ));
    };
    let Json::String(text) = item else {
        return unavailable("the recorded price item is not JSON text");
    };
    let Ok(parsed) = parse_json(text) else {
        return unavailable("the recorded price item is not valid JSON");
    };
    match parse_price_dimensions(&parsed) {
        Err(reason) => unavailable(&reason),
        Ok((input, output)) => {
            let available = input.is_some();
            Price {
                available,
                input_per_1k_usd: input,
                output_per_1k_usd: output,
                note: if available {
                    format!(
                        "live Pricing API (matched display name '{}')",
                        safe_text(&name)
                    )
                } else {
                    "rates not found in price item".to_string()
                },
                rates_first: true,
            }
        }
    }
}

/// `arn:aws:bedrock:<region>:<12 digits>:inference-profile/<id>` is priced as
/// its profile id.
fn profile_id(model_id: &str) -> Option<&str> {
    let rest = model_id.strip_prefix("arn:aws:bedrock:")?;
    let (region, rest) = rest.split_once(':')?;
    let (account, rest) = rest.split_once(':')?;
    let profile = rest.strip_prefix("inference-profile/")?;
    (!region.is_empty()
        && account.len() == 12
        && account.bytes().all(|b| b.is_ascii_digit())
        && !profile.is_empty()
        && !profile.contains('/'))
    .then_some(profile)
}

/// The price of `model_id` in `region`. Never fails.
pub(crate) fn lookup(region: &str, model_id: &str, price_list: Option<&PriceList>) -> Price {
    let model_id = profile_id(model_id).unwrap_or(model_id);
    if model_id.contains("claude-opus-5") {
        return opus55_pricing(region, model_id);
    }
    // The curated table is the primary source: the Pricing API keys models by
    // display name and often lacks entries for new inference profiles.
    if let Some(mut price) = static_fallback(model_id) {
        price.note = "static pricing table (verified 2026-10-05 against \
aws.amazon.com/bedrock/pricing, the Bedrock model cards' Geo/Global inference ID tables, \
and the vendored pricing cache)"
            .to_string();
        if model_id.ends_with("openai.gpt-6-astra") {
            price.note.push_str(
                "; Standard tier, <=272K input tokens. For >272K, use 2x input / 1.5x output rates.",
            );
        }
        return price;
    }
    if is_mantle_gpt(model_id) || is_any_gpt_profile(model_id) {
        // The PriceList API carries no rows for the proprietary GPT models; a
        // bare "unavailable" would read as "this model does not exist".
        return unavailable(&format!(
            "the AWS PriceList API does not carry OpenAI's proprietary GPT models, and {} is not in the static table. This does NOT mean the model is unavailable. Read the rate from the OpenAI tab of aws.amazon.com/bedrock/pricing and add it to STATIC_FALLBACK. Do not derive it from a percentage change to an older rate.",
            safe_text(model_id)
        ));
    }
    recorded_lookup(region, model_id, price_list)
}
