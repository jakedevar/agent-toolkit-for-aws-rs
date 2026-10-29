//! Notebook serialization and versioned append.

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ap_nb_08_10_11_12_append_versions_and_ids() {
        let cells = vec![Json::object(vec![("cell_type", Json::string("code"))])];
        let source = r#"{"cells":[{"id":"a1"}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
        let mut ids = FixedIds(vec!["a1", "b2"].into_iter());
        let copy = append(source, cells.clone(), &mut ids).unwrap();
        assert!(copy.to_compact_string().contains("\"id\":\"b2\""));
        let invalid = source.replace("\"a1\"", "\"bad id!\"");
        assert_eq!(append(&invalid, cells.clone(), &mut ids).unwrap_err(), "notebook_cell_ids_invalid");
        let old = source.replace("\"nbformat_minor\":5", "\"nbformat_minor\":4");
        let copy = append(&old, cells.clone(), &mut ids).unwrap();
        assert!(!copy.to_compact_string().contains("\"id\":\"b2\""));
        let unsupported = source.replace("\"nbformat_minor\":5", "\"nbformat_minor\":6");
        assert_eq!(append(&unsupported, cells, &mut ids).unwrap_err(), "notebook_version_unsupported");
    }
    struct FixedIds(std::vec::IntoIter<&'static str>);
    impl IdSource for FixedIds {
        fn id(&mut self, _: usize, _: usize) -> String { self.0.next().unwrap_or("a1").to_owned() }
    }
}
