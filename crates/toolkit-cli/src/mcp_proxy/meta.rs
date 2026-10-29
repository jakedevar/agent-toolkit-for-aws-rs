//! `_meta` injection (design section 2.5): the configured metadata is merged
//! into `params._meta` of every client JSON-RPC message whose `params` is an
//! object. Keys the client already set win; new keys are appended in the
//! configured order. A `_meta` that is not an object is left untouched.

use toolkit_core::Json;

/// What an injection did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Injected {
    /// The message changed.
    pub changed: bool,
    /// Configured keys the client's value kept.
    pub kept: usize,
    /// `_meta` was present but not an object.
    pub not_object: bool,
}

impl Injected {
    fn add(&mut self, other: Self) {
        self.changed |= other.changed;
        self.kept += other.kept;
        self.not_object |= other.not_object;
    }
}

/// Inject `metadata` into `message` (one message or a batch).
pub(crate) fn inject(message: &mut Json, metadata: &[(String, String)]) -> Injected {
    let mut outcome = Injected::default();
    if metadata.is_empty() {
        return outcome;
    }
    match message {
        Json::Array(items) => {
            for item in items {
                outcome.add(inject_one(item, metadata));
            }
        }
        _ => outcome.add(inject_one(message, metadata)),
    }
    outcome
}

fn inject_one(message: &mut Json, metadata: &[(String, String)]) -> Injected {
    let mut outcome = Injected::default();
    let Json::Object(members) = message else {
        return outcome;
    };
    let Some((_, Json::Object(params))) = members.iter_mut().find(|(key, _)| key == "params")
    else {
        return outcome;
    };
    let meta = match params.iter_mut().find(|(key, _)| key == "_meta") {
        Some((_, Json::Object(meta))) => meta,
        Some(_) => {
            outcome.not_object = true;
            return outcome;
        }
        None => {
            params.push(("_meta".to_string(), Json::Object(Vec::new())));
            outcome.changed = true;
            match params.last_mut() {
                Some((_, Json::Object(meta))) => meta,
                _ => return outcome,
            }
        }
    };
    for (key, value) in metadata {
        if meta.iter().any(|(existing, _)| existing == key) {
            outcome.kept += 1;
        } else {
            meta.push((key.clone(), Json::string(value.clone())));
            outcome.changed = true;
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::parse_json;

    fn run(text: &str) -> (String, Injected) {
        let mut value = parse_json(text).unwrap();
        let meta = vec![
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "2".to_string()),
        ];
        let outcome = inject(&mut value, &meta);
        (value.to_compact_string(), outcome)
    }

    #[test]
    fn merges_with_client_keys_winning() {
        let (text, outcome) = run(r#"{"params":{"_meta":{"B":"client"}}}"#);
        assert_eq!(text, r#"{"params":{"_meta":{"B":"client","A":"1"}}}"#);
        assert_eq!(
            outcome,
            Injected {
                changed: true,
                kept: 1,
                not_object: false
            }
        );
    }

    #[test]
    fn leaves_messages_without_a_params_object_alone() {
        for text in [
            r#"{"id":1}"#,
            r#"{"params":[1]}"#,
            r#"{"params":{"_meta":3}}"#,
        ] {
            let (out, outcome) = run(text);
            assert_eq!(out, parse_json(text).unwrap().to_compact_string());
            assert!(!outcome.changed);
        }
    }

    #[test]
    fn a_meta_with_every_key_already_set_is_unchanged() {
        let (_, outcome) = run(r#"{"params":{"_meta":{"A":"x","B":"y"}}}"#);
        assert!(!outcome.changed);
        assert_eq!(outcome.kept, 2);
    }
}
