//! Validation of the design JSON and the `CreateTable` requests built from it
//! (`_validate_design`, `_validate_pattern_refs`, `_collect_attr_types` and
//! `_build_create_kwargs` of `deploy_model.py`). Nothing here sends a request.

use std::collections::{BTreeMap, BTreeSet};

use toolkit_core::{quote_untrusted, Json, ToolkitError};

use super::model::{
    die, first_text, list, name_or_unknown, object, py_int, require_objects, show, strings, text,
};
use super::names::check_table_name;
use crate::dynamodb::pyfmt;

const DISTANCE_FUNCTIONS: [&str; 3] = ["COSINE", "EUCLIDEAN", "DOT_PRODUCT"];
const PROJECTION_TYPES: [&str; 3] = ["ALL", "KEYS_ONLY", "INCLUDE"];
const STREAM_VIEWS: [&str; 4] = ["KEYS_ONLY", "NEW_IMAGE", "OLD_IMAGE", "NEW_AND_OLD_IMAGES"];
/// Per-table limit of vector indexes.
const MAX_VECTOR_INDEXES: usize = 5;

/// A list of names the way `sorted(...)` printed it, each one escaped.
fn quoted_list<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let items: Vec<String> = names.into_iter().map(quote_untrusted).collect();
    format!("[{}]", items.join(", "))
}

fn table_label(table: &Json) -> String {
    name_or_unknown(table, "table_name")
}

/// The tables of a validated design.
pub(crate) fn tables(model: &Json) -> Result<&[Json], ToolkitError> {
    list(model, "tables")
}

/// `_validate_design`: refuse a design that cannot be deployed as written.
pub(crate) fn validate_design(model: &Json) -> Result<(), ToolkitError> {
    if model.type_name() != "object" {
        return Err(die("design JSON must be a JSON object"));
    }
    let all_tables = tables(model)?;
    if all_tables.is_empty() {
        return Err(die("design JSON has no tables"));
    }
    let patterns = list(model, "access_patterns")?;
    if patterns.is_empty() {
        return Err(die(
            "design JSON has no access_patterns — refusing (Mechanics #2: unknown RPS is a \
             design gap, not a benchmark input).",
        ));
    }
    require_objects(patterns, "access_patterns")?;
    for pattern in patterns {
        if !pattern.get("peak_rps").is_some_and(pyfmt::truthy) {
            return Err(die(format!(
                "access pattern {} has missing or zero peak_rps — refusing per Mechanics #2.",
                name_or_unknown(pattern, "pattern_id")
            )));
        }
    }
    require_objects(all_tables, "tables")?;
    for table in all_tables {
        let key_schema = object(table, "key_schema")?;
        let has_partition_key = match key_schema {
            Some(schema) => text(schema, "partition_key")?.is_some(),
            None => false,
        };
        if !has_partition_key {
            return Err(die(format!(
                "table {} has no key_schema.partition_key. Live deploy requires an explicit \
                 {{\"key_schema\": {{\"partition_key\": \"<attr>\", \"sort_key\": \"<attr>?\"}}}} \
                 block per table. Add it to the JSON and re-run. (The cost calculator does not \
                 require this; live deploy does.)",
                table_label(table)
            )));
        }
        match text(table, "table_name")? {
            Some(name) => check_table_name(name)?,
            None => return Err(die("every table needs a table_name")),
        }
    }
    let mut seen = BTreeSet::new();
    for table in all_tables {
        let name = text(table, "table_name")?.unwrap_or_default();
        if !seen.insert(name) {
            return Err(die(format!(
                "table {} is defined more than once in tables[].",
                quote_untrusted(name)
            )));
        }
    }
    validate_pattern_refs(model)
}

