//! Recorded AWS calls and the shape of the responses they replay (#4727).
//!
//! A slice without a live AWS client replays a recorded document
//! (`--recorded FILE`) and reads the responses it replays. Reviews of the first
//! two such slices found the same defects in each hand-written copy: a
//! recorded member of the wrong type, or `null`, silently defaulted (#4712,
//! #4724); a `null` response list read as an empty listing (#4713, #4725); the
//! expected request of a failure step never compared (#4726). The rules live
//! here once, and `crates/toolkit-cli/tests/recorded_contract.rs` keeps the
//! slices on them.
//!
//! **Recorded input.**
//!
//! - [`step_fields`] reads one step: an optional `request` (an object) and
//!   exactly one of `response` (an object) and `error` (an object).
//! - Every optional member is read through [`optional_member`] or one of its
//!   typed accessors ([`optional_text`], [`optional_object`], ...): an
//!   **absent** member takes its default; a **present** member of another
//!   type, `null` included, rejects the recording.
//! - A [`Step`] stores the expected request of every step, response and error
//!   steps alike, and compares it **before** either outcome is replayed
//!   ([`Step::replay`], [`StepQueue::replay`]).
//! - A [`StepQueue`] replays steps in order per key: a call past the last step
//!   is [`ReplayError::Exhausted`], and [`StepQueue::unconsumed`] counts the
//!   steps a run never replayed.
//!
//! **Responses.** [`optional_list`], [`optional_object`] and [`optional_text`]
//! (with [`optional_object_list`] and [`optional_string_list`]): an absent
//! member or `[]` is empty, while `null` or another type is an invalid
//! response, never an empty discovery. A legitimately nullable scalar is read
//! only by explicit opt-in: [`nullable_text`] for a scalar,
//! [`nullable_list`] for a list whose `null` the slice documents as "none",
//! and [`crate::next_token_from`] for a pagination token (absent, `null` and
//! `""` end paging). A whole value that must be a list or an object (a
//! recorded document, a list entry) is read with [`list_value`] or
//! [`object_members`].
//!
//! **Diagnostics.** A [`MemberError`] carries the member name (a literal of
//! the code), the expected type and the type found, never a value, so a
//! hostile recording or response cannot reach the terminal through it.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use toolkit_core::Json;

use crate::error::AwsError;

/// A present member of the wrong type. It names the member and both types and
/// carries no value of the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberError {
    key: String,
    expected: &'static str,
    found: &'static str,
}

impl MemberError {
    /// The member name, as the code asked for it.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The expected type, such as `a list`.
    #[must_use]
    pub fn expected(&self) -> &'static str {
        self.expected
    }

    /// The JSON type of the present member (`null`, `integer`, ...).
    #[must_use]
    pub fn found(&self) -> &'static str {
        self.found
    }
}

/// `<key> must be <expected> when present (it is <found>)`.
impl fmt::Display for MemberError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} must be {} when present (it is {})",
            self.key, self.expected, self.found
        )
    }
}

impl std::error::Error for MemberError {}

impl From<MemberError> for String {
    fn from(error: MemberError) -> Self {
        error.to_string()
    }
}

/// The one accessor of an *optional* member: absent is `Ok(None)`; a present
/// member must satisfy `expect` (which returns `None` for a wrong type, an
/// explicit `null` included).
///
/// # Errors
///
/// The member is present and `expect` refuses it (`null` is not absent).
pub fn optional_member<'j, T>(
    object: &'j Json,
    key: &str,
    expected: &'static str,
    expect: impl FnOnce(&'j Json) -> Option<T>,
) -> Result<Option<T>, MemberError> {
    match object.get(key) {
        None => Ok(None),
        Some(member) => expect(member).map(Some).ok_or_else(|| MemberError {
            key: key.to_string(),
            expected,
            found: member.type_name(),
        }),
    }
}

