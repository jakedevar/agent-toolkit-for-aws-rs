//! Source gates for the #4413 S1 boundary (test builds only).
//!
//! Text scans over this crate's `src/`, so they run in the default (SDK-free)
//! test build as well:
//!
//! - **T-TLS-20 (R-ROOTS):** no native-root path. No `TrustStore::default()`,
//!   no `with_native_roots(true)`; every connector or client chain that picks
//!   a TLS provider also sets a TLS context, and only from `ValidatedRoots`.
//! - **R-SRC / T-PROV:** every SDK credential-provider builder lives in
//!   `sdk/source.rs` and takes the shared provider configuration; the IMDS
//!   provider is never built directly.
//! - **R-PROV:** this crate never mutates its environment.

use std::path::{Path, PathBuf};

/// This file: it names every banned pattern, so it is not scanned.
const SELF: &str = "gates.rs";

fn sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("source dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    let sources: Vec<(String, String)> = files
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name != SELF))
        .map(|path| {
            let relative = path
                .strip_prefix(&root)
                .expect("under src")
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(&path).expect("source text");
            (relative, text)
        })
        .collect();
    assert!(sources.len() > 20, "the scan found the crate's sources");
    sources
}

/// The statement around byte `at`: from the previous `;`, `{` or `}` to the
/// next `;`.
fn statement(text: &str, at: usize) -> &str {
    let start = text[..at]
        .rfind([';', '{', '}'])
        .map_or(0, |index| index + 1);
    let end = text[at..].find(';').map_or(text.len(), |index| at + index);
    &text[start..end]
}

#[test]
fn t_tls_20_there_is_no_native_root_path() {
    let mut chains = 0;
    for (file, text) in sources() {
        for banned in ["TrustStore::default()", "with_native_roots(true)"] {
            assert!(!text.contains(banned), "{file}: {banned}");
        }
        for marker in [".tls_provider(", "build_https()"] {
            for (at, _) in text.match_indices(marker) {
                let chain = statement(&text, at);
                assert!(
                    chain.contains(".tls_context(ctx.clone())")
                        || chain.contains(".tls_context(roots.tls_context()?)"),
                    "{file}: a TLS chain without the validated context:\n{chain}"
                );
                chains += 1;
            }
        }
    }
    // The SdkTransport connector and the provider-plane client.
    assert_eq!(chains, 3, "tls_provider twice, build_https once");
}

#[test]
fn credential_provider_builders_live_only_in_the_source_module() {
    let builders = [
        "DefaultCredentialsChain::builder",
        "ProfileFileCredentialsProvider::builder",
        "WebIdentityTokenCredentialsProvider::builder",
        "EcsCredentialsProvider::builder",
        "EnvironmentVariableCredentialsProvider::new",
    ];
    for (file, text) in sources() {
        assert!(
            !text.contains("ImdsCredentialsProvider::builder"),
            "{file}: IMDS is reached only through the default chain"
        );
        for builder in builders {
            let uses = text.matches(builder).count();
            if file == "sdk/source.rs" {
                assert_eq!(uses, 1, "{file}: {builder}");
            } else {
                assert_eq!(uses, 0, "{file}: {builder} outside sdk/source.rs");
            }
        }
    }
    let source = sources()
        .into_iter()
        .find(|(file, _)| file == "sdk/source.rs")
        .expect("sdk/source.rs")
        .1;
    for builder in &builders[1..4] {
        let at = source.find(builder).expect("builder");
        let chain = &source[at..at + source[at..].find(".build()").expect("build call")];
        assert!(chain.contains(".configure(&pc)"), "{builder}: {chain}");
    }
    let at = source.find(builders[0]).expect("default chain");
    assert!(
        statement(&source, at).contains(".configure(provider_config(region, http))"),
        "the default chain takes the shared provider configuration"
    );
    // The one shared configuration always carries the injected client when
    // one is given.
    assert!(source.contains("config.with_http_client(client.clone())"));
}

#[test]
fn r_prov_this_crate_never_mutates_its_environment() {
    for (file, text) in sources() {
        for banned in ["set_var(", "remove_var("] {
            assert!(!text.contains(banned), "{file}: {banned}");
        }
    }
}
