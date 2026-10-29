//! Rewriting catalogued literal assignments.

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::Json;

    #[test]
    fn ap_asn_01_02_04_05_08_literals_consent_and_replaced_cells() {
        let source = "# Cell 0 [markdown]: Test\n# test\n# Cell 1: Config\nBASE_MODEL = 'old' # comment\nACCEPT_EULA = (\n    False # consent\n)\n";
        let template = super::super::template::parse(source);
        let mut entries = BTreeMap::new();
        entries.insert("BASE_MODEL".to_owned(), Json::string("a'b"));
        entries.insert("ACCEPT_EULA".to_owned(), Json::Bool(true));
        assert_eq!(rewrite(source, &template, &entries, &BTreeSet::new(), &BTreeSet::new()).unwrap_err().code, "consent_not_given");
        let consent = BTreeSet::from(["ACCEPT_EULA".to_owned()]);
        let rendered = rewrite(source, &template, &entries, &consent, &BTreeSet::new()).unwrap();
        assert!(rendered.contains("BASE_MODEL = 'a\\'b' # comment"));
        assert!(rendered.contains("    True # consent"));
        assert_eq!(rewrite(source, &template, &entries, &consent, &BTreeSet::from([1])).unwrap_err().code, "assign_target_replaced");
        entries.insert("MISSING".to_owned(), Json::Int(1));
        assert_eq!(rewrite(source, &template, &entries, &consent, &BTreeSet::new()).unwrap_err().code, "assign_target_invalid");
    }
}