/// An optional string member: absent is `None`.
///
/// # Errors
///
/// The member is present and not a string (`null` included).
pub fn optional_text<'j>(object: &'j Json, key: &str) -> Result<Option<&'j str>, MemberError> {
    optional_member(object, key, "a string", Json::as_str)
}

/// An optional object member: absent is `None`.
///
/// # Errors
///
/// The member is present and not an object (`null` included).
pub fn optional_object<'j>(object: &'j Json, key: &str) -> Result<Option<&'j Json>, MemberError> {
    optional_member(object, key, "an object", |member| {
        matches!(member, Json::Object(_)).then_some(member)
    })
}

/// An optional list member: absent and `[]` are empty.
///
/// # Errors
///
/// The member is present and not a list (`null` included).
pub fn optional_list<'j>(object: &'j Json, key: &str) -> Result<&'j [Json], MemberError> {
    optional_member(object, key, "a list", |member| match member {
        Json::Array(items) => Some(items.as_slice()),
        _ => None,
    })
    .map(Option::unwrap_or_default)
}

/// An optional list member whose items must all be objects: absent and `[]`
/// are empty.
///
/// # Errors
///
/// The member is present and not a list of objects.
pub fn optional_object_list<'j>(object: &'j Json, key: &str) -> Result<&'j [Json], MemberError> {
    optional_member(object, key, "a list of objects", |member| match member {
        Json::Array(items) if items.iter().all(|item| matches!(item, Json::Object(_))) => {
            Some(items.as_slice())
        }
        _ => None,
    })
    .map(Option::unwrap_or_default)
}

/// An optional list member whose items must all be strings: absent and `[]`
/// are empty.
///
/// # Errors
///
/// The member is present and not a list of strings.
pub fn optional_string_list<'j>(object: &'j Json, key: &str) -> Result<Vec<&'j str>, MemberError> {
    optional_member(object, key, "a list of strings", |member| match member {
        Json::Array(items) => items.iter().map(Json::as_str).collect::<Option<Vec<_>>>(),
        _ => None,
    })
    .map(Option::unwrap_or_default)
}

/// The explicit opt-in for a legitimately nullable string scalar (a
/// `FailureReason`, an end-of-data marker): absent and `null` are `None`.
///
/// # Errors
///
/// The member is present and neither a string nor `null`.
pub fn nullable_text<'j>(object: &'j Json, key: &str) -> Result<Option<&'j str>, MemberError> {
    optional_member(object, key, "a string or null", |member| match member {
        Json::Null => Some(None),
        Json::String(text) => Some(Some(text.as_str())),
        _ => None,
    })
    .map(Option::flatten)
}

/// The explicit opt-in for a list member whose `null` a slice documents as
/// "none" (the recorded answer of a failed call, a script's `or []`): absent
/// and `null` are `None`, `[]` is an empty list.
///
/// # Errors
///
/// The member is present and neither a list nor `null`.
pub fn nullable_list<'j>(object: &'j Json, key: &str) -> Result<Option<&'j [Json]>, MemberError> {
    optional_member(object, key, "a list or null", |member| match member {
        Json::Null => Some(None),
        Json::Array(items) => Some(Some(items.as_slice())),
        _ => None,
    })
    .map(Option::flatten)
}

/// A whole value that must be a list (a recorded document, an entry of a
/// list, a required member already looked up): `None` for any other type,
/// `null` included. The caller names the value in its own diagnostic.
#[must_use]
pub fn list_value(value: &Json) -> Option<&[Json]> {
    match value {
        Json::Array(items) => Some(items),
        _ => None,
    }
}

/// The members, in order, of a whole value that must be an object: `None`
/// for any other type, `null` included. The caller names the value in its
/// own diagnostic.
#[must_use]
pub fn object_members(value: &Json) -> Option<&[(String, Json)]> {
    match value {
        Json::Object(members) => Some(members),
        _ => None,
    }
}

