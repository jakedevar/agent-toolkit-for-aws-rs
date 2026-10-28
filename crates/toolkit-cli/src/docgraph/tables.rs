//! Instance-class lookup tables of the review (`CONN_LIMITS`,
//! `INSTANCE_RAM_GIB`, `GRAVITON_FAMILIES` of `wa_review.py`).

/// Maximum client connections per instance class.
pub(super) fn connection_limit(class: &str) -> Option<u64> {
    let (family, size) = split_class(class)?;
    let medium_small = matches!(family, "t3" | "t4g") && size == "medium";
    if medium_small {
        return Some(1000);
    }
    if !matches!(family, "r5" | "r6g" | "r6gd" | "r8g") {
        return None;
    }
    match size {
        "large" => Some(3400),
        "xlarge" => Some(7000),
        "2xlarge" => Some(14200),
        "4xlarge" => Some(28400),
        "8xlarge" | "12xlarge" | "16xlarge" => Some(60000),
        "24xlarge" if family == "r5" => Some(60000),
        _ => None,
    }
}

/// Instance memory in GiB.
pub(super) fn ram_gib(class: &str) -> Option<u64> {
    let (family, size) = split_class(class)?;
    if matches!(family, "t3" | "t4g") && size == "medium" {
        return Some(4);
    }
    if !matches!(family, "r5" | "r6g" | "r6gd" | "r8g") {
        return None;
    }
    match size {
        "large" => Some(16),
        "xlarge" => Some(32),
        "2xlarge" => Some(64),
        "4xlarge" => Some(128),
        "8xlarge" => Some(256),
        "12xlarge" => Some(384),
        "16xlarge" => Some(512),
        "24xlarge" if family == "r5" => Some(768),
        _ => None,
    }
}

/// `db.r6g.large` -> (`r6g`, `large`).
fn split_class(class: &str) -> Option<(&str, &str)> {
    let rest = class.strip_prefix("db.")?;
    let (family, size) = rest.split_once('.')?;
    (!size.contains('.')).then_some((family, size))
}

/// Graviton families, matched on the part of the class after `db.` and before
/// the next dot.
pub(super) const GRAVITON_FAMILIES: [&str; 5] = ["r6g", "r7g", "r8g", "t4g", "r6gd"];
