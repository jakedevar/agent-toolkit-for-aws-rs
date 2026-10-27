//! Validation and normalization of instrumentation inputs
//! (`di_validation.py`). Every message is byte-for-byte the legacy text.

use super::consts::SNAPSHOT_SIGNAL_TYPE;
use super::pytext::{opt_int, opt_text, strip};

/// The API's canonical `ProgrammingLanguage` casing, or `None` if unknown.
/// The API enum is case-sensitive (`Java`, `Python`, `Javascript`).
#[must_use]
pub fn canonical_language(language: Option<&str>) -> Option<&'static str> {
    match strip(language.unwrap_or("")).to_lowercase().as_str() {
        "python" => Some("Python"),
        "java" => Some("Java"),
        "javascript" => Some("Javascript"),
        _ => None,
    }
}

/// Normalizes the type to upper case and returns `(normalized, error)`.
///
/// The normalized value is always the upper-cased received value, even on
/// the error path; check `error` before using it.
#[must_use]
pub fn normalize_instrumentation_type(
    instrumentation_type: Option<&str>,
) -> (String, Option<String>) {
    let normalized = strip(instrumentation_type.unwrap_or("")).to_uppercase();
    if normalized == "BREAKPOINT" || normalized == "PROBE" {
        return (normalized, None);
    }
    let error = format!(
        "ERROR: instrumentation_type must be one of BREAKPOINT, PROBE (received: {})",
        opt_text(instrumentation_type)
    );
    (normalized, Some(error))
}

/// Validates a capture-name list (`capture_arguments` / `capture_locals`).
///
/// An omitted list (`None`) is valid and means "capture nothing". A provided
/// list must be non-empty and may not contain the `*` wildcard.
#[must_use]
pub fn validate_capture_names(field_name: &str, names: Option<&[String]>) -> Option<String> {
    let names = names?;
    if names.is_empty() {
        return Some(format!(
            "ERROR: {field_name} must contain at least one name if provided. \
             Omit it to capture none."
        ));
    }
    if names.iter().any(|name| name == "*") {
        return Some(format!(
            "ERROR: {field_name} does not support the wildcard \"*\". \
             List explicit names, or omit it to capture none."
        ));
    }
    None
}

/// Validates the PROBE-only constraints: not supported for JavaScript and
/// method/function-level only (the SDKs ignore `line_number`).
#[must_use]
pub fn validate_probe_constraints(
    normalized_type: &str,
    language: Option<&str>,
    line_number: Option<i64>,
) -> Option<String> {
    if normalized_type != "PROBE" {
        return None;
    }
    if strip(language.unwrap_or("")).to_lowercase() == "javascript" {
        return Some(
            "ERROR: PROBE is not supported for JavaScript. \
             Use instrumentation_type=BREAKPOINT for JavaScript targets."
                .to_string(),
        );
    }
    if line_number.is_some() {
        return Some(
            "ERROR: PROBE does not support line_number (the SDKs ignore it). \
             Omit line_number for PROBE \u{2014} it is method/function-level only."
                .to_string(),
        );
    }
    None
}