/// JSON equality that ignores object key order (Python `dict` equality).
#[must_use]
pub fn same_json(left: &Json, right: &Json) -> bool {
    match (left, right) {
        (Json::Object(a), Json::Object(b)) => {
            a.len() == b.len()
                && a.iter().all(|(key, value)| {
                    b.iter()
                        .find(|(other, _)| other == key)
                        .is_some_and(|(_, other)| same_json(value, other))
                })
        }
        (Json::Array(a), Json::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same_json(x, y))
        }
        _ => left == right,
    }
}

/// How a recorded request is compared with the request a call makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOrder {
    /// Objects compare without regard to key order, as Python `dict`s do.
    /// New slices use this.
    Ignored,
    /// Object members must also be in the same order (`Json` equality); kept
    /// for the recordings an existing slice already pins with it.
    Significant,
}

impl KeyOrder {
    /// Whether `expected` and `actual` are the same request.
    #[must_use]
    pub fn same(self, expected: &Json, actual: &Json) -> bool {
        match self {
            Self::Ignored => same_json(expected, actual),
            Self::Significant => expected == actual,
        }
    }
}

/// The members of one recorded step, validated by the shared rules.
pub struct StepFields<'j> {
    /// `request`, when present (always an object).
    pub request: Option<&'j Json>,
    /// What the step replays.
    pub outcome: RecordedOutcome<'j>,
}

/// The outcome member of a recorded step.
pub enum RecordedOutcome<'j> {
    /// `response`: always an object.
    Response(&'j Json),
    /// `error`: always an object; its members are the slice's to read, each
    /// through [`optional_member`] or a typed accessor.
    Error(&'j Json),
}

/// Why a recorded step has the wrong shape. Like [`MemberError`] it names
/// members, never values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepShapeError {
    /// A present `request` that is not an object (`null` included).
    Request(MemberError),
    /// Neither or both of `response` and `error`, or a `response` that is not
    /// an object.
    Outcome,
    /// An `error` that is not an object.
    ErrorNotObject,
}

/// Reads the `request`, `response` and `error` members of one recorded step.
/// The step is expected to be an object; any other value has no members and
/// is an [`StepShapeError::Outcome`].
///
/// # Errors
///
/// The first member, in the order `request`, outcome, of the wrong shape.
pub fn step_fields(step: &Json) -> Result<StepFields<'_>, StepShapeError> {
    let request = optional_object(step, "request").map_err(StepShapeError::Request)?;
    let outcome = match (step.get("response"), step.get("error")) {
        (Some(response @ Json::Object(_)), None) => RecordedOutcome::Response(response),
        (None, Some(error @ Json::Object(_))) => RecordedOutcome::Error(error),
        (None, Some(_)) => return Err(StepShapeError::ErrorNotObject),
        _ => return Err(StepShapeError::Outcome),
    };
    Ok(StepFields { request, outcome })
}

/// Why a step could not be replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayError {
    /// Every recorded step of this call was already replayed.
    Exhausted,
    /// The step's recorded request differs from the request made.
    RequestMismatch,
}

/// One recorded call: the expected request and the outcome it replays.
///
/// It has no `Debug`: the request and response are recording values.
#[derive(Clone)]
pub struct Step {
    request: Option<Json>,
    outcome: Result<Json, AwsError>,
}

impl Step {
    /// A step that answers `response`. `request` is the expected request;
    /// `None` accepts any request.
    #[must_use]
    pub fn response(request: Option<Json>, response: Json) -> Self {
        Self {
            request,
            outcome: Ok(response),
        }
    }

    /// A step that fails with `error`, after the same request check as a
    /// response step.
    #[must_use]
    pub fn failure(request: Option<Json>, error: AwsError) -> Self {
        Self {
            request,
            outcome: Err(error),
        }
    }

    /// The expected request, if the step recorded one.
    #[must_use]
    pub fn request(&self) -> Option<&Json> {
        self.request.as_ref()
    }

