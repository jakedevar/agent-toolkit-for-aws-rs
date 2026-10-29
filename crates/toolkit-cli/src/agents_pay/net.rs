//! Merchant network plane.

fn safe_method(_method: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_method_allows_only_get_and_head_case_insensitively() {
        assert!(safe_method("get"));
        assert!(safe_method("HEAD"));
        for method in ["POST", "PUT", "PATCH", "DELETE", "GET ", ""] {
            assert!(!safe_method(method), "{method}");
        }
    }
}