/// True for a 16-character lowercase hexadecimal location hash.
///
/// Hex can never contain the double quote that would break out of a Logs
/// Insights string literal, so the snapshot and status tools validate with
/// this before interpolating the hash into a query.
#[must_use]
pub fn is_valid_location_hash(location_hash: Option<&str>) -> bool {
    location_hash.is_some_and(|hash| {
        hash.len() == 16
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Returns an error message unless `signal_type` is `SNAPSHOT`.
#[must_use]
pub fn validate_snapshot_signal(signal_type: Option<&str>) -> Option<String> {
    let normalized = strip(signal_type.unwrap_or("")).to_uppercase();
    if normalized == SNAPSHOT_SIGNAL_TYPE {
        return None;
    }
    Some(format!(
        "ERROR: signal_type must be SNAPSHOT for this API (received: {})",
        opt_text(signal_type)
    ))
}

/// Troubleshooting guidance for code-location create failures.
///
/// The arguments are the raw, unvalidated inputs (which may be absent); the
/// received-inputs block renders them verbatim, `None` included.
#[must_use]
pub fn format_code_location_troubleshooting(
    language: Option<&str>,
    file_path: Option<&str>,
    code_unit: Option<&str>,
    class_name: Option<&str>,
    method_name: Option<&str>,
    line_number: Option<i64>,
) -> String {
    let lang = strip(language.unwrap_or("")).to_lowercase();

    let mut lines: Vec<String> = vec![
        "CODE LOCATION TROUBLESHOOTING:".to_string(),
        "- file_path: source file path for the target code.".to_string(),
        "- code_unit: Python runtime module path OR Java package name.".to_string(),
        "- class_name: use for class methods (Java: simple class name only).".to_string(),
        "- method_name: function/method name.".to_string(),
        "- line_number: set only for line-level breakpoints (1-based).".to_string(),
    ];

    match line_number {
        None => lines
            .push("- Breakpoint level: FUNCTION/METHOD-level (line_number omitted).".to_string()),
        Some(number) => {
            lines.push(format!("- Breakpoint level: LINE-LEVEL (L{number})."));
            if lang == "python" || lang == "java" {
                lines.push(
                    "  * NOTE: target an executable statement. In Python/Java a non-executable \
                     line (blank, comment, decorator, signature) is ignored and the breakpoint \
                     never fires."
                        .to_string(),
                );
            } else if lang == "javascript" {
                lines.push(
                    "  * NOTE: in JavaScript a breakpoint on a non-executable line slides to the \
                     next parseable line and fires there \u{2014} verify it lands where you intend."
                        .to_string(),
                );
            }
        }
    }

    let rules: &[&str] = match lang.as_str() {
        "python" => &[
            "- Python rules:",
            "  * Set code_unit to the dotted runtime import path for the module that defines the target code.",
            "  * Example: services.billing, not billing.py or /app/services/billing.py.",
            "  * Use code_unit=\"__main__\" only when the target file is executed",
            "    directly as the process entry script.",
            "  * If call site uses direct import aliasing, target importing module and alias name.",
            "  * If you cannot determine the runtime module path confidently, inspect first instead of guessing.",
        ],
        "java" => &[
            "- Java rules:",
            "  * Set code_unit to the Java package name (e.g., com.amazon.sampleapp).",
            "  * class_name must be simple name (e.g., OrderContext), not fully qualified.",
        ],
        "javascript" => &[
            "- JavaScript rules:",
            "  * JavaScript binds by file_path + line_number; line_number is required (>= 1).",
            "  * code_unit, class_name, and method_name are not used for JavaScript.",
            "  * Point line_number at the executable statement you want to observe.",
        ],
        _ => &[],
    };
    lines.extend(rules.iter().map(|line| (*line).to_string()));

    lines.push("LOCATION INPUTS RECEIVED:".to_string());
    lines.push(format!("- language={}", opt_text(language)));
    lines.push(format!("- file_path={}", opt_text(file_path)));
    lines.push(format!("- code_unit={}", opt_text(code_unit)));
    lines.push(format!("- class_name={}", opt_text(class_name)));
    lines.push(format!("- method_name={}", opt_text(method_name)));
    lines.push(format!("- line_number={}", opt_int(line_number)));

    lines.join("\n")
}

/// Validates the location fields and returns actionable error text if
/// invalid. Enforces the per-language fields the SDK needs to bind the
/// instrumentation:
///
/// - Java: `code_unit`, `class_name` and `method_name`.
/// - Python: `code_unit` and `method_name` (`class_name` optional).
/// - JavaScript: `line_number` (>= 1); it binds by file and line.
#[must_use]
pub fn validate_location_inputs(
    language: &str,
    file_path: &str,
    code_unit: Option<&str>,
    class_name: Option<&str>,
    method_name: Option<&str>,
    line_number: Option<i64>,
) -> Option<String> {
    let lang = strip(language).to_lowercase();

    let mut errors: Vec<String> = Vec::new();
    let mut suggestions: Vec<String> = Vec::new();

    if strip(file_path).is_empty() {
        errors.push("file_path is required and must be non-empty.".to_string());
    }

    if let Some(number) = line_number.filter(|number| *number < 1) {
        errors.push(format!("line_number must be >= 1 (received: {number})."));
    }

    if !matches!(lang.as_str(), "python" | "java" | "javascript") {
        errors.push(format!(
            "language must be Python, Java, or JavaScript (received: {language})."
        ));
    }

    let raw = (code_unit, class_name, method_name);
    let code_unit = code_unit.filter(|text| !text.is_empty());
    let class_name = class_name.filter(|text| !text.is_empty());
    let method_name = method_name.filter(|text| !text.is_empty());

    match lang.as_str() {
        "java" => {
            if code_unit.is_none() {
                errors.push(
                    "Java requires code_unit (the package name, e.g. com.amazon.sampleapp)."
                        .to_string(),
                );
            }
            if class_name.is_none() {
                errors.push(
                    "Java requires class_name (the simple class name, e.g. OrderContext)."
                        .to_string(),
                );
            }
            if method_name.is_none() {
                errors.push("Java requires method_name.".to_string());
            }
            if let Some(class_name) = class_name.filter(|name| name.contains('.')) {
                errors.push(
                    "For Java, class_name must be simple (e.g., \"OrderContext\"), \
                     not fully qualified (e.g., \"com.example.OrderContext\")."
                        .to_string(),
                );
                if code_unit.is_none() {
                    let parts: Vec<&str> = class_name.split('.').collect();
                    if parts.len() > 1 {
                        suggestions.push(format!(
                            "Use code_unit=\"{}\" and class_name=\"{}\".",
                            parts[..parts.len() - 1].join("."),
                            parts[parts.len() - 1]
                        ));
                    }
                }
            }
            if code_unit.is_some_and(|unit| unit.contains('/')) {
                suggestions.push(
                    "Java code_unit should be a package name with dots, not a path with slashes."
                        .to_string(),
                );
            }
        }
        "python" => {
            if code_unit.is_none() {
                errors.push(
                    "Python requires code_unit (the dotted runtime module path, e.g. services.billing)."
                        .to_string(),
                );
            }
            if method_name.is_none() {
                errors.push("Python requires method_name.".to_string());
            }
            if code_unit.is_some_and(|unit| unit.ends_with(".py")) {
                suggestions.push(
                    "Python code_unit should be a module path (e.g., services.billing), not a .py filename."
                        .to_string(),
                );
            }
        }
        "javascript" => {
            if line_number.is_none() {
                errors.push(
                    "JavaScript requires line_number (>= 1); it binds by file and line."
                        .to_string(),
                );
            }
        }
        _ => {}
    }

    if errors.is_empty() {
        return None;
    }

    let mut message = String::from("Invalid breakpoint location inputs:\n");
    for (index, error) in errors.iter().enumerate() {
        message.push_str(&format!("{}. {error}\n", index + 1));
    }

    if !suggestions.is_empty() {
        message.push_str("\nSuggestions:\n");
        for (index, item) in suggestions.iter().enumerate() {
            message.push_str(&format!("{}. {item}\n", index + 1));
        }
    }

    message.push('\n');
    message.push_str(&format_code_location_troubleshooting(
        Some(language),
        Some(file_path),
        raw.0,
        raw.1,
        raw.2,
        line_number,
    ));
    Some(message)
}