    /// Whether `actual` is the recorded request (always, when none was
    /// recorded).
    #[must_use]
    pub fn matches(&self, actual: &Json, order: KeyOrder) -> bool {
        self.request
            .as_ref()
            .is_none_or(|expected| order.same(expected, actual))
    }

    /// The outcome, for a step that may be replayed more than once.
    ///
    /// # Errors
    ///
    /// The recorded failure.
    pub fn outcome(&self) -> Result<Json, AwsError> {
        self.outcome.clone()
    }

    /// Checks `actual` against the recorded request, then replays the
    /// outcome, success or failure alike.
    ///
    /// # Errors
    ///
    /// [`ReplayError::RequestMismatch`] before any outcome is replayed.
    pub fn replay(
        self,
        actual: &Json,
        order: KeyOrder,
    ) -> Result<Result<Json, AwsError>, ReplayError> {
        if self.matches(actual, order) {
            Ok(self.outcome)
        } else {
            Err(ReplayError::RequestMismatch)
        }
    }
}

/// Recorded steps replayed in order, one queue per key (an operation).
pub struct StepQueue<K> {
    steps: BTreeMap<K, VecDeque<Step>>,
    order: KeyOrder,
}

impl<K: Ord> StepQueue<K> {
    /// An empty queue whose requests compare with `order`.
    #[must_use]
    pub fn new(order: KeyOrder) -> Self {
        Self {
            steps: BTreeMap::new(),
            order,
        }
    }

    /// Sets the steps of `key`, replacing any earlier ones.
    pub fn insert(&mut self, key: K, steps: VecDeque<Step>) {
        self.steps.insert(key, steps);
    }

    /// Appends one step to the steps of `key`.
    pub fn push(&mut self, key: K, step: Step) {
        self.steps.entry(key).or_default().push_back(step);
    }

    /// Takes the next step of `key` and replays it against `actual`. The step
    /// is consumed even when its request differs.
    ///
    /// # Errors
    ///
    /// [`ReplayError::Exhausted`] when `key` has no further step;
    /// [`ReplayError::RequestMismatch`] when the request differs.
    pub fn replay(
        &mut self,
        key: &K,
        actual: &Json,
    ) -> Result<Result<Json, AwsError>, ReplayError> {
        let step = self
            .steps
            .get_mut(key)
            .and_then(VecDeque::pop_front)
            .ok_or(ReplayError::Exhausted)?;
        step.replay(actual, self.order)
    }

