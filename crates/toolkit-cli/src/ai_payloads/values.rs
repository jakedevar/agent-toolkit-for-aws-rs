//! Typed placeholder substitution.

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::{parse_json, Json};
    use std::collections::{BTreeMap, BTreeSet};

    const SOURCE: &str = "# Cell 0 [markdown]: Test\n# test\n# Cell 1: Config\nACCEPT_EULA = [ACCEPT_EULA]\nx = \"[ROLE_ARN]\"\n# [ROLE_ARN]\n";

    #[test]
    fn ap_sub_10_13_20_21_con_01_substitutes_only_code_and_refuses_missing_bare() {
        let template = super::super::template::parse(SOURCE);
        let mut values = BTreeMap::new();
        assert_eq!(substitute(SOURCE, &template, &values, &BTreeSet::new(), true).unwrap_err().code, "typed_slot_unresolved");
        values.insert("ACCEPT_EULA".into(), Json::Bool(false));
        values.insert("ROLE_ARN".into(), Json::string("a\"b\\c"));
        let output = substitute(SOURCE, &template, &values, &BTreeSet::new(), false).unwrap();
        assert!(output.contains("ACCEPT_EULA = False"));
        assert!(output.contains("x = \"a\\\"b\\\\c\""));
        assert!(output.contains("# [ROLE_ARN]"));
        values.remove("ROLE_ARN");
        assert!(substitute(SOURCE, &template, &values, &BTreeSet::new(), true).unwrap().contains("\"[ROLE_ARN]\""));
        assert_eq!(substitute(SOURCE, &template, &values, &BTreeSet::new(), false).unwrap_err().code, "slot_unresolved");
    }

    #[test]
    fn ap_sec_01_02_03_sub_15_secret_and_unknown_values_fail_without_echo() {
        let source = "# Cell 0 [markdown]: Test\n# test\n# Cell 1: Config\ntoken = \"[HF_TOKEN_OR_NONE]\"\nif token == \"[HF_TOKEN_OR_NONE]\": pass\n";
        let template = super::super::template::parse(source);
        let mut values = BTreeMap::new();
        values.insert("HF_TOKEN_OR_NONE".into(), Json::string("secret-sentinel"));
        let error = substitute(source, &template, &values, &BTreeSet::new(), false).unwrap_err();
        assert_eq!(error.code, "secret_value_invalid");
        assert!(!error.names.contains("secret-sentinel"));
        values.insert("HF_TOKEN_OR_NONE".into(), parse_json("null").unwrap());
        let output = substitute(source, &template, &values, &BTreeSet::new(), false).unwrap();
        assert!(output.contains("token = None"));
        assert!(output.contains("== \"[HF_TOKEN_OR_NONE]\""));
        values.insert("TYPO".into(), Json::Bool(true));
        assert_eq!(substitute(source, &template, &values, &BTreeSet::new(), false).unwrap_err().code, "unknown_value");
    }
}
