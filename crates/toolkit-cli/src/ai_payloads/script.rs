//! Script rendering and numbered publication.

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ap_scr_01_02_notebook_only_is_dropped() {
        let source = "# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\nkeep()\nmagic() # NOTEBOOK_ONLY\n# Cell 2: Skip # NOTEBOOK_ONLY_SECTION\nsecret()\n";
        let template = super::super::template::parse(source);
        let result = render(source, &template);
        assert!(result.contains("Example"));
        assert!(result.contains("# %% Cell 1: Code\nkeep()"));
        assert!(!result.contains("magic()"));
        assert!(!result.contains("secret()"));
    }
}