/// `_validate_pattern_refs`: every access pattern names a defined table, and
/// an index it uses exists in the right family (GSI or vector).
fn validate_pattern_refs(model: &Json) -> Result<(), ToolkitError> {
    let all_tables = tables(model)?;
    let mut by_name: BTreeMap<&str, &Json> = BTreeMap::new();
    for table in all_tables {
        by_name.insert(text(table, "table_name")?.unwrap_or_default(), table);
    }
    for pattern in list(model, "access_patterns")? {
        let pid = name_or_unknown(pattern, "pattern_id");
        let Some(table_name) = text(pattern, "table")? else {
            return Err(die(format!(
                "access pattern {pid} has no \"table\" — every pattern must name the table it \
                 runs against."
            )));
        };
        let Some(table) = by_name.get(table_name) else {
            return Err(die(format!(
                "access pattern {pid} references table {}, which is not defined in tables[]. \
                 Defined tables: {}. Fix the \"table\" field or add the table.",
                quote_untrusted(table_name),
                quoted_list(by_name.keys().copied())
            )));
        };
        let index = text(pattern, "index")?;
        let operation = pattern.get("operation");
        let vector_names = index_names(table, "vector_indexes")?;
        let gsi_names = index_names(table, "gsis")?;
        let shown_table = quote_untrusted(table_name);
        let is_search = matches!(operation, Some(Json::String(op)) if op == "SearchVectors");
        if is_search {
            let Some(index) = index else {
                return Err(die(format!(
                    "access pattern {pid} is a SearchVectors but has no \"index\" — it must name \
                     the vector index to search."
                )));
            };
            if !vector_names.contains(index) {
                return Err(die(format!(
                    "access pattern {pid} searches vector index {} on table {shown_table}, but \
                     that table defines no such vector index. Defined vector indexes on {}: {}. \
                     Add it to vector_indexes[] or correct the \"index\" field.",
                    quote_untrusted(index),
                    quote_untrusted(table_name),
                    quoted_list(vector_names.iter().map(String::as_str))
                )));
            }
        } else if let Some(index) = index {
            if vector_names.contains(index) {
                return Err(die(format!(
                    "access pattern {pid} targets {} on table {shown_table} with operation {}, \
                     but {} is a VECTOR index. Query and Scan are rejected against a vector index \
                     with 'ValidationException: Query operation not supported on this index \
                     type.' Use operation \"SearchVectors\" to read it.",
                    quote_untrusted(index),
                    show(operation),
                    quote_untrusted(index)
                )));
            }
            if !gsi_names.contains(index) {
                return Err(die(format!(
                    "access pattern {pid} uses index {} on table {shown_table}, but that table \
                     defines no such GSI. Defined GSIs on {}: {}. A Query/Scan against a \
                     non-existent index fails every call at runtime — add the GSI to the table or \
                     correct the \"index\" field.",
                    quote_untrusted(index),
                    quote_untrusted(table_name),
                    quoted_list(gsi_names.iter().map(String::as_str))
                )));
            }
        }
    }
    Ok(())
}

/// The `index_name` of every entry of `table[key]` (entries without a string
/// name are ignored, like `{g.get("index_name") for g in ...}` filtered).
fn index_names(table: &Json, key: &str) -> Result<BTreeSet<String>, ToolkitError> {
    let entries = list(table, key)?;
    require_objects(entries, key)?;
    Ok(entries
        .iter()
        .filter_map(|entry| match entry.get("index_name") {
            Some(Json::String(name)) if !name.is_empty() => Some(name.clone()),
            _ => None,
        })
        .collect())
}

fn norm_type(value: Option<&str>) -> String {
    match value.map(str::to_uppercase).as_deref() {
        Some(kind @ ("S" | "N" | "B")) => kind.to_string(),
        _ => "S".to_string(),
    }
}

/// `_collect_attr_types`: attribute name -> `S`/`N`/`B` per table. Types come
/// from `entities[].attributes[]`, then from the table-level
/// `attribute_definitions` (which wins).
pub(crate) fn collect_attr_types(table: &Json) -> Result<BTreeMap<String, String>, ToolkitError> {
    let mut types = BTreeMap::new();
    let entities = list(table, "entities")?;
    require_objects(entities, "entities")?;
    for entity in entities {
        let attributes = list(entity, "attributes")?;
        require_objects(attributes, "attributes")?;
        for attribute in attributes {
            if let Some(name) = text(attribute, "name")? {
                types.insert(name.to_string(), norm_type(text(attribute, "type")?));
            }
        }
    }
    let definitions = list(table, "attribute_definitions")?;
    require_objects(definitions, "attribute_definitions")?;
    for definition in definitions {
        if let Some(name) = first_text(definition, &["attribute_name", "name"])? {
            types.insert(
                name.to_string(),
                norm_type(first_text(definition, &["attribute_type", "type"])?),
            );
        }
    }
    Ok(types)
}

fn attr_type(types: &BTreeMap<String, String>, name: &str) -> String {
    types.get(name).cloned().unwrap_or_else(|| "S".to_string())
}

fn key_element(name: &str, key_type: &str) -> Json {
    Json::object(vec![
        ("AttributeName", Json::string(name)),
        ("KeyType", Json::string(key_type)),
    ])
}