    /// How many recorded steps were never replayed. A slice that requires the
    /// whole recording to be used fails when this is not zero.
    #[must_use]
    pub fn unconsumed(&self) -> usize {
        self.steps.values().map(VecDeque::len).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ServiceError;
    use toolkit_core::parse_json;

    fn j(text: &str) -> Json {
        parse_json(text).expect("fixture JSON")
    }

    const SECRET: &str = "FAKE-RECORDED-SECRET";

    #[test]
    fn optional_members_default_when_absent_and_reject_null_or_wrong_type() {
        let doc = j(r#"{"list": [1], "empty": [], "obj": {}, "text": "t", "null": null}"#);
        assert_eq!(optional_list(&doc, "list").expect("list").len(), 1);
        assert!(optional_list(&doc, "empty").expect("empty").is_empty());
        assert!(optional_list(&doc, "absent").expect("absent").is_empty());
        assert!(optional_object(&doc, "absent").expect("absent").is_none());
        assert!(optional_object(&doc, "obj").expect("obj").is_some());
        assert_eq!(optional_text(&doc, "text").expect("text"), Some("t"));
        assert_eq!(optional_text(&doc, "absent").expect("absent"), None);
        for key in ["null", "text", "obj"] {
            let error = optional_list(&doc, key).expect_err("not a list");
            assert_eq!(error.key(), key);
            assert_eq!(error.expected(), "a list");
        }
        let error = optional_object(&doc, "null").expect_err("null is not absent");
        assert_eq!(
            error.to_string(),
            "null must be an object when present (it is null)"
        );
        assert!(optional_text(&doc, "null").is_err());
        assert!(optional_text(&doc, "list").is_err());
    }

    #[test]
    fn typed_lists_check_every_item() {
        let doc = j(r#"{"objects": [{}, 1], "strings": ["a", null], "ok": ["a"]}"#);
        assert!(optional_object_list(&doc, "objects").is_err());
        assert!(optional_string_list(&doc, "strings").is_err());
        assert_eq!(optional_string_list(&doc, "ok").expect("ok"), vec!["a"]);
        assert!(optional_object_list(&doc, "absent")
            .expect("absent")
            .is_empty());
    }

    #[test]
    fn nullable_text_is_the_explicit_opt_in() {
        let doc = j(r#"{"null": null, "text": "t", "int": 1}"#);
        assert_eq!(nullable_text(&doc, "null").expect("null"), None);
        assert_eq!(nullable_text(&doc, "absent").expect("absent"), None);
        assert_eq!(nullable_text(&doc, "text").expect("text"), Some("t"));
        let error = nullable_text(&doc, "int").expect_err("int");
        assert_eq!(error.expected(), "a string or null");
        assert_eq!(error.found(), "integer");
    }

    #[test]
    fn nullable_list_is_the_explicit_opt_in() {
        let doc = j(r#"{"null": null, "list": [1], "empty": [], "obj": {}, "text": "t"}"#);
        assert_eq!(nullable_list(&doc, "null").expect("null"), None);
        assert_eq!(nullable_list(&doc, "absent").expect("absent"), None);
        assert_eq!(
            nullable_list(&doc, "list")
                .expect("list")
                .map(<[Json]>::len),
            Some(1)
        );
        assert_eq!(
            nullable_list(&doc, "empty")
                .expect("empty")
                .map(<[Json]>::len),
            Some(0)
        );
        for (key, found) in [("obj", "object"), ("text", "string")] {
            let error = nullable_list(&doc, key).expect_err("not a list");
            assert_eq!(
                error.to_string(),
                format!("{key} must be a list or null when present (it is {found})")
            );
        }
    }

    #[test]
    fn whole_values_are_lists_or_objects_only_by_their_type() {
        let list = j("[1, 2]");
        let object = j(r#"{"a": 1, "b": 2}"#);
        assert_eq!(list_value(&list).map(<[Json]>::len), Some(2));
        assert!(list_value(&object).is_none());
        let keys: Vec<&str> = object_members(&object)
            .expect("object")
            .iter()
            .map(|(key, _)| key.as_str())
            .collect();
        assert_eq!(keys, ["a", "b"]);
        assert!(object_members(&list).is_none());
        for other in ["null", "1", "\"t\"", "true"] {
            assert!(list_value(&j(other)).is_none(), "{other}");
            assert!(object_members(&j(other)).is_none(), "{other}");
        }
    }

    #[test]
    fn member_errors_never_carry_the_value() {
        let doc = Json::object(vec![("k", Json::string(SECRET))]);
        let error = optional_list(&doc, "k").expect_err("string");
        assert!(!error.to_string().contains(SECRET));
        assert!(!format!("{error:?}").contains(SECRET));
    }

    #[test]
    fn step_fields_validate_request_and_exactly_one_outcome() {
        let step = j(r#"{"request": {"a": 1}, "response": {}}"#);
        let fields = step_fields(&step).expect("ok");
        assert!(fields.request.is_some());
        assert!(matches!(fields.outcome, RecordedOutcome::Response(_)));
        let step = j(r#"{"error": {}}"#);
        let fields = step_fields(&step).expect("ok");
        assert!(fields.request.is_none());
        assert!(matches!(fields.outcome, RecordedOutcome::Error(_)));
        for (document, expected) in [
            (
                r#"{"request": null, "response": {}}"#,
                StepShapeError::Request(MemberError {
                    key: "request".to_string(),
                    expected: "an object",
                    found: "null",
                }),
            ),
            (r#"{"response": {}, "error": {}}"#, StepShapeError::Outcome),
            (r"{}", StepShapeError::Outcome),
            (r#"{"response": null}"#, StepShapeError::Outcome),
            (r#"{"response": []}"#, StepShapeError::Outcome),
            (r#"{"error": null}"#, StepShapeError::ErrorNotObject),
            (r#"{"error": "x"}"#, StepShapeError::ErrorNotObject),
            (r"[]", StepShapeError::Outcome),
        ] {
            assert_eq!(
                step_fields(&j(document)).err(),
                Some(expected),
                "{document}"
            );
        }
    }

    #[test]
    fn same_json_ignores_key_order_only() {
        assert!(same_json(
            &j(r#"{"a":1,"b":[{"c":2,"d":3}]}"#),
            &j(r#"{"b":[{"d":3,"c":2}],"a":1}"#)
        ));
        assert!(!same_json(&j(r#"{"a":1}"#), &j(r#"{"a":1,"b":2}"#)));
        assert!(!same_json(&j("[1,2]"), &j("[2,1]")));
        assert!(KeyOrder::Ignored.same(&j(r#"{"a":1,"b":2}"#), &j(r#"{"b":2,"a":1}"#)));
        assert!(!KeyOrder::Significant.same(&j(r#"{"a":1,"b":2}"#), &j(r#"{"b":2,"a":1}"#)));
    }

    fn denied() -> AwsError {
        AwsError::service(ServiceError::new(
            "POST",
            "/",
            400,
            Some("AccessDenied"),
            "no",
            None,
        ))
    }

    #[test]
    fn failure_steps_compare_their_request_before_failing() {
        let step = Step::failure(Some(j(r#"{"a":1}"#)), denied());
        assert_eq!(
            step.clone()
                .replay(&j(r#"{"a":2}"#), KeyOrder::Ignored)
                .err(),
            Some(ReplayError::RequestMismatch)
        );
        assert_eq!(
            step.replay(&j(r#"{"a":1}"#), KeyOrder::Ignored)
                .expect("request matches"),
            Err(denied())
        );
        let any = Step::response(None, j("{}"));
        assert!(any.matches(&j(r#"{"x":1}"#), KeyOrder::Significant));
        assert_eq!(any.outcome(), Ok(j("{}")));
    }

    #[test]
    fn queues_replay_in_order_and_fail_when_exhausted() {
        let mut queue = StepQueue::new(KeyOrder::Significant);
        queue.push("op", Step::response(None, j(r#"{"n":1}"#)));
        queue.push("op", Step::failure(Some(j(r#"{"a":1,"b":2}"#)), denied()));
        queue.push("op", Step::response(None, j(r#"{"n":3}"#)));
        assert_eq!(queue.unconsumed(), 3);
        assert_eq!(queue.replay(&"op", &j("{}")), Ok(Ok(j(r#"{"n":1}"#))));
        assert_eq!(
            queue.replay(&"op", &j(r#"{"b":2,"a":1}"#)),
            Err(ReplayError::RequestMismatch)
        );
        assert_eq!(queue.unconsumed(), 1, "a mismatched step is consumed");
        assert_eq!(queue.replay(&"op", &j("{}")), Ok(Ok(j(r#"{"n":3}"#))));
        assert_eq!(queue.replay(&"op", &j("{}")), Err(ReplayError::Exhausted));
        assert_eq!(
            queue.replay(&"other", &j("{}")),
            Err(ReplayError::Exhausted)
        );
        let mut replaced = StepQueue::new(KeyOrder::Ignored);
        replaced.push(1, Step::response(None, j("{}")));
        replaced.insert(1, VecDeque::new());
        assert_eq!(replaced.unconsumed(), 0);
    }
}
