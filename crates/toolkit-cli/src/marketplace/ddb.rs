//! DynamoDB requests of the slice, as the `boto3` resource layer sends them:
//! the key condition, the expression values and the pagination loop over
//! `LastEvaluatedKey`.

use toolkit_aws::recorded::{optional_object, optional_object_list};
use toolkit_core::Json;

use super::base::Failure;
use super::client::{AwsClient, Service};

/// One `Query`: the members of the request without the pagination key.
pub(super) struct Query {
    members: Vec<(&'static str, Json)>,
}

impl Query {
    pub(super) fn new(
        table: &str,
        index: Option<&str>,
        condition: &str,
        values: Vec<(&str, Json)>,
    ) -> Self {
        let mut members = vec![("TableName", Json::string(table))];
        if let Some(index) = index {
            members.push(("IndexName", Json::string(index)));
        }
        members.push(("KeyConditionExpression", Json::string(condition)));
        members.push(("ExpressionAttributeValues", Json::object(values)));
        Self { members }
    }

    pub(super) fn with_limit(mut self, limit: i64) -> Self {
        self.members.push(("Limit", Json::Int(limit)));
        self
    }

    /// The request of the first page.
    pub(super) fn first_page(&self) -> Json {
        self.request(None)
    }

    fn request(&self, start: Option<&Json>) -> Json {
        let mut members: Vec<(&str, Json)> = self
            .members
            .iter()
            .map(|(name, value)| (*name, value.clone()))
            .collect();
        if let Some(key) = start {
            members.push(("ExclusiveStartKey", key.clone()));
        }
        Json::object(members)
    }
}

/// Whether a page walk goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Flow {
    Continue,
    Stop,
}

fn invalid(what: &str, error: impl std::fmt::Display) -> Failure {
    Failure::message(format!("invalid {what} response: {error}"))
}

/// Walks every page of `query`, handing each item to `each`. `Flow::Stop`
/// ends the walk early (the submitter's per-run cap). A failed call, an
/// `Items` or `LastEvaluatedKey` of the wrong type, or a failure of `each`
/// ends it with that failure; nothing is swallowed.
pub(super) fn query_each(
    client: &mut dyn AwsClient,
    query: &Query,
    mut each: impl FnMut(&Json) -> Result<Flow, Failure>,
) -> Result<(), Failure> {
    let mut start: Option<Json> = None;
    loop {
        let response = client.call(Service::DynamoDb, "Query", &query.request(start.as_ref()))?;
        let items =
            optional_object_list(&response, "Items").map_err(|error| invalid("Query", error))?;
        for item in items {
            if each(item)? == Flow::Stop {
                return Ok(());
            }
        }
        match optional_object(&response, "LastEvaluatedKey")
            .map_err(|error| invalid("Query", error))?
        {
            Some(key) => start = Some(key.clone()),
            None => return Ok(()),
        }
    }
}

/// All items of `query`, in page order.
pub(super) fn query_all(client: &mut dyn AwsClient, query: &Query) -> Result<Vec<Json>, Failure> {
    let mut items = Vec::new();
    query_each(client, query, |item| {
        items.push(item.clone());
        Ok(Flow::Continue)
    })?;
    Ok(items)
}

/// One `UpdateItem`.
pub(super) struct Update<'a> {
    pub(super) table: &'a str,
    pub(super) key: Vec<(&'a str, Json)>,
    pub(super) expression: &'a str,
    pub(super) condition: Option<&'a str>,
    /// `None` leaves `ExpressionAttributeValues` out (DynamoDB refuses an
    /// empty map).
    pub(super) values: Option<Json>,
    pub(super) names: Option<Json>,
}

impl Update<'_> {
    pub(super) fn request(&self) -> Json {
        let mut members = vec![
            ("TableName", Json::string(self.table)),
            ("Key", Json::object(self.key.clone())),
            ("UpdateExpression", Json::string(self.expression)),
        ];
        if let Some(condition) = self.condition {
            members.push(("ConditionExpression", Json::string(condition)));
        }
        if let Some(values) = &self.values {
            members.push(("ExpressionAttributeValues", values.clone()));
        }
        if let Some(names) = &self.names {
            members.push(("ExpressionAttributeNames", names.clone()));
        }
        Json::object(members)
    }
}

/// Sends an `UpdateItem`.
pub(super) fn update_item(
    client: &mut dyn AwsClient,
    update: &Update<'_>,
) -> Result<(), toolkit_aws::AwsError> {
    client
        .call(Service::DynamoDb, "UpdateItem", &update.request())
        .map(|_| ())
}

/// The string member `key` of an item, `""` when absent (`item.get(key, "")`).
/// A member of another type is an invalid response.
pub(super) fn text_or_empty(item: &Json, key: &str) -> Result<String, Failure> {
    toolkit_aws::recorded::optional_text(item, key)
        .map(|text| text.unwrap_or_default().to_string())
        .map_err(|error| Failure::message(format!("invalid item: {error}")))
}

/// The string member `key` of an item, required (`item[key]`).
pub(super) fn required_text(item: &Json, key: &str) -> Result<String, Failure> {
    match toolkit_aws::recorded::optional_text(item, key) {
        Ok(Some(text)) => Ok(text.to_string()),
        Ok(None) => Err(Failure::message(format!("invalid item: {key} is missing"))),
        Err(error) => Err(Failure::message(format!("invalid item: {error}"))),
    }
}