/// The projection of a GSI or a vector index. `shorthand` accepts the bare
/// string (`"KEYS_ONLY"`), which only vector indexes accepted.
fn projection(owner: &str, parent: &Json, shorthand: bool) -> Result<Json, ToolkitError> {
    let default = Json::object(vec![("type", Json::string("ALL"))]);
    let given = match parent.get("projection") {
        Some(value) if pyfmt::truthy(value) => value,
        _ => &default,
    };
    let spec = match given {
        Json::String(kind) if shorthand => Json::object(vec![("type", Json::string(kind))]),
        other => other.clone(),
    };
    if spec.type_name() != "object" {
        return Err(die(format!(
            "{owner} projection must be an object (it is {})",
            spec.type_name()
        )));
    }
    let kind = text(&spec, "type")?.unwrap_or("ALL").to_uppercase();
    if !PROJECTION_TYPES.contains(&kind.as_str()) {
        return Err(die(format!(
            "{owner} projection type {} must be ALL, KEYS_ONLY or INCLUDE.",
            quote_untrusted(&kind)
        )));
    }
    let mut pairs = vec![("ProjectionType", Json::string(kind.as_str()))];
    if kind == "INCLUDE" {
        // `attributes` is canonical; the calculator and the schema fingerprint
        // also accept the other two spellings, so all three are read here.
        let mut included = Vec::new();
        for key in ["attributes", "non_key_attributes", "NonKeyAttributes"] {
            included = strings(&spec, key)?;
            if !included.is_empty() {
                break;
            }
        }
        if included.is_empty() {
            return Err(die(format!(
                "{owner} has projection type INCLUDE but no projected attributes. Add a \
                 non-empty \"attributes\" list to the projection, or use \"ALL\"/\"KEYS_ONLY\"."
            )));
        }
        pairs.push((
            "NonKeyAttributes",
            Json::Array(
                included
                    .iter()
                    .map(|name| Json::string(name.as_str()))
                    .collect(),
            ),
        ));
    }
    Ok(Json::object(pairs))
}

/// `provisioned_capacity` as `(read, write)` capacity units.
pub(crate) fn provisioned(value: &Json) -> Result<(i64, i64), ToolkitError> {
    if value.type_name() != "object" {
        return Err(die(format!(
            "provisioned_capacity must be an object with \"read\" and \"write\" (it is {})",
            value.type_name()
        )));
    }
    let mut units = [5_i64, 5_i64];
    for (slot, key) in units.iter_mut().zip(["read", "write"]) {
        if let Some(raw) = value.get(key) {
            let parsed = super::model::py_int(raw)
                .filter(|number| (1..=i128::from(i64::MAX)).contains(number))
                .ok_or_else(|| {
                    die(format!(
                        "provisioned_capacity.{key} must be a positive whole number, got {}",
                        show(Some(raw))
                    ))
                })?;
            *slot = i64::try_from(parsed).unwrap_or(i64::MAX);
        }
    }
    Ok((units[0], units[1]))
}

fn throughput(read: i64, write: i64) -> Json {
    Json::object(vec![
        ("ReadCapacityUnits", Json::Int(read)),
        ("WriteCapacityUnits", Json::Int(write)),
    ])
}

fn build_gsis(
    table: &Json,
    table_name: &str,
    types: &BTreeMap<String, String>,
    referenced: &mut BTreeMap<String, String>,
) -> Result<Vec<Vec<(String, Json)>>, ToolkitError> {
    let entries = list(table, "gsis")?;
    require_objects(entries, "gsis")?;
    let mut gsis = Vec::new();
    for gsi in entries {
        let Some(partition) = text(gsi, "partition_key")? else {
            return Err(die(format!(
                "GSI {} on table {} has no partition_key",
                name_or_unknown(gsi, "index_name"),
                quote_untrusted(table_name)
            )));
        };
        referenced.insert(partition.to_string(), attr_type(types, partition));
        let mut key_schema = vec![key_element(partition, "HASH")];
        if let Some(sort) = text(gsi, "sort_key")? {
            referenced.insert(sort.to_string(), attr_type(types, sort));
            key_schema.push(key_element(sort, "RANGE"));
        }
        let Some(index_name) = text(gsi, "index_name")? else {
            return Err(die(format!(
                "a GSI on table {} has no index_name",
                quote_untrusted(table_name)
            )));
        };
        let owner = format!(
            "GSI {} on table {}",
            quote_untrusted(index_name),
            quote_untrusted(table_name)
        );
        gsis.push(vec![
            ("IndexName".to_string(), Json::string(index_name)),
            ("KeySchema".to_string(), Json::Array(key_schema)),
            ("Projection".to_string(), projection(&owner, gsi, false)?),
        ]);
    }
    Ok(gsis)
}

fn build_vector_indexes(
    table: &Json,
    table_name: &str,
    types: &BTreeMap<String, String>,
    referenced: &mut BTreeMap<String, String>,
) -> Result<Vec<Json>, ToolkitError> {
    let entries = list(table, "vector_indexes")?;
    require_objects(entries, "vector_indexes")?;
    let mut indexes = Vec::new();
    for vector in entries {
        let name = text(vector, "index_name")?;
        let attribute = text(vector, "vector_attribute")?;
        let distance = text(vector, "distance_function")?.map(str::to_uppercase);
        let dimensions = vector.get("dimensions").filter(|d| pyfmt::truthy(d));
        let (Some(name), Some(attribute), Some(dimensions), Some(distance)) =
            (name, attribute, dimensions, distance)
        else {
            return Err(die(format!(
                "vector index {} on table {} needs index_name, vector_attribute, dimensions and \
                 distance_function",
                name.map_or_else(|| "?".to_string(), quote_untrusted),
                quote_untrusted(table_name)
            )));
        };
        if !DISTANCE_FUNCTIONS.contains(&distance.as_str()) {
            return Err(die(format!(
                "vector index {}: distance_function must be COSINE, EUCLIDEAN or DOT_PRODUCT, \
                 got {}",
                quote_untrusted(name),
                show(vector.get("distance_function"))
            )));
        }
        let dims = py_int(dimensions)
            .filter(|number| (1..=4096).contains(number))
            .ok_or_else(|| {
                die(format!(
                    "vector index {}: dimensions must be 1-4096, got {}",
                    quote_untrusted(name),
                    show(Some(dimensions))
                ))
            })?;
        let owner = format!("vector index {}", quote_untrusted(name));
        let mut spec = vec![
            ("IndexName", Json::string(name)),
            (
                "VectorAttribute",
                Json::object(vec![("AttributeName", Json::string(attribute))]),
            ),
            ("Projection", projection(&owner, vector, true)?),
            ("Dimensions", Json::Int(i64::try_from(dims).unwrap_or(0))),
            ("DistanceFunction", Json::string(distance.as_str())),
        ];
        let mut elements = Vec::new();
        if let Some(schema) = object(vector, "search_schema")? {
            if let Some(partition) = text(schema, "partition_key")? {
                referenced.insert(partition.to_string(), attr_type(types, partition));
                elements.push(Json::object(vec![
                    ("AttributeName", Json::string(partition)),
                    ("SearchSchemaElementType", Json::string("HASH")),
                ]));
            }
            for filter in strings(schema, "inline_filters")? {
                referenced.insert(filter.clone(), attr_type(types, &filter));
                elements.push(Json::object(vec![
                    ("AttributeName", Json::string(filter.as_str())),
                    ("SearchSchemaElementType", Json::string("INLINE_FILTER")),
                ]));
            }
        }
        if !elements.is_empty() {
            spec.push(("SearchSchema", Json::Array(elements)));
        }
        indexes.push(Json::object(spec));
    }
    Ok(indexes)
}

/// `_build_create_kwargs`: the `CreateTable` request of one design table.
/// `tags` is the ordered tag list of the run; `global` is the config-level
/// provisioned capacity.
pub(crate) fn build_create_table(
    table: &Json,
    prefix: &str,
    tags: &[(String, String)],
    global: Option<(i64, i64)>,
) -> Result<Json, ToolkitError> {
    let table_name = text(table, "table_name")?.unwrap_or_default();
    let types = collect_attr_types(table)?;
    let key_schema = object(table, "key_schema")?.ok_or_else(|| die("table has no key_schema"))?;
    let partition = text(key_schema, "partition_key")?
        .ok_or_else(|| die("table has no key_schema.partition_key"))?;
    let sort = text(key_schema, "sort_key")?;

    check_table_name(&format!("{prefix}{table_name}"))?;
    let mut referenced = BTreeMap::new();
    referenced.insert(partition.to_string(), attr_type(&types, partition));
    let mut keys = vec![key_element(partition, "HASH")];
    if let Some(sort) = sort {
        referenced.insert(sort.to_string(), attr_type(&types, sort));
        keys.push(key_element(sort, "RANGE"));
    }

    let mut gsis = build_gsis(table, table_name, &types, &mut referenced)?;
    let mut vectors = build_vector_indexes(table, table_name, &types, &mut referenced)?;

    let own = match table.get("provisioned_capacity") {
        Some(value) if pyfmt::truthy(value) => Some(provisioned(value)?),
        _ => None,
    };
    let provisioned_capacity = own.or(global);
    if provisioned_capacity.is_some() && !vectors.is_empty() {
        return Err(die(format!(
            "table {} declares a vector index AND provisioned capacity. Vector indexes are \
             supported only on on-demand (PAY_PER_REQUEST) tables, so this table cannot be \
             created as declared. Remove provisioned_capacity for this table (or drop the vector \
             index) and re-run. Note a provisioned-ceiling throttle experiment is therefore not \
             available on a vector-indexed table.",
            quote_untrusted(table_name)
        )));
    }

    let attribute_definitions = referenced
        .iter()
        .map(|(name, kind)| {
            Json::object(vec![
                ("AttributeName", Json::string(name.as_str())),
                ("AttributeType", Json::string(kind.as_str())),
            ])
        })
        .collect();
    let mut request = vec![
        ("TableName", Json::string(format!("{prefix}{table_name}"))),
        ("AttributeDefinitions", Json::Array(attribute_definitions)),
        ("KeySchema", Json::Array(keys)),
        ("BillingMode", Json::string("PAY_PER_REQUEST")),
        // Bench-only: deletion protection off, so teardown needs no two-step
        // disable. DynamoDB always encrypts at rest with an AWS-owned key and
        // accepts no SSESpecification for that (SSEType only takes "KMS").
        ("DeletionProtectionEnabled", Json::Bool(false)),
        (
            "Tags",
            Json::Array(
                tags.iter()
                    .map(|(key, value)| {
                        Json::object(vec![
                            ("Key", Json::string(key.as_str())),
                            ("Value", Json::string(value.as_str())),
                        ])
                    })
                    .collect(),
            ),
        ),
    ];
    if let Some((read, write)) = provisioned_capacity {
        set(&mut request, "BillingMode", Json::string("PROVISIONED"));
        request.push(("ProvisionedThroughput", throughput(read, write)));
        for gsi in &mut gsis {
            gsi.push(("ProvisionedThroughput".to_string(), throughput(read, write)));
        }
    }
    if !gsis.is_empty() {
        request.push((
            "GlobalSecondaryIndexes",
            Json::Array(gsis.into_iter().map(Json::Object).collect()),
        ));
    }
    if vectors.len() > MAX_VECTOR_INDEXES {
        return Err(die(format!(
            "table {} declares {} vector indexes; the per-table limit is {MAX_VECTOR_INDEXES}",
            quote_untrusted(table_name),
            vectors.len()
        )));
    }
    if !vectors.is_empty() {
        request.push(("VectorIndexes", Json::Array(std::mem::take(&mut vectors))));
    }
    if let Some(streams) = stream_specification(table, table_name)? {
        request.push(("StreamSpecification", streams));
    }
    Ok(Json::object(request))
}

/// The `StreamSpecification` of a table that enables streams.
fn stream_specification(table: &Json, table_name: &str) -> Result<Option<Json>, ToolkitError> {
    let Some(streams) = object(table, "streams")? else {
        return Ok(None);
    };
    if !streams.get("enabled").is_some_and(pyfmt::truthy) {
        return Ok(None);
    }
    let view = text(streams, "view_type")?.unwrap_or("NEW_AND_OLD_IMAGES");
    if !STREAM_VIEWS.contains(&view) {
        return Err(die(format!(
            "table {}: streams.view_type {} must be KEYS_ONLY, NEW_IMAGE, OLD_IMAGE or \
             NEW_AND_OLD_IMAGES.",
            quote_untrusted(table_name),
            quote_untrusted(view)
        )));
    }
    Ok(Some(Json::object(vec![
        ("StreamEnabled", Json::Bool(true)),
        ("StreamViewType", Json::string(view)),
    ])))
}

fn set(pairs: &mut [(&str, Json)], key: &str, value: Json) {
    if let Some(slot) = pairs.iter_mut().find(|(name, _)| *name == key) {
        slot.1 = value;
    }
}
