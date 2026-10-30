//! T-ORDER-CTOR, in-crate side (x402 S1, #5118; design R-SPEND-ORDER).
//!
//! `Approved` is the only value a spending call accepts. Outside the crate
//! the `compile_fail` doc-tests on `Approved` prove it cannot be built,
//! changed or copied. Inside the crate rustc's privacy already stops the
//! sibling modules, because the fields are private to
//! `agents_pay/policy/approved.rs` and `Approved::new` is
//! `pub(in crate::agents_pay::policy)`. This gate pins the rest from the
//! syntax tree of every file under `src/`:
//!
//! - the struct exists once, in `approved.rs`, with private fields only;
//! - `Approved::new` is `pub(in crate::agents_pay::policy)` and has exactly
//!   one use crate-wide, a call in `policy::authorize` (macro bodies are
//!   scanned too);
//! - no `Approved { .. }` literal outside `approved.rs`;
//! - no `impl` for `Approved` outside `approved.rs`, no `&mut self`
//!   receiver, and no trait impl but `Debug`;
//! - no derive on `Approved`;
//! - R-TOK-NOFRESH (S1 part): no randomness or clock source is named in the
//!   pure policy modules, so the token material cannot gain one.
//!
//! Each rule also runs over a seeded violation, so the gate cannot pass by
//! finding nothing.

#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::fs;
use std::path::Path;

use proc_macro2::{TokenStream, TokenTree};
use syn::ext::IdentExt;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Token, Type, Visibility};

const APPROVED_FILE: &str = "agents_pay/policy/approved.rs";
const AUTHORIZE_FILE: &str = "agents_pay/policy/mod.rs";
const NEW_VISIBILITY: &str = "crate::agents_pay::policy";

/// The pure S1 modules: their token material and decisions must not depend
/// on time or randomness.
const PURE_FILES: &[&str] = &[
    "agents_pay/amount.rs",
    "agents_pay/challenge.rs",
    "agents_pay/pyval.rs",
    "agents_pay/urlparse.rs",
];
const PURE_DIR: &str = "agents_pay/policy/";
const FORBIDDEN_SOURCES: &[&str] = &[
    "getrandom",
    "rand",
    "Uuid",
    "uuid",
    "thread_rng",
    "OsRng",
    "SystemTime",
    "Instant",
];

struct Source {
    path: String,
    text: String,
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<Source>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .map(|entry| entry.expect("directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(root, &path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let relative = path
                .strip_prefix(root)
                .expect("under src")
                .to_string_lossy()
                .replace('\\', "/");
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            out.push(Source {
                path: relative,
                text,
            });
        }
    }
}

fn real_sources() -> Vec<Source> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    collect(&root, &root, &mut out);
    out
}

fn seeded(files: &[(&str, &str)]) -> Vec<Source> {
    files
        .iter()
        .map(|(path, text)| Source {
            path: (*path).to_owned(),
            text: (*text).to_owned(),
        })
        .collect()
}

/// The tokens of a source with every raw identifier unraw-normalised
/// (`r#Ok` is `Ok`, as `IdentExt::unraw` gives it), spans kept. Every gate
/// in this file reads source through this or [`parse_source`], so it
/// compares identifiers after unraw everywhere (#5400). A raw keyword
/// (`r#type`) stays as written: it is an identifier, not the keyword.
fn tokens(text: &str) -> Result<TokenStream, proc_macro2::LexError> {
    text.parse::<TokenStream>().map(unraw_stream)
}

fn unraw_stream(stream: TokenStream) -> TokenStream {
    stream
        .into_iter()
        .map(|tree| match tree {
            TokenTree::Ident(ident) if ident.to_string().starts_with("r#") => {
                let plain = ident.unraw();
                if syn::parse_str::<syn::Ident>(&plain.to_string()).is_ok() {
                    TokenTree::Ident(plain)
                } else {
                    TokenTree::Ident(ident)
                }
            }
            TokenTree::Group(group) => {
                let mut inner =
                    proc_macro2::Group::new(group.delimiter(), unraw_stream(group.stream()));
                inner.set_span(group.span());
                TokenTree::Group(inner)
            }
            other => other,
        })
        .collect()
}

/// The syntax tree of a source, over [`tokens`].
fn parse_source(text: &str) -> syn::Result<syn::File> {
    let stream =
        tokens(text).map_err(|error| syn::Error::new(proc_macro2::Span::call_site(), error))?;
    syn::parse2(stream)
}

/// What the syntax tree says about `Approved`.
#[derive(Default)]
struct Analysis {
    /// Each `Approved::new` path use: (file, enclosing function).
    new_uses: Vec<(String, Option<String>)>,
    /// `Approved :: new` token sequences, macro bodies included.
    new_tokens: usize,
    /// Files that declare `struct Approved`.
    structs: Vec<String>,
    /// Struct literals of `Approved`: file.
    literals: Vec<String>,
    /// `impl` blocks for `Approved`: (file, trait name if any).
    impls: Vec<(String, Option<String>)>,
    /// Methods of `Approved` with a `&mut self` receiver: (file, name).
    mut_receivers: Vec<(String, String)>,
    /// Derives on `Approved`.
    derives: Vec<String>,
    /// Fields of `Approved` that are not private.
    public_fields: Vec<String>,
    /// The visibility of `Approved::new`, as `crate::a::b`.
    new_visibility: Vec<String>,
    /// Forbidden randomness or clock names in the pure modules: (file, name).
    forbidden: Vec<(String, String)>,
    /// Files that do not parse.
    unparsed: Vec<String>,
}

fn last_two(path: &syn::Path) -> Option<(String, String)> {
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    match segments.as_slice() {
        [.., owner, item] => Some((owner.clone(), item.clone())),
        _ => None,
    }
}

fn names_approved(ty: &Type) -> bool {
    match ty {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "Approved"),
        _ => false,
    }
}

fn path_text(path: &syn::Path) -> String {
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    segments.join("::")
}

struct FileVisitor<'a> {
    file: &'a str,
    functions: Vec<String>,
    /// Inside `impl Approved`, where `Self` is `Approved`.
    in_approved_impl: bool,
    analysis: &'a mut Analysis,
}

impl<'ast> Visit<'ast> for FileVisitor<'_> {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        self.functions.push(item.sig.ident.to_string());
        visit::visit_item_fn(self, item);
        self.functions.pop();
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.functions.push(item.sig.ident.to_string());
        visit::visit_impl_item_fn(self, item);
        self.functions.pop();
    }

    fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
        if let Some((owner, item)) = last_two(&expr.path) {
            let approved = owner == "Approved" || (owner == "Self" && self.in_approved_impl);
            if approved && item == "new" {
                self.analysis
                    .new_uses
                    .push((self.file.to_owned(), self.functions.last().cloned()));
            }
        }
        visit::visit_expr_path(self, expr);
    }

    fn visit_expr_struct(&mut self, expr: &'ast syn::ExprStruct) {
        let named = expr
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "Approved");
        let own_self = self.in_approved_impl
            && expr
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "Self");
        if named || own_self {
            self.analysis.literals.push(self.file.to_owned());
        }
        visit::visit_expr_struct(self, expr);
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let approved = names_approved(&item.self_ty);
        if approved {
            let trait_name = item.trait_.as_ref().and_then(|(_, path, _)| {
                path.segments
                    .last()
                    .map(|segment| segment.ident.to_string())
            });
            self.analysis.impls.push((self.file.to_owned(), trait_name));
            for impl_item in &item.items {
                let syn::ImplItem::Fn(method) = impl_item else {
                    continue;
                };
                if let Some(receiver) = method.sig.receiver() {
                    let mutable_borrow = matches!(
                        &*receiver.ty,
                        Type::Reference(reference) if reference.mutability.is_some()
                    );
                    if mutable_borrow {
                        self.analysis
                            .mut_receivers
                            .push((self.file.to_owned(), method.sig.ident.to_string()));
                    }
                }
                if method.sig.ident == "new" {
                    let shown = match &method.vis {
                        Visibility::Restricted(restricted) => path_text(&restricted.path),
                        Visibility::Public(_) => "pub".to_owned(),
                        Visibility::Inherited => "private".to_owned(),
                    };
                    self.analysis.new_visibility.push(shown);
                }
            }
        }
        let outer = self.in_approved_impl;
        self.in_approved_impl = approved;
        visit::visit_item_impl(self, item);
        self.in_approved_impl = outer;
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        if item.ident == "Approved" {
            self.analysis.structs.push(self.file.to_owned());
            for attr in &item.attrs {
                if !attr.path().is_ident("derive") {
                    continue;
                }
                let derived = attr
                    .parse_args_with(Punctuated::<syn::Path, Token![,]>::parse_terminated)
                    .expect("derive list");
                for path in derived {
                    self.analysis.derives.push(path_text(&path));
                }
            }
            for (index, field) in item.fields.iter().enumerate() {
                if !matches!(field.vis, Visibility::Inherited) {
                    let name = field
                        .ident
                        .as_ref()
                        .map_or_else(|| index.to_string(), ToString::to_string);
                    self.analysis.public_fields.push(name);
                }
            }
        }
        visit::visit_item_struct(self, item);
    }
}

/// Identifier and punctuation leaves of a token stream, groups flattened;
/// literals (doc comments included) are skipped.
fn leaves(stream: TokenStream, out: &mut Vec<String>) {
    for tree in stream {
        match tree {
            TokenTree::Group(group) => leaves(group.stream(), out),
            TokenTree::Ident(ident) => out.push(ident.to_string()),
            TokenTree::Punct(punct) => out.push(punct.as_char().to_string()),
            TokenTree::Literal(_) => out.push(String::new()),
        }
    }
}

fn is_pure(path: &str) -> bool {
    PURE_FILES.contains(&path) || path.starts_with(PURE_DIR)
}

fn analyse(sources: &[Source]) -> Analysis {
    let mut analysis = Analysis::default();
    for source in sources {
        let Ok(file) = parse_source(&source.text) else {
            analysis.unparsed.push(source.path.clone());
            continue;
        };
        let mut visitor = FileVisitor {
            file: &source.path,
            functions: Vec::new(),
            in_approved_impl: false,
            analysis: &mut analysis,
        };
        visitor.visit_file(&file);

        let stream: TokenStream = tokens(&source.text).expect("tokens of a parsed file");
        let mut toks = Vec::new();
        leaves(stream, &mut toks);
        analysis.new_tokens += toks
            .windows(4)
            .filter(|window| window == &["Approved", ":", ":", "new"])
            .count();
        if is_pure(&source.path) {
            for tok in &toks {
                if FORBIDDEN_SOURCES.contains(&tok.as_str()) {
                    analysis.forbidden.push((source.path.clone(), tok.clone()));
                }
            }
        }
    }
    analysis
}

/// Every rule the analysis breaks.
fn violations(analysis: &Analysis) -> Vec<String> {
    let mut out = Vec::new();
    if !analysis.unparsed.is_empty() {
        out.push(format!("unparsed files: {:?}", analysis.unparsed));
    }
    if analysis.structs != [APPROVED_FILE] {
        out.push(format!(
            "struct Approved must be declared once, in {APPROVED_FILE}: {:?}",
            analysis.structs
        ));
    }
    let expected_use = (AUTHORIZE_FILE.to_owned(), Some("authorize".to_owned()));
    if analysis.new_uses != [expected_use] {
        out.push(format!(
            "Approved::new must have exactly one use, in policy::authorize: {:?}",
            analysis.new_uses
        ));
    }
    if analysis.new_tokens != 1 {
        out.push(format!(
            "`Approved::new` appears {} times in the token streams (macro bodies included); \
             exactly one is allowed",
            analysis.new_tokens
        ));
    }
    if analysis.new_visibility != [NEW_VISIBILITY] {
        out.push(format!(
            "Approved::new must be pub(in {NEW_VISIBILITY}): {:?}",
            analysis.new_visibility
        ));
    }
    for file in &analysis.literals {
        if file != APPROVED_FILE {
            out.push(format!(
                "Approved struct literal outside {APPROVED_FILE}: {file}"
            ));
        }
    }
    for (file, trait_name) in &analysis.impls {
        if file != APPROVED_FILE {
            out.push(format!("impl for Approved outside {APPROVED_FILE}: {file}"));
        }
        if let Some(name) = trait_name {
            if name != "Debug" {
                out.push(format!("trait impl {name} for Approved in {file}"));
            }
        }
    }
    for (file, method) in &analysis.mut_receivers {
        out.push(format!("&mut self method Approved::{method} in {file}"));
    }
    if !analysis.derives.is_empty() {
        out.push(format!("derives on Approved: {:?}", analysis.derives));
    }
    if !analysis.public_fields.is_empty() {
        out.push(format!(
            "Approved fields must be private to {APPROVED_FILE}: {:?}",
            analysis.public_fields
        ));
    }
    for (file, name) in &analysis.forbidden {
        out.push(format!(
            "{name} named in the pure policy module {file} (R-TOK-NOFRESH)"
        ));
    }
    out
}

#[test]
fn t_order_ctor_the_real_sources_keep_every_approved_rule() {
    let analysis = analyse(&real_sources());
    let found = violations(&analysis);
    assert!(found.is_empty(), "T-ORDER-CTOR:\n{}", found.join("\n"));
    // Not vacuous: the struct, its one constructor use and its impl exist.
    assert_eq!(analysis.structs, [APPROVED_FILE]);
    assert_eq!(analysis.new_uses.len(), 1);
    assert!(analysis
        .impls
        .iter()
        .any(|(file, trait_name)| file == APPROVED_FILE && trait_name.is_none()));
}

const GOOD_APPROVED: &str = r"
pub struct Approved {
    token: String,
}
impl Approved {
    pub(in crate::agents_pay::policy) fn new(token: String) -> Self {
        Self { token }
    }
    pub fn token(&self) -> &str {
        &self.token
    }
}
impl std::fmt::Debug for Approved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.token[..8])
    }
}
";

const GOOD_POLICY: &str = r"
pub fn authorize(token: String) -> Result<Approved, ()> {
    Ok(Approved::new(token))
}
";

fn findings_with(extra: &[(&str, &str)], approved: &str, policy: &str) -> Vec<String> {
    let mut files = vec![(APPROVED_FILE, approved), (AUTHORIZE_FILE, policy)];
    files.extend_from_slice(extra);
    violations(&analyse(&seeded(&files)))
}

fn assert_caught(findings: &[String], needle: &str) {
    assert!(
        findings.iter().any(|finding| finding.contains(needle)),
        "expected a finding containing {needle:?}: {findings:?}"
    );
}

#[test]
fn t_order_ctor_the_seeded_good_shape_passes() {
    assert_eq!(
        findings_with(&[], GOOD_APPROVED, GOOD_POLICY),
        Vec::<String>::new()
    );
}

#[test]
fn t_order_ctor_catches_a_second_constructor_use() {
    let flow = "fn pay(token: String) { let _a = Approved::new(token); }";
    let found = findings_with(&[("agents_pay/flow.rs", flow)], GOOD_APPROVED, GOOD_POLICY);
    assert_caught(&found, "exactly one use");
    // Hidden in a macro body, or passed as a function value.
    let macro_body = "fn pay(t: String) { let _v = vec![Approved::new(t)]; }";
    let found = findings_with(
        &[("agents_pay/flow.rs", macro_body)],
        GOOD_APPROVED,
        GOOD_POLICY,
    );
    assert_caught(&found, "token streams");
    let value = "fn pay(t: Option<String>) { let _a = t.map(Approved::new); }";
    let found = findings_with(
        &[("agents_pay/vault.rs", value)],
        GOOD_APPROVED,
        GOOD_POLICY,
    );
    assert_caught(&found, "exactly one use");
    // The one use outside `authorize`.
    let elsewhere = "pub fn other(t: String) -> Approved { Approved::new(t) }";
    let found = findings_with(&[], GOOD_APPROVED, elsewhere);
    assert_caught(&found, "exactly one use");
}

#[test]
fn t_order_ctor_catches_a_wider_constructor() {
    let public = GOOD_APPROVED.replace("pub(in crate::agents_pay::policy) fn new", "pub fn new");
    assert_caught(&findings_with(&[], &public, GOOD_POLICY), "pub(in");
    let crate_wide = GOOD_APPROVED.replace(
        "pub(in crate::agents_pay::policy) fn new",
        "pub(crate) fn new",
    );
    assert_caught(&findings_with(&[], &crate_wide, GOOD_POLICY), "pub(in");
}

#[test]
fn t_order_ctor_catches_struct_literals_and_foreign_impls() {
    let literal = "fn forge() -> Approved { Approved { token: String::new() } }";
    let found = findings_with(
        &[("agents_pay/agentcore.rs", literal)],
        GOOD_APPROVED,
        GOOD_POLICY,
    );
    assert_caught(&found, "struct literal outside");
    let foreign = "impl Approved { pub fn forge() -> Self { todo!() } }";
    let found = findings_with(
        &[("agents_pay/proof.rs", foreign)],
        GOOD_APPROVED,
        GOOD_POLICY,
    );
    assert_caught(&found, "impl for Approved outside");
}

#[test]
fn t_order_ctor_catches_mutation_and_copies() {
    let setter = GOOD_APPROVED.replace(
        "pub fn token(&self)",
        "pub fn set(&mut self, t: String) { self.token = t; }\n    pub fn token(&self)",
    );
    assert_caught(
        &findings_with(&[], &setter, GOOD_POLICY),
        "&mut self method Approved::set",
    );
    let typed = GOOD_APPROVED.replace(
        "pub fn token(&self)",
        "pub fn set(self: &mut Self) {}\n    pub fn token(&self)",
    );
    assert_caught(&findings_with(&[], &typed, GOOD_POLICY), "&mut self method");
    let derived = GOOD_APPROVED.replace(
        "pub struct Approved",
        "#[derive(Clone)]\npub struct Approved",
    );
    assert_caught(
        &findings_with(&[], &derived, GOOD_POLICY),
        "derives on Approved",
    );
    let copy = GOOD_APPROVED.replace(
        "pub struct Approved",
        "#[derive(Debug, Copy, Clone)]\npub struct Approved",
    );
    assert_caught(&findings_with(&[], &copy, GOOD_POLICY), "Copy");
    let manual = format!(
        "{GOOD_APPROVED}\nimpl Clone for Approved {{ fn clone(&self) -> Self {{ todo!() }} }}"
    );
    assert_caught(
        &findings_with(&[], &manual, GOOD_POLICY),
        "trait impl Clone",
    );
    let deref = format!(
        "{GOOD_APPROVED}\nimpl std::ops::DerefMut for Approved {{ fn deref_mut(&mut self) -> &mut String {{ &mut self.token }} }}"
    );
    assert_caught(
        &findings_with(&[], &deref, GOOD_POLICY),
        "trait impl DerefMut",
    );
    let public = GOOD_APPROVED.replace("    token: String,", "    pub token: String,");
    assert_caught(
        &findings_with(&[], &public, GOOD_POLICY),
        "fields must be private",
    );
    let crate_field = GOOD_APPROVED.replace("    token: String,", "    pub(crate) token: String,");
    assert_caught(
        &findings_with(&[], &crate_field, GOOD_POLICY),
        "fields must be private",
    );
}

#[test]
fn t_order_ctor_catches_a_missing_or_duplicated_struct() {
    let found = findings_with(&[], "", GOOD_POLICY);
    assert_caught(&found, "declared once");
    let twin = "pub struct Approved;";
    let found = findings_with(&[("agents_pay/flow.rs", twin)], GOOD_APPROVED, GOOD_POLICY);
    assert_caught(&found, "declared once");
}

#[test]
fn t_order_ctor_catches_randomness_in_the_pure_policy_modules() {
    let random = "fn salt() -> u64 { let mut b = [0u8; 8]; getrandom::fill(&mut b).ok(); 0 }";
    let found = findings_with(
        &[("agents_pay/policy/token.rs", random)],
        GOOD_APPROVED,
        GOOD_POLICY,
    );
    assert_caught(&found, "getrandom named in the pure policy module");
    let clock = "fn now() { let _t = std::time::SystemTime::now(); }";
    let found = findings_with(
        &[("agents_pay/amount.rs", clock)],
        GOOD_APPROVED,
        GOOD_POLICY,
    );
    assert_caught(&found, "SystemTime");
    let in_macro = "fn id() -> String { format!(\"{}\", uuid::Uuid::new_v4()) }";
    let found = findings_with(
        &[("agents_pay/urlparse.rs", in_macro)],
        GOOD_APPROVED,
        GOOD_POLICY,
    );
    assert_caught(&found, "Uuid");
    // The admin plane may use its own UUIDs (section 6.1).
    let admin = "fn id() -> uuid::Uuid { uuid::Uuid::new_v4() }";
    let found = findings_with(
        &[("agents_pay/admin/write.rs", admin)],
        GOOD_APPROVED,
        GOOD_POLICY,
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn process_request_has_private_fields_and_only_approved_constructor() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agents_pay/agentcore/request.rs");
    let source = fs::read_to_string(path).unwrap();
    check_process_request(&source);
}

fn check_process_request(source: &str) {
    let file = parse_source(source).unwrap();
    let structs: Vec<_> = file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Struct(s) if s.ident == "ProcessPaymentRequest" => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(
        structs.len(),
        1,
        "one immutable payment request boundary is required"
    );
    assert!(structs[0]
        .fields
        .iter()
        .all(|field| matches!(field.vis, Visibility::Inherited)));
    assert!(structs[0]
        .attrs
        .iter()
        .all(|attr| !attr.path().is_ident("derive")));
    let traits: Vec<_> = file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Impl(i) if names_request(&i.self_ty) => {
                i.trait_.as_ref().map(|(_, path, _)| path_text(path))
            }
            _ => None,
        })
        .collect();
    assert_eq!(traits, ["std::fmt::Debug"]);
    let implementations: Vec<_> = file.items.iter().filter_map(|item| match item {
        syn::Item::Impl(i) if i.trait_.is_none() && matches!(&*i.self_ty, Type::Path(p) if p.path.is_ident("ProcessPaymentRequest")) => Some(i),
        _ => None,
    }).collect();
    assert_eq!(implementations.len(), 1);
    let methods: Vec<_> = implementations[0]
        .items
        .iter()
        .filter_map(|item| match item {
            syn::ImplItem::Fn(f) => Some(f),
            _ => None,
        })
        .collect();
    let constructors: Vec<_> = methods
        .iter()
        .filter(|f| f.sig.ident == "from_approved")
        .collect();
    assert_eq!(constructors.len(), 1);
    let constructor = constructors[0];
    assert!(
        matches!(&constructor.vis, Visibility::Inherited)
            || matches!(&constructor.vis, Visibility::Restricted(vis) if vis.path.is_ident("super"))
    );
    assert_eq!(constructor.sig.inputs.len(), 1);
    for method in methods.iter().filter(|f| f.sig.ident != "from_approved") {
        assert!(["body", "manager", "session", "instrument", "user"]
            .contains(&method.sig.ident.to_string().as_str()));
        assert_eq!(method.sig.inputs.len(), 1);
        assert!(method
            .sig
            .receiver()
            .is_some_and(|r| r.reference.is_some() && r.mutability.is_none()));
        assert!(
            matches!(&method.sig.output, syn::ReturnType::Type(_, ty) if matches!(&**ty, Type::Reference(reference) if reference.mutability.is_none()))
        );
    }
    let syn::FnArg::Typed(arg) = &constructor.sig.inputs[0] else {
        panic!("constructor must take Approved");
    };
    let Type::Reference(reference) = &*arg.ty else {
        panic!("constructor must borrow Approved");
    };
    assert!(matches!(&*reference.elem, Type::Path(p) if p.path.is_ident("Approved")));
    assert!(reference.mutability.is_none());
    let mut scanner = RequestScanner::default();
    scanner.visit_file(&file);
    assert_eq!(
        scanner.literals, 1,
        "only the constructor may build a request"
    );
    assert_eq!(
        scanner.bad_literals, 0,
        "request literal outside its constructor"
    );
    assert_eq!(scanner.aliases, 0, "request aliases are forbidden");
    assert_eq!(scanner.macro_mentions, 0, "request type hidden in a macro");
    assert_eq!(scanner.mutable_receivers, 0, "request mutation method");
    assert_eq!(
        scanner.field_writes, 0,
        "request fields must remain immutable"
    );
}

fn names_request(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "ProcessPaymentRequest"))
}

fn protected_field(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Field(field) => {
            matches!(&field.member, syn::Member::Named(name) if ["body", "manager", "session", "instrument", "user"].contains(&name.to_string().as_str()))
        }
        syn::Expr::Index(index) => protected_field(&index.expr),
        syn::Expr::Paren(paren) => protected_field(&paren.expr),
        _ => false,
    }
}

#[derive(Default)]
struct RequestScanner {
    request_impl: bool,
    constructor: bool,
    literals: usize,
    bad_literals: usize,
    aliases: usize,
    macro_mentions: usize,
    mutable_receivers: usize,
    field_writes: usize,
}

impl<'ast> Visit<'ast> for RequestScanner {
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        fn mentions_request(tree: &syn::UseTree) -> bool {
            match tree {
                syn::UseTree::Path(path) => {
                    path.ident == "ProcessPaymentRequest" || mentions_request(&path.tree)
                }
                syn::UseTree::Name(name) => name.ident == "ProcessPaymentRequest",
                syn::UseTree::Rename(rename) => rename.ident == "ProcessPaymentRequest",
                syn::UseTree::Group(group) => group.items.iter().any(mentions_request),
                syn::UseTree::Glob(_) => false,
            }
        }
        if mentions_request(&item.tree) {
            self.aliases += 1;
        }
        visit::visit_item_use(self, item);
    }
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if item.attrs.iter().any(|a| matches!(&a.meta, syn::Meta::List(m) if m.path.is_ident("cfg") && m.tokens.to_string() == "test")) {
            return;
        }
        visit::visit_item_mod(self, item);
    }
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let previous = self.request_impl;
        self.request_impl = names_request(&item.self_ty);
        visit::visit_item_impl(self, item);
        self.request_impl = previous;
    }
    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        let previous = self.constructor;
        self.constructor = self.request_impl && item.sig.ident == "from_approved";
        if self.request_impl
            && item
                .sig
                .receiver()
                .is_some_and(|r| matches!(&*r.ty, Type::Reference(t) if t.mutability.is_some()))
        {
            self.mutable_receivers += 1;
        }
        visit::visit_impl_item_fn(self, item);
        self.constructor = previous;
    }
    fn visit_expr_struct(&mut self, expr: &'ast syn::ExprStruct) {
        let name = expr.path.segments.last().unwrap().ident.to_string();
        if name == "ProcessPaymentRequest" || (name == "Self" && self.request_impl) {
            self.literals += 1;
            if !self.constructor {
                self.bad_literals += 1;
            }
        }
        visit::visit_expr_struct(self, expr);
    }
    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        struct TypeMention(bool);
        impl<'ast> Visit<'ast> for TypeMention {
            fn visit_path(&mut self, path: &'ast syn::Path) {
                if path
                    .segments
                    .iter()
                    .any(|s| s.ident == "ProcessPaymentRequest")
                {
                    self.0 = true;
                }
                visit::visit_path(self, path);
            }
        }
        let mut mention = TypeMention(false);
        mention.visit_type(&item.ty);
        if mention.0 {
            self.aliases += 1;
        }
        visit::visit_item_type(self, item);
    }
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let mut tokens = Vec::new();
        leaves(mac.tokens.clone(), &mut tokens);
        if tokens.iter().any(|t| t == "ProcessPaymentRequest")
            || (self.request_impl && tokens.iter().any(|t| t == "Self"))
            || tokens.windows(3).any(|window| {
                window[0] == "."
                    && ["body", "manager", "session", "instrument", "user"]
                        .contains(&window[1].as_str())
                    && window[2] == "="
            })
        {
            self.macro_mentions += 1;
        }
        visit::visit_macro(self, mac);
    }
    fn visit_expr_assign(&mut self, expr: &'ast syn::ExprAssign) {
        if protected_field(&expr.left) {
            self.field_writes += 1;
        }
        visit::visit_expr_assign(self, expr);
    }
    fn visit_expr_binary(&mut self, expr: &'ast syn::ExprBinary) {
        if matches!(
            expr.op,
            syn::BinOp::AddAssign(_)
                | syn::BinOp::SubAssign(_)
                | syn::BinOp::MulAssign(_)
                | syn::BinOp::DivAssign(_)
                | syn::BinOp::RemAssign(_)
                | syn::BinOp::BitXorAssign(_)
                | syn::BinOp::BitAndAssign(_)
                | syn::BinOp::BitOrAssign(_)
                | syn::BinOp::ShlAssign(_)
                | syn::BinOp::ShrAssign(_)
        ) && protected_field(&expr.left)
        {
            self.field_writes += 1;
        }
        visit::visit_expr_binary(self, expr);
    }
    fn visit_expr_reference(&mut self, expr: &'ast syn::ExprReference) {
        if expr.mutability.is_some() && protected_field(&expr.expr) {
            self.field_writes += 1;
        }
        visit::visit_expr_reference(self, expr);
    }
    fn visit_expr_method_call(&mut self, expr: &'ast syn::ExprMethodCall) {
        if protected_field(&expr.receiver) && expr.method != "clone" {
            self.field_writes += 1;
        }
        visit::visit_expr_method_call(self, expr);
    }
}

#[test]
fn request_gate_rejects_use_alias_literal() {
    let source = r"
        pub struct ProcessPaymentRequest { body: Vec<u8> }
        impl ProcessPaymentRequest { fn from_approved(approved: &Approved) -> Self { Self { body: vec![] } } }
        impl std::fmt::Debug for ProcessPaymentRequest { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { todo!() } }
        use self::ProcessPaymentRequest as RequestAlias;
        fn forge() -> RequestAlias { RequestAlias { body: vec![] } }
    ";
    assert!(std::panic::catch_unwind(|| check_process_request(source)).is_err());
}

#[test]
fn request_gate_rejects_index_and_compound_writes() {
    let source = r"
        pub struct ProcessPaymentRequest { body: Vec<u8>, manager: String }
        impl ProcessPaymentRequest { fn from_approved(approved: &Approved) -> Self { Self { body: vec![], manager: String::new() } } }
        impl std::fmt::Debug for ProcessPaymentRequest { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { todo!() } }
        fn change(r: &mut ProcessPaymentRequest) { WRITE }
    ";
    for write in ["r.body[0] = b'x';", "r.manager += \"\";"] {
        let bad = source.replace("WRITE", write);
        assert!(
            std::panic::catch_unwind(|| check_process_request(&bad)).is_err(),
            "{write}"
        );
    }
}

#[test]
fn process_request_gate_rejects_extra_trait_constructors() {
    let source = r"
        pub struct ProcessPaymentRequest { body: Vec<u8> }
        impl ProcessPaymentRequest {
            fn from_approved(approved: &Approved) -> Self { todo!() }
        }
        impl Default for ProcessPaymentRequest {
            fn default() -> Self { Self { body: vec![] } }
        }
    ";
    assert!(std::panic::catch_unwind(|| check_process_request(source)).is_err());
}

#[test]
fn process_request_gate_rejects_alias_macro_literal_and_mutation() {
    let good = r"
        pub struct ProcessPaymentRequest { body: Vec<u8> }
        impl ProcessPaymentRequest {
            fn from_approved(approved: &Approved) -> Self { Self { body: vec![] } }
        }
        impl std::fmt::Debug for ProcessPaymentRequest {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { todo!() }
        }
    ";
    check_process_request(good);
    for extra in [
        "type Alias = ProcessPaymentRequest;",
        "fn forge() { let _ = ProcessPaymentRequest { body: vec![] }; }",
        "fn forge() { helper!(ProcessPaymentRequest { body: vec![] }); }",
        "fn change(r: &mut ProcessPaymentRequest) { r.body = vec![]; }",
        "fn change(r: &mut ProcessPaymentRequest) { mutate(&mut r.body); }",
        "fn change(r: &mut ProcessPaymentRequest) { r.body.clear(); }",
    ] {
        let bad = format!("{good}\n{extra}");
        assert!(
            std::panic::catch_unwind(|| check_process_request(&bad)).is_err(),
            "{extra}"
        );
    }
}

#[test]
fn process_request_gate_rejects_macro_hidden_self_and_field_mutation() {
    let good = r"
        pub struct ProcessPaymentRequest { body: Vec<u8> }
        impl ProcessPaymentRequest {
            fn from_approved(approved: &Approved) -> Self { Self { body: vec![] } }
        }
        impl std::fmt::Debug for ProcessPaymentRequest {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { todo!() }
        }
    ";
    let hidden_write = good.replace(
        "Self { body: vec![] }",
        "{ let mut r = Self { body: vec![] }; helper!(r.body = vec![]); r }",
    );
    let hidden_self = good.replace("{ todo!() }", "{ helper!(Self { body: vec![] }); todo!() }");
    for bad in [&hidden_write, &hidden_self] {
        assert!(std::panic::catch_unwind(|| check_process_request(bad)).is_err());
    }
}

#[test]
fn payment_service_model_is_embedded() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agents_pay/agentcore.rs");
    let source = fs::read_to_string(path).unwrap();
    let file = parse_source(&source).unwrap();
    let embedded = file.items.iter().any(|item| match item {
        syn::Item::Const(c) if c.ident == "PAYMENT_SERVICE_MODEL" => {
            matches!(&*c.expr, syn::Expr::Macro(m) if m.mac.path.is_ident("include_str") && m.mac.tokens.to_string().contains("agentcore-payments.json"))
        }
        _ => false,
    });
    assert!(
        embedded,
        "the pinned payment service model must be embedded"
    );
}

// =====================================================================
// x402 S5 (#5352): G1a / T-AC3-GUARD, T-AC3-FEAT, T-AC3-MAKE, G4, the proof
// gate (R-SPEND-ORDER rule 5) and R-TOK-NOFRESH for the flow modules.
// The feature name is assembled here so that this file, like every other
// test file, spells it only in its own G1a guard.

const LIVE_FEATURE: &str = concat!("agents-pay", "-live");

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> std::path::PathBuf {
    manifest_dir().join("../..")
}

/// The test crate roots Cargo discovers: `tests/*.rs` and `tests/*/main.rs`.
fn test_crate_roots() -> Vec<(String, String)> {
    let dir = manifest_dir().join("tests");
    let mut roots = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        let root = if path.is_dir() {
            path.join("main.rs")
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            path.clone()
        } else {
            continue;
        };
        if root.is_file() {
            let name = root
                .strip_prefix(manifest_dir())
                .unwrap()
                .to_string_lossy()
                .into_owned();
            roots.push((name, fs::read_to_string(&root).unwrap()));
        }
    }
    roots
}

/// Whether a crate root's first item is the G1a guard (after its inner
/// doc comments and inner attributes).
fn has_test_guard(text: &str) -> bool {
    let Ok(file) = parse_source(text) else {
        return false;
    };
    let Some(syn::Item::Macro(item)) = file.items.first() else {
        return false;
    };
    let cfg = item.attrs.iter().any(|attr| match &attr.meta {
        syn::Meta::List(list) => {
            list.path.is_ident("cfg")
                && list.tokens.to_string().replace(' ', "") == format!("feature=\"{LIVE_FEATURE}\"")
        }
        _ => false,
    });
    cfg && item.mac.path.is_ident("compile_error")
        && item
            .mac
            .tokens
            .to_string()
            .contains("never built for tests")
}

/// Spellings of the live feature outside the G1a guard (two per guard).
fn stray_feature_names(text: &str) -> usize {
    text.matches(LIVE_FEATURE).count().saturating_sub(2)
}

fn ac3_guard_findings(roots: &[(String, String)]) -> Vec<String> {
    let mut found = Vec::new();
    for (name, text) in roots {
        if !has_test_guard(text) {
            found.push(format!("{name}: the G1a guard is not the first item"));
        }
        if stray_feature_names(text) > 0 {
            found.push(format!("{name}: names the live feature outside its guard"));
        }
    }
    found
}

#[test]
fn t_ac3_guard_every_test_crate_root_refuses_the_live_feature() {
    let roots = test_crate_roots();
    assert!(roots.len() > 20, "found {} test crate roots", roots.len());
    let found = ac3_guard_findings(&roots);
    assert!(found.is_empty(), "{found:#?}");
    // The library refuses test and doctest builds with the feature as well.
    let lib = fs::read_to_string(manifest_dir().join("src/lib.rs")).unwrap();
    assert!(lib.contains(&format!(
        "#[cfg(all(any(test, doctest), feature = \"{LIVE_FEATURE}\"))]\ncompile_error!(\"{LIVE_FEATURE} is never built for tests (AC3)\");"
    )));
    // No example or bench target can link the payment modules.
    for dir in ["examples", "benches"] {
        assert!(!manifest_dir().join(dir).exists(), "{dir}/ exists");
    }
    let manifest = fs::read_to_string(manifest_dir().join("Cargo.toml")).unwrap();
    assert!(!manifest.contains("[[example]]") && !manifest.contains("[[bench]]"));
}

#[test]
fn t_ac3_guard_catches_a_missing_late_or_stray_guard() {
    let guard = format!(
        "#[cfg(feature = \"{LIVE_FEATURE}\")]\ncompile_error!(\"{LIVE_FEATURE} is never built for tests (AC3)\");\n"
    );
    let good = format!("//! doc\n#![cfg(unix)]\n{guard}use std::fs;\n#[test]\nfn t() {{}}\n");
    assert!(ac3_guard_findings(&[("good.rs".into(), good)]).is_empty());
    let cases = [
        "//! no guard\n#[test]\nfn t() {}\n".to_owned(),
        format!("use std::fs;\n{guard}"),
        format!("{guard}#[cfg(feature = \"{LIVE_FEATURE}\")]\nfn live() {{}}\n"),
        "#[cfg(feature = \"aws-sdk\")]\ncompile_error!(\"never built for tests\");\n".to_owned(),
    ];
    for case in cases {
        assert!(
            !ac3_guard_findings(&[("bad.rs".into(), case.clone())]).is_empty(),
            "{case}"
        );
    }
}

/// T-AC3-FEAT: only the toolkit-cli feature table defines the feature, as
/// `["aws-sdk"]`, and nothing (default, aws-sdk, a dependent's `features`)
/// enables it.
fn feature_findings(manifests: &[(String, String)]) -> Vec<String> {
    let mut found = Vec::new();
    let mut defined = 0;
    for (name, text) in manifests {
        let mut section = String::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                trimmed.clone_into(&mut section);
                continue;
            }
            if trimmed.starts_with('#') || !trimmed.contains(LIVE_FEATURE) {
                continue;
            }
            let definition = format!("{LIVE_FEATURE} = [\"aws-sdk\"]");
            if name.ends_with("toolkit-cli/Cargo.toml")
                && section == "[features]"
                && trimmed == definition
            {
                defined += 1;
            } else {
                found.push(format!("{name}: `{trimmed}` in {section}"));
            }
        }
        if text.contains("all-features") {
            found.push(format!("{name}: all-features"));
        }
    }
    if defined != 1 {
        found.push(format!("the feature is defined {defined} times"));
    }
    found
}

fn workspace_manifests() -> Vec<(String, String)> {
    let root = repo_root();
    let workspace = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let mut manifests = vec![("Cargo.toml".to_owned(), workspace.clone())];
    let members = workspace
        .lines()
        .find(|line| line.trim_start().starts_with("members"))
        .expect("members");
    for member in members.split('"').skip(1).step_by(2) {
        let path = format!("{member}/Cargo.toml");
        manifests.push((path.clone(), fs::read_to_string(root.join(&path)).unwrap()));
    }
    manifests
}

#[test]
fn t_ac3_feat_no_default_or_dependency_enables_the_live_feature() {
    let manifests = workspace_manifests();
    assert!(manifests
        .iter()
        .any(|(n, _)| n.ends_with("toolkit-cli/Cargo.toml")));
    let found = feature_findings(&manifests);
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn t_ac3_feat_catches_default_and_dependency_enablement() {
    let good = format!("[features]\ndefault = []\naws-sdk = []\n{LIVE_FEATURE} = [\"aws-sdk\"]\n");
    let cli = "crates/toolkit-cli/Cargo.toml".to_owned();
    assert!(feature_findings(&[(cli.clone(), good.clone())]).is_empty());
    for bad in [
        format!("[features]\ndefault = [\"{LIVE_FEATURE}\"]\n{LIVE_FEATURE} = [\"aws-sdk\"]\n"),
        format!("[features]\naws-sdk = [\"{LIVE_FEATURE}\"]\n{LIVE_FEATURE} = [\"aws-sdk\"]\n"),
        format!("[features]\n{LIVE_FEATURE} = []\n"),
    ] {
        assert!(
            !feature_findings(&[(cli.clone(), bad.clone())]).is_empty(),
            "{bad}"
        );
    }
    let dependent = format!(
        "[dependencies]\ntoolkit-cli = {{ path = \"../toolkit-cli\", features = [\"{LIVE_FEATURE}\"] }}\n"
    );
    assert!(!feature_findings(&[
        (cli.clone(), good.clone()),
        ("crates/other/Cargo.toml".into(), dependent)
    ])
    .is_empty());
}

/// T-AC3-MAKE: only `clippy-live` names the feature, as a lint of the
/// library and binaries with no test target; nothing uses `--all-features`.
fn make_findings(makefile: &str, recipes: &[(String, String)]) -> Vec<String> {
    let mut found = Vec::new();
    let mut target = String::new();
    for line in makefile.lines() {
        if !line.starts_with('\t') && line.contains(':') && !line.starts_with('#') {
            line.split(':')
                .next()
                .unwrap_or_default()
                .trim()
                .clone_into(&mut target);
        }
        if line.contains("all-features") {
            found.push(format!("`{line}` uses --all-features"));
        }
        if !line.contains(LIVE_FEATURE) {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let lint_only = target == "clippy-live"
            && words.starts_with(&["cargo", "clippy"])
            && words.contains(&"--lib")
            && words.contains(&"--bins")
            && !words.iter().any(|w| {
                matches!(
                    *w,
                    "--all-targets"
                        | "--tests"
                        | "--test"
                        | "--benches"
                        | "--examples"
                        | "--doc"
                        | "test"
                )
            });
        if !lint_only {
            found.push(format!(
                "target `{target}` passes the live feature: `{line}`"
            ));
        }
    }
    for (name, text) in recipes {
        if text.contains("all-features") {
            found.push(format!("{name}: all-features"));
        }
        if text.contains(LIVE_FEATURE) {
            found.push(format!("{name}: names the live feature"));
        }
    }
    found
}

#[test]
fn t_ac3_make_no_test_target_passes_the_live_feature() {
    let root = repo_root();
    let makefile = fs::read_to_string(root.join("Makefile")).unwrap();
    assert!(
        makefile.contains("clippy-live:"),
        "make clippy-live (Q24) is missing"
    );
    let recipes: Vec<(String, String)> = [".rsi/project.toml", ".rsi/jobs.toml"]
        .iter()
        .filter_map(|name| {
            fs::read_to_string(root.join(name))
                .ok()
                .map(|text| ((*name).to_owned(), text))
        })
        .collect();
    let found = make_findings(&makefile, &recipes);
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn t_ac3_make_catches_a_test_target_with_the_feature() {
    let good = format!(
        "clippy-live:\n\tcargo clippy -p toolkit-cli --features {LIVE_FEATURE} --lib --bins --locked -- -D warnings\n"
    );
    assert!(make_findings(&good, &[]).is_empty());
    for bad in [
        format!("test-live:\n\tcargo test -p toolkit-cli --features {LIVE_FEATURE}\n"),
        format!(
            "clippy-live:\n\tcargo clippy -p toolkit-cli --features {LIVE_FEATURE} --all-targets\n"
        ),
        "test:\n\tcargo test --workspace --all-features\n".to_owned(),
    ] {
        assert!(!make_findings(&bad, &[]).is_empty(), "{bad}");
    }
}

// ---------------------------------------------------------------- G4

/// Names a test must never spell: the live constructors and real sources.
const LIVE_PATHS: &[(&str, &str)] = &[("Deps", "for_process"), ("PaymentRoots", "from_process")];
const LIVE_IDENTS: &[&str] = &[
    "SdkTransport",
    "SystemResolver",
    "OsAccount",
    "default_chain",
    "default_chain_with_http",
];

fn idents(stream: TokenStream, out: &mut Vec<String>) {
    for tree in stream {
        match tree {
            TokenTree::Group(group) => {
                // The opening delimiter, so `name(` and `Type {` are visible.
                out.push(
                    match group.delimiter() {
                        proc_macro2::Delimiter::Parenthesis => "(",
                        proc_macro2::Delimiter::Brace => "{",
                        proc_macro2::Delimiter::Bracket => "[",
                        proc_macro2::Delimiter::None => "",
                    }
                    .to_owned(),
                );
                idents(group.stream(), out);
            }
            TokenTree::Ident(ident) => out.push(ident.to_string()),
            TokenTree::Punct(punct) => out.push(punct.as_char().to_string()),
            TokenTree::Literal(_) => out.push(String::new()),
        }
    }
}

struct MethodCalls(Vec<String>);
impl<'ast> Visit<'ast> for MethodCalls {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.0.push(call.method.to_string());
        visit::visit_expr_method_call(self, call);
    }
}

fn g4_findings(files: &[(String, String)]) -> Vec<String> {
    let mut found = Vec::new();
    for (name, text) in files {
        let Ok(stream) = tokens(text) else {
            found.push(format!("{name}: does not tokenize"));
            continue;
        };
        let mut words = Vec::new();
        idents(stream, &mut words);
        for (index, word) in words.iter().enumerate() {
            if LIVE_IDENTS.contains(&word.as_str()) {
                found.push(format!("{name}: names {word}"));
            }
            let path_after = |n: usize| words.get(index + n).map(String::as_str);
            if word == "live" && path_after(1) == Some(":") && path_after(2) == Some(":") {
                found.push(format!("{name}: names live::"));
            }
            for (head, tail) in LIVE_PATHS {
                if word == head
                    && path_after(1) == Some(":")
                    && path_after(2) == Some(":")
                    && path_after(3) == Some(tail)
                {
                    found.push(format!("{name}: names {head}::{tail}"));
                }
            }
        }
        if words.iter().any(|w| w == "wiring") {
            if let Ok(file) = parse_source(text) {
                let mut calls = MethodCalls(Vec::new());
                calls.visit_file(&file);
                if calls.0.iter().any(|m| m == "open" || m == "send") {
                    found.push(format!("{name}: calls open/send near the wiring helpers"));
                }
            }
        }
    }
    found
}

/// The bodies of every inline `#[cfg(test)] mod NAME { .. }` in `text`, at
/// any depth (#5377): the in-file unit tests of a production source.
fn inline_test_modules(stream: TokenStream, out: &mut Vec<String>) {
    let trees: Vec<TokenTree> = stream.into_iter().collect();
    for (index, tree) in trees.iter().enumerate() {
        let TokenTree::Group(group) = tree else {
            continue;
        };
        let is_cfg_test = group.delimiter() == proc_macro2::Delimiter::Bracket
            && compact(&group.stream()) == "cfg(test)"
            && matches!(index.checked_sub(1).map(|i| &trees[i]), Some(TokenTree::Punct(p)) if p.as_char() == '#');
        if is_cfg_test {
            // Skip further attributes and the visibility, then `mod NAME {`.
            let rest = &trees[index + 1..];
            let at_mod = rest.iter().position(|t| match t {
                TokenTree::Punct(p) => p.as_char() != '#',
                TokenTree::Group(g) => !matches!(
                    g.delimiter(),
                    proc_macro2::Delimiter::Bracket | proc_macro2::Delimiter::Parenthesis
                ),
                TokenTree::Ident(i) => i != "pub",
                TokenTree::Literal(_) => true,
            });
            let at_mod =
                at_mod.filter(|&at| matches!(&rest[at], TokenTree::Ident(i) if i == "mod"));
            if let Some(at) = at_mod {
                if let Some(TokenTree::Group(body)) = rest.get(at + 2) {
                    if body.delimiter() == proc_macro2::Delimiter::Brace {
                        out.push(body.stream().to_string());
                    }
                }
            }
        }
        inline_test_modules(group.stream(), out);
    }
}

/// The payment test files: integration tests, the in-crate test files and
/// the inline test modules of every payment source (#5377).
fn payment_test_files() -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = test_crate_roots()
        .into_iter()
        .filter(|(name, _)| name.starts_with("tests/agents_pay"))
        .collect();
    for source in real_sources() {
        if !source.path.starts_with("agents_pay/") {
            continue;
        }
        if source.path.ends_with("/tests.rs") || source.path.contains("/tests/") {
            files.push((source.path, source.text));
        } else if let Ok(stream) = tokens(&source.text) {
            let mut modules = Vec::new();
            inline_test_modules(stream, &mut modules);
            for module in modules {
                files.push((format!("{} (inline tests)", source.path), module));
            }
        } else {
            files.push((source.path, "fn ) does not tokenize".to_owned()));
        }
    }
    files
}

/// G4 findings over the payment test files. There is no allowance: the
/// process world (`Deps::for_process`) exists only under the live feature,
/// so no default-build test can name it (D32 is the route's own literal).
fn g4_unexpected(files: &[(String, String)]) -> Vec<String> {
    g4_findings(files)
}

#[test]
fn g4_payment_tests_never_name_a_live_constructor() {
    let files = payment_test_files();
    assert!(files.len() >= 8, "{}", files.len());
    assert!(
        files
            .iter()
            .any(|(name, _)| name == "agents_pay/deps.rs (inline tests)"),
        "the inline test modules are not scanned"
    );
    let found = g4_unexpected(&files);
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn g4_scans_inline_test_modules() {
    let deps = real_source("agents_pay/deps.rs");
    let anchor = "    fn g3_test_identities_are_recognized() {\n";
    assert!(deps.contains(anchor), "the deps unit test moved");
    let inline = |text: &str| -> Vec<(String, String)> {
        let mut modules = Vec::new();
        inline_test_modules(tokens(text).unwrap(), &mut modules);
        modules
            .into_iter()
            .map(|m| ("agents_pay/deps.rs (inline tests)".to_owned(), m))
            .collect()
    };
    assert!(g4_unexpected(&inline(&deps)).is_empty());
    // The #5377 mutation: a live resolver named inside the inline test.
    for live in [
        "let _ = toolkit_aws::SystemResolver;",
        "let _ = Deps::for_process();",
    ] {
        let bad = deps.replacen(anchor, &format!("{anchor}        {live}\n"), 1);
        assert!(
            !g4_unexpected(&inline(&bad)).is_empty(),
            "inline mutation survived: {live}"
        );
    }
    // Production code outside the test module is not a test file.
    let mut modules = Vec::new();
    inline_test_modules(
        tokens("fn live() { let _ = SystemResolver; } #[cfg(test)] mod tests { fn t() {} }")
            .unwrap(),
        &mut modules,
    );
    assert_eq!(modules.len(), 1);
    assert!(!modules[0].contains("SystemResolver"));
}

#[test]
fn g4_catches_live_names_and_wiring_sends() {
    for bad in [
        "fn t() { let _ = toolkit_cli::agents_pay::live::World::new(); }",
        "fn t() { let _ = Deps::for_process(); }",
        "fn t() { let _ = PaymentRoots::from_process(); }",
        "fn t() { let _ = SystemResolver; }",
        "fn t() { let t = SdkTransport::new(&r, &roots); }",
        "fn t() { default_chain_with_http(&r, &c, &h); }",
        "fn t() { let m = wiring::merchant_transport(&r, &roots).unwrap(); m.open(&q, &l); }",
    ] {
        assert!(
            !g4_findings(&[("tests/agents_pay_x.rs".into(), bad.into())]).is_empty(),
            "{bad}"
        );
    }
    let good = "fn t() { let deps = Deps { config: &c }; let _ = wiring::merchant_transport(&r, &roots); }";
    assert!(g4_findings(&[("tests/agents_pay_x.rs".into(), good.into())]).is_empty());
}

// ------------------------------------------- the proof gate (rule 5)

/// Where the proof may be unwrapped, built into a header, or verified.
const PROOF_FILE: &str = "agents_pay/proof.rs";

fn proof_findings(sources: &[Source]) -> Vec<String> {
    let mut found = Vec::new();
    for source in sources {
        let Ok(stream) = tokens(&source.text) else {
            continue;
        };
        let mut words = Vec::new();
        idents(stream, &mut words);
        let in_proof = source.path == PROOF_FILE;
        let in_agentcore = source.path == "agents_pay/agentcore.rs";
        let net_tests = source.path == "agents_pay/net/tests.rs";
        for (index, word) in words.iter().enumerate() {
            let next = |n: usize| words.get(index + n).map(String::as_str);
            let prev = index
                .checked_sub(1)
                .and_then(|i| words.get(i))
                .map(String::as_str);
            if word == "into_parts" && prev == Some(".") && !in_proof {
                found.push(format!(
                    "{}: Authorized::into_parts outside proof.rs",
                    source.path
                ));
            }
            if word == "payload"
                && prev == Some(".")
                && next(1) == Some("(")
                && !in_proof
                && !in_agentcore
            {
                found.push(format!(
                    "{}: PaymentProof::payload outside proof.rs",
                    source.path
                ));
            }
            if word == "VerifiedProof" && next(1) == Some("{") && !in_proof {
                found.push(format!(
                    "{}: VerifiedProof literal outside proof.rs",
                    source.path
                ));
            }
            if word == "ProofHeader"
                && next(1) == Some(":")
                && next(3) == Some("new")
                && !in_proof
                && !net_tests
            {
                found.push(format!(
                    "{}: ProofHeader::new outside proof.rs",
                    source.path
                ));
            }
        }
    }
    found
}

#[test]
fn proof_gate_only_proof_rs_unwraps_verifies_and_builds_headers() {
    let sources = real_sources();
    assert!(sources.iter().any(|s| s.path == PROOF_FILE));
    let found = proof_findings(&sources);
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn proof_gate_catches_unwrapping_and_header_forgery() {
    for bad in [
        "fn f(a: Authorized) { let _ = a.into_parts(); }",
        "fn f(p: &PaymentProof) { let _ = p.payload(); }",
        "fn f(p: Json) -> VerifiedProof { VerifiedProof { payload: p } }",
        "fn f() -> ProofHeader { ProofHeader::new(String::new(), String::new()) }",
    ] {
        let sources = seeded(&[("agents_pay/flow.rs", bad)]);
        assert!(!proof_findings(&sources).is_empty(), "{bad}");
        let allowed = seeded(&[(PROOF_FILE, bad)]);
        assert!(proof_findings(&allowed).is_empty(), "{bad}");
    }
}

// ----------------------------------------------- R-TOK-NOFRESH (S5)

const NOFRESH_FILES: &[&str] = &["agents_pay/flow.rs", "agents_pay/proof.rs"];
const RANDOM_NAMES: &[&str] = &["getrandom", "rand", "Uuid", "uuid", "thread_rng", "OsRng"];

/// The tokens of every `fn NAME` body, at any depth.
fn fn_bodies(stream: TokenStream, name: &str, out: &mut Vec<String>) {
    let trees: Vec<TokenTree> = stream.into_iter().collect();
    for (index, tree) in trees.iter().enumerate() {
        if let TokenTree::Group(group) = tree {
            fn_bodies(group.stream(), name, out);
        }
        let is_fn = matches!(tree, TokenTree::Ident(i) if i == "fn");
        let named = matches!(trees.get(index + 1), Some(TokenTree::Ident(i)) if i == name);
        if is_fn && named {
            let body = trees[index + 2..].iter().find_map(|t| match t {
                TokenTree::Group(g) if g.delimiter() == proc_macro2::Delimiter::Brace => {
                    Some(g.stream())
                }
                _ => None,
            });
            if let Some(body) = body {
                idents(body, out);
            }
        }
    }
}

fn nofresh_findings(sources: &[Source]) -> Vec<String> {
    let mut found = Vec::new();
    for source in sources {
        let words = if NOFRESH_FILES.contains(&source.path.as_str()) {
            let Ok(stream) = tokens(&source.text) else {
                continue;
            };
            let mut words = Vec::new();
            idents(stream, &mut words);
            words
        } else if source.path == "agents_pay/agentcore.rs" {
            let Ok(stream) = tokens(&source.text) else {
                found.push("agentcore.rs does not tokenize".into());
                continue;
            };
            let mut words = Vec::new();
            fn_bodies(stream, "process_payment", &mut words);
            words
        } else {
            continue;
        };
        for word in words {
            if RANDOM_NAMES.contains(&word.as_str()) {
                found.push(format!("{}: names {word}", source.path));
            }
        }
    }
    found
}

#[test]
fn r_tok_nofresh_the_flow_and_spend_path_name_no_randomness() {
    let sources = real_sources();
    assert!(sources.iter().any(|s| s.path == "agents_pay/flow.rs"));
    let found = nofresh_findings(&sources);
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn r_tok_nofresh_catches_a_fresh_token_source() {
    for (path, bad) in [
        (
            "agents_pay/flow.rs",
            "fn retry() { let mut b = [0u8; 32]; getrandom::fill(&mut b).ok(); }",
        ),
        (
            "agents_pay/proof.rs",
            "fn n() -> String { uuid::Uuid::new_v4().to_string() }",
        ),
        (
            "agents_pay/agentcore.rs",
            "pub fn process_payment() { let _ = rand::random::<u64>(); }",
        ),
    ] {
        assert!(
            !nofresh_findings(&seeded(&[(path, bad)])).is_empty(),
            "{bad}"
        );
    }
    let vault = "fn handle() { let mut b = [0u8; 24]; getrandom::fill(&mut b).ok(); }";
    assert!(nofresh_findings(&seeded(&[("agents_pay/vault.rs", vault)])).is_empty());
}

// ------------ D32 structural refusal (#5376, #5390, #5391, #5400, AC3)
//
// A build without the live feature never parses argv, builds `Deps` or
// calls a helper on `toolkit agents-pay fetch`. Its registry entry is
// `self::no_payment::fetch_command`, in the file module `no_payment.rs`:
// one call to one D32 function, which writes one string literal and sets
// exit 2 (PM rulings on #5389 and #5397). That file uses absolute paths only
// and has no `use`, no macro invocation and no other item, so no module
// item, import, raw identifier or textual macro can shadow a name it
// resolves, and the gate pins its whole token stream: any change to it is a
// finding. In `mod.rs` the gate pins the `mod` declaration and the two
// cfg-split registry entries. Under `src/agents_pay/` it bans raw
// identifiers, `macro_rules!`, `use .. as` renames, `extern crate` and
// attributes that run code outside a call. Every gate in this file compares
// identifiers after unraw ([`tokens`], #5400).
//
// Repair 4 (PM ruling on #5419): the registry pin is bound to the entries
// `SLICE` compiles ([`slice_findings`], #5421); `global_allocator`,
// `global_asm!`, `asm!` and `naked` are banned in the whole crate, and the
// `unsafe_code = "forbid"` lint the crate inherits is pinned
// ([`premain_findings`], [`lint_findings`], #5423). The gate is a regression
// guard against honest mistakes, not an adversary-proof sandbox: the
// runtime hostile-environment child test (`agents_pay_ac3.rs`) is the
// second, independent layer, and `docs/migration/agents-pay.md` records
// what each covers and the accepted residuals (#5422).

const ROUTE_FILE: &str = "agents_pay/mod.rs";
const NO_PAYMENT_FILE: &str = "agents_pay/no_payment.rs";

/// The whole of `no_payment.rs`, compared token for token after unraw.
/// Comments are not tokens; a doc comment is, so the file has none.
const NO_PAYMENT_PIN: &str = r"
pub(super) fn fetch_command(
    _invocation: &crate::Invocation,
    out: &mut crate::Output<'_>,
) -> ::core::result::Result<(), ::toolkit_core::ToolkitError> {
    self::refuse_without_payment_support(out)
}

fn refuse_without_payment_support(
    out: &mut crate::Output<'_>,
) -> ::core::result::Result<(), ::toolkit_core::ToolkitError> {
    out.line(super::D32_REFUSAL)?;
    out.set_exit_status(2);
    ::core::result::Result::Ok(())
}
";

/// The `mod` declaration in `mod.rs`, with no other attribute.
fn no_payment_decl() -> String {
    format!("#[cfg(not(feature = \"{LIVE_FEATURE}\"))]\nmod no_payment;")
}

/// The registry's `handler:` entries in `mod.rs`, in order: `fetch`, then
/// `bridge` (`OpenClaw` B2, #5443), each split on the live feature.
fn registry_handlers() -> Vec<String> {
    vec![
        format!("#[cfg(feature = \"{LIVE_FEATURE}\")]\nhandler: fetch_command,"),
        format!(
            "#[cfg(not(feature = \"{LIVE_FEATURE}\"))]\nhandler: self::no_payment::fetch_command,"
        ),
        format!("#[cfg(feature = \"{LIVE_FEATURE}\")]\nhandler: self::bridge::live_command,"),
        format!(
            "#[cfg(not(feature = \"{LIVE_FEATURE}\"))]\nhandler: self::bridge::no_payment::bridge_command,"
        ),
    ]
}

// ------------------------------- the bridge route (OpenClaw B2, #5443)
//
// `toolkit agents-pay bridge` is the second command of the payment table.
// It reads its stdin request in every build (design 4.8, the G1c
// refinement), so its default-build route is not "print a literal": it is
// the pinned file module `bridge/no_payment.rs`, which hands the process
// stdin to the pure pipe `bridge::serve` with the pinned D32 refusal as the
// only service. `serve` answers `sanitize_output` without a world and gives
// every valid service request to that refusal. The same structural rules as
// `fetch` apply: the whole file is pinned token for token, its declaration
// is pinned, its registry entry is bound to the compiled `SLICE` literal,
// the names it resolves are bound once in `bridge/mod.rs`, and the D32
// frame is one byte-string literal. The live entry `live_command` is pinned
// too, so the process world is built only inside its service closure,
// after the pure route ([`bridge_world_findings`]).

const BRIDGE_FILE: &str = "agents_pay/bridge/mod.rs";
const BRIDGE_NO_PAYMENT_FILE: &str = "agents_pay/bridge/no_payment.rs";

/// The whole of `bridge/no_payment.rs`, compared token for token.
const BRIDGE_NO_PAYMENT_PIN: &str = r"
pub(in crate::agents_pay) fn bridge_command(
    _invocation: &crate::Invocation,
    out: &mut crate::Output<'_>,
) -> ::core::result::Result<(), ::toolkit_core::ToolkitError> {
    super::serve(
        &mut super::process_stdin(),
        out,
        &self::refuse_without_payment_support,
    )
}

fn refuse_without_payment_support(_request: &::toolkit_core::Json) -> (::std::vec::Vec<u8>, i32) {
    (::std::vec::Vec::from(super::UNAVAILABLE_FRAME), 1)
}
";

/// The live entry of the bridge in `bridge/mod.rs`, without its attributes:
/// the process world is built only inside the service closure, so the pure
/// `sanitize_output` route returns before it.
const BRIDGE_LIVE_PIN: &str = r"
pub(in crate::agents_pay) fn live_command(
    _invocation: &crate::Invocation,
    out: &mut crate::Output<'_>,
) -> ::core::result::Result<(), ::toolkit_core::ToolkitError> {
    self::serve(
        &mut self::process_stdin(),
        out,
        &|request| match super::deps::Deps::for_process() {
            ::core::result::Result::Ok(world) => self::handle_service(&world.deps(), request),
            ::core::result::Result::Err(_) => {
                self::render::error_frame(self::protocol::BridgeError::AgentCore)
            }
        },
    )
}
";

/// The bridge's unbuffered stdin in `bridge/mod.rs` (R-BR-IN: at most
/// 131073 bytes leave the pipe), without its attributes.
const BRIDGE_STDIN_PIN: &str = r"
pub(in crate::agents_pay) fn process_stdin() -> ::std::boxed::Box<dyn ::std::io::Read> {
    #[cfg(unix)]
    let owned = ::std::os::fd::AsFd::as_fd(&::std::io::stdin()).try_clone_to_owned();
    #[cfg(windows)]
    let owned =
        ::std::os::windows::io::AsHandle::as_handle(&::std::io::stdin()).try_clone_to_owned();
    match owned {
        ::core::result::Result::Ok(owned) => ::std::boxed::Box::new(::std::fs::File::from(owned)),
        ::core::result::Result::Err(_) => ::std::boxed::Box::new(::std::io::empty()),
    }
}
";

/// The rendered R-BR-D32 frame.
const BRIDGE_UNAVAILABLE: &[u8] = b"{\"ok\":false,\"error\":\"payment support unavailable\"}\n";

/// The bridge's `no_payment` declaration, with no other attribute.
fn bridge_no_payment_decl() -> String {
    format!("#[cfg(not(feature = \"{LIVE_FEATURE}\"))]\npub(super) mod no_payment;")
}

/// What may bind a name the pinned bridge file resolves: one item each.
const BRIDGE_SURFACE_ITEMS: &[(&str, &str)] = &[
    ("no_payment", "mod"),
    ("serve", "fn"),
    ("process_stdin", "fn"),
    ("UNAVAILABLE_FRAME", "const"),
];

/// The value of `const UNAVAILABLE_FRAME: &[u8] = b"..";` when it is exactly
/// a byte-string literal of type `&[u8]` with no attribute but its docs.
fn unavailable_literal(file: &syn::File) -> Option<Vec<u8>> {
    let consts: Vec<&syn::ItemConst> = file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Const(c) if c.ident == "UNAVAILABLE_FRAME" => Some(c),
            _ => None,
        })
        .collect();
    let [item] = consts.as_slice() else {
        return None;
    };
    let is_bytes = matches!(&*item.ty, Type::Reference(r)
        if r.lifetime.is_none() && r.mutability.is_none()
            && matches!(&*r.elem, Type::Slice(s)
                if matches!(&*s.elem, Type::Path(p) if p.qself.is_none() && p.path.is_ident("u8"))));
    let docs_only = item.attrs.iter().all(|a| a.path().is_ident("doc"));
    match &*item.expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::ByteStr(lit),
            attrs,
        }) if is_bytes && docs_only && attrs.is_empty() => Some(lit.value()),
        _ => None,
    }
}

/// The pinned tokens of one file, or a finding naming the first difference.
fn pinned_file_findings(sources: &[Source], path: &str, pin: &str) -> Vec<String> {
    let text = sources.iter().find(|s| s.path == path).map(|s| &s.text);
    match text.map(String::as_str).map(tokens) {
        None => vec![format!("{path}: is missing")],
        Some(Err(_)) => vec![format!("{path}: does not tokenize")],
        Some(Ok(stream)) => {
            let mut actual = Vec::new();
            token_list(stream, &mut actual);
            let pinned = list_of_text(pin);
            if actual == pinned {
                return Vec::new();
            }
            let at = actual
                .iter()
                .zip(&pinned)
                .position(|(a, p)| a != p)
                .unwrap_or(actual.len().min(pinned.len()));
            vec![format!(
                "{path}: differs from its pin at token {at}: `{}` where the pin has `{}`",
                actual[at.min(actual.len())..]
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" "),
                pinned[at.min(pinned.len())..]
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ")
            )]
        }
    }
}

/// The bridge's default-build route and live entry (see the section note).
fn bridge_route_findings(sources: &[Source]) -> Vec<String> {
    let mut found = pinned_file_findings(sources, BRIDGE_NO_PAYMENT_FILE, BRIDGE_NO_PAYMENT_PIN);
    let Some(text) = sources
        .iter()
        .find(|s| s.path == BRIDGE_FILE)
        .map(|s| s.text.as_str())
    else {
        found.push(format!("{BRIDGE_FILE}: is missing"));
        return found;
    };
    let (Ok(stream), Ok(file)) = (tokens(text), parse_source(text)) else {
        found.push(format!("{BRIDGE_FILE}: does not parse"));
        return found;
    };
    let trees: Vec<TokenTree> = stream.into_iter().collect();
    let decls: Vec<Vec<String>> = trees
        .windows(2)
        .enumerate()
        .filter(|(_, w)| {
            matches!((&w[0], &w[1]), (TokenTree::Ident(m), TokenTree::Ident(n)) if m == "mod" && n == "no_payment")
        })
        .map(|(at, _)| {
            // Step back over a `pub(..)` visibility, then the attributes.
            let mut start = at;
            if start >= 2
                && matches!(&trees[start - 1], TokenTree::Group(g) if g.delimiter() == proc_macro2::Delimiter::Parenthesis)
                && matches!(&trees[start - 2], TokenTree::Ident(p) if p == "pub")
            {
                start -= 2;
            }
            list_of_trees(&trees[attr_start(&trees, start)..(at + 3).min(trees.len())])
        })
        .collect();
    if decls != [list_of_text(&bridge_no_payment_decl())] {
        found.push(format!(
            "{BRIDGE_FILE}: the `no_payment` module must be declared once, exactly `{}`; found {decls:?}",
            bridge_no_payment_decl()
        ));
    }
    found.extend(surface_bindings(&file, BRIDGE_FILE, BRIDGE_SURFACE_ITEMS));
    if unavailable_literal(&file).as_deref() != Some(BRIDGE_UNAVAILABLE) {
        found.push(format!(
            "{BRIDGE_FILE}: `UNAVAILABLE_FRAME` is not one `const UNAVAILABLE_FRAME: &[u8] = b\"..\";` literal of the R-BR-D32 frame"
        ));
    }
    let live_cfg = [format!("cfg(feature=\"{LIVE_FEATURE}\")")];
    if !pinned_fn(&trees, "live_command", &live_cfg, BRIDGE_LIVE_PIN) {
        found.push(format!(
            "{BRIDGE_FILE}: `live_command` must be one `#[cfg(feature = \"{LIVE_FEATURE}\")]` fn, exactly the pinned live entry"
        ));
    }
    if !pinned_fn(
        &trees,
        "process_stdin",
        &["must_use".to_owned()],
        BRIDGE_STDIN_PIN,
    ) {
        found.push(format!(
            "{BRIDGE_FILE}: `process_stdin` must be one `#[must_use]` fn, exactly the pinned unbuffered stdin"
        ));
    }
    found
}

/// Whether `trees` define `fn name` exactly once at the top level, with
/// exactly the non-doc attributes `attrs`, and the tokens of `pin` from its
/// visibility to the end of its body.
fn pinned_fn(trees: &[TokenTree], name: &str, attrs: &[String], pin: &str) -> bool {
    let found: Vec<usize> = trees
        .windows(2)
        .enumerate()
        .filter(|(_, w)| {
            matches!((&w[0], &w[1]), (TokenTree::Ident(f), TokenTree::Ident(n)) if f == "fn" && n == name)
        })
        .map(|(at, _)| at)
        .collect();
    let [at] = found.as_slice() else {
        return false;
    };
    let mut start = *at;
    if start >= 2
        && matches!(&trees[start - 1], TokenTree::Group(g) if g.delimiter() == proc_macro2::Delimiter::Parenthesis)
        && matches!(&trees[start - 2], TokenTree::Ident(p) if p == "pub")
    {
        start -= 2;
    }
    let actual: Vec<String> = trees[attr_start(trees, start)..start]
        .iter()
        .filter_map(|t| match t {
            TokenTree::Group(g) => Some(compact(&g.stream())),
            _ => None,
        })
        .filter(|attr| !attr.starts_with("doc="))
        .collect();
    let end = trees[*at..]
        .iter()
        .position(
            |t| matches!(t, TokenTree::Group(g) if g.delimiter() == proc_macro2::Delimiter::Brace),
        )
        .map_or(trees.len(), |p| at + p + 1);
    actual == attrs && list_of_trees(&trees[start..end]) == list_of_text(pin)
}

/// What may bind a name at the top of `mod.rs`: `no_payment` and
/// `D32_REFUSAL` once each, by their own items; a prelude name never.
const SURFACE_ITEMS: &[(&str, &str)] = &[("no_payment", "mod"), ("D32_REFUSAL", "const")];
const PRELUDE_NAMES: &[&str] = &["Ok", "Err", "Some", "None", "Result", "Option"];

/// Attributes that run or export code outside a call (`.init_array`), or
/// import macros by another route.
const HIDDEN_ENTRY_ATTRS: &[&str] = &[
    "link_section",
    "used",
    "no_mangle",
    "export_name",
    "macro_use",
    "macro_export",
];

/// Dependency names the crate manifest may not bind: the absolute paths of
/// `no_payment.rs` resolve `::core` and `::toolkit_core` through them.
const ROOT_CRATES: &[&str] = &["core", "std", "alloc"];

/// The whitespace-free spelling of a token stream.
fn compact(stream: &TokenStream) -> String {
    stream
        .to_string()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// A token stream as a flat list that keeps every token boundary, each
/// punctuation's spacing and every group delimiter, so two lists are equal
/// only for the same tokens. Identifiers are as [`tokens`] left them.
fn token_list(stream: TokenStream, out: &mut Vec<String>) {
    for tree in stream {
        match tree {
            TokenTree::Group(group) => {
                let (open, close) = match group.delimiter() {
                    proc_macro2::Delimiter::Parenthesis => ("(", ")"),
                    proc_macro2::Delimiter::Brace => ("{", "}"),
                    proc_macro2::Delimiter::Bracket => ("[", "]"),
                    proc_macro2::Delimiter::None => ("<none", "none>"),
                };
                out.push(open.to_owned());
                token_list(group.stream(), out);
                out.push(close.to_owned());
            }
            TokenTree::Ident(ident) => out.push(ident.to_string()),
            TokenTree::Punct(punct) => out.push(match punct.spacing() {
                proc_macro2::Spacing::Joint => format!("{}+", punct.as_char()),
                proc_macro2::Spacing::Alone => punct.as_char().to_string(),
            }),
            TokenTree::Literal(literal) => out.push(literal.to_string()),
        }
    }
}

fn list_of_trees(trees: &[TokenTree]) -> Vec<String> {
    let mut out = Vec::new();
    token_list(trees.iter().cloned().collect(), &mut out);
    out
}

fn list_of_text(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    token_list(tokens(text).expect("a pinned text tokenizes"), &mut out);
    out
}

/// Where the outer attributes right before `trees[at]` start.
fn attr_start(trees: &[TokenTree], at: usize) -> usize {
    let mut start = at;
    while start >= 2 {
        let (TokenTree::Punct(hash), TokenTree::Group(group)) =
            (&trees[start - 2], &trees[start - 1])
        else {
            break;
        };
        if hash.as_char() != '#' || group.delimiter() != proc_macro2::Delimiter::Bracket {
            break;
        }
        start -= 2;
    }
    start
}

/// Every `handler: ..,` struct field, at any depth, with its attributes.
fn handler_fields(trees: &[TokenTree], out: &mut Vec<Vec<String>>) {
    for (index, tree) in trees.iter().enumerate() {
        if let TokenTree::Group(group) = tree {
            let inner: Vec<TokenTree> = group.stream().into_iter().collect();
            handler_fields(&inner, out);
        }
        let is_field = matches!(tree, TokenTree::Ident(i) if i == "handler")
            && matches!(trees.get(index + 1), Some(TokenTree::Punct(p))
                if p.as_char() == ':' && p.spacing() == proc_macro2::Spacing::Alone);
        if !is_field {
            continue;
        }
        let end = trees[index..]
            .iter()
            .position(|t| matches!(t, TokenTree::Punct(p) if p.as_char() == ','))
            .map_or(trees.len(), |at| index + at + 1);
        out.push(list_of_trees(&trees[attr_start(trees, index)..end]));
    }
}

/// The leaf bindings of a `use` tree: (bound name, full path), or a glob.
fn use_leaves(prefix: &str, tree: &syn::UseTree, out: &mut Vec<(String, String)>) {
    let join = |name: &str| {
        if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}::{name}")
        }
    };
    match tree {
        syn::UseTree::Path(path) => use_leaves(&join(&path.ident.to_string()), &path.tree, out),
        syn::UseTree::Name(name) => {
            out.push((name.ident.to_string(), join(&name.ident.to_string())));
        }
        syn::UseTree::Rename(rename) => {
            out.push((rename.rename.to_string(), join(&rename.ident.to_string())));
        }
        syn::UseTree::Glob(_) => out.push(("*".to_owned(), join("*"))),
        syn::UseTree::Group(group) => {
            for item in &group.items {
                use_leaves(prefix, item, out);
            }
        }
    }
}

fn item_binding(item: &syn::Item) -> Option<(String, &'static str)> {
    let (ident, kind) = match item {
        syn::Item::Fn(i) => (&i.sig.ident, "fn"),
        syn::Item::Const(i) => (&i.ident, "const"),
        syn::Item::Static(i) => (&i.ident, "static"),
        syn::Item::Struct(i) => (&i.ident, "struct"),
        syn::Item::Enum(i) => (&i.ident, "enum"),
        syn::Item::Union(i) => (&i.ident, "union"),
        syn::Item::Type(i) => (&i.ident, "type"),
        syn::Item::Trait(i) => (&i.ident, "trait"),
        syn::Item::TraitAlias(i) => (&i.ident, "trait"),
        syn::Item::Mod(i) => (&i.ident, "mod"),
        syn::Item::ExternCrate(i) => (&i.ident, "extern crate"),
        _ => return None,
    };
    Some((ident.to_string(), kind))
}

/// What may bind a surface or prelude name at the top of `mod.rs`.
fn binding_findings(file: &syn::File) -> Vec<String> {
    surface_bindings(file, ROUTE_FILE, SURFACE_ITEMS)
}

/// What may bind one of `surface` or a prelude name at the top of `path`.
fn surface_bindings(
    file: &syn::File,
    file_path: &str,
    surface: &[(&str, &'static str)],
) -> Vec<String> {
    let mut found = Vec::new();
    let mut items: Vec<(String, &'static str)> = Vec::new();
    let mut imports: Vec<(String, String)> = Vec::new();
    for item in &file.items {
        match item {
            syn::Item::Use(item) => use_leaves("", &item.tree, &mut imports),
            syn::Item::Macro(item) => found.push(format!(
                "{file_path}: a top-level macro item `{}!` can define any name",
                path_text(&item.mac.path)
            )),
            other => items.extend(item_binding(other)),
        }
    }
    for (name, import) in &imports {
        if name == "*" {
            found.push(format!(
                "{file_path}: glob import `{import}` can shadow a surface name"
            ));
        }
    }
    let bound = |name: &str| {
        items.iter().filter(|(n, _)| n == name).count()
            + imports.iter().filter(|(n, _)| n == name).count()
    };
    for (name, kind) in surface {
        let of_kind = items.iter().filter(|(n, k)| n == name && k == kind).count();
        if of_kind != 1 || bound(name) != 1 {
            found.push(format!(
                "{file_path}: `{name}` must be bound by exactly one top-level {kind} item, found {} binding(s)",
                bound(name)
            ));
        }
    }
    for name in PRELUDE_NAMES {
        if bound(name) != 0 {
            found.push(format!("{file_path}: `{name}` is shadowed"));
        }
    }
    found
}

/// The default-build route: `no_payment.rs` is exactly the pinned file,
/// `mod.rs` declares it exactly once as pinned and dispatches to it through
/// exactly the pinned registry entries, and `D32_REFUSAL` is one literal.
#[allow(clippy::too_many_lines)] // One pass over the pinned file, the declaration and the entries.
fn d32_findings(sources: &[Source]) -> Vec<String> {
    let text = |path: &str| sources.iter().find(|s| s.path == path).map(|s| &s.text);
    let mut found = Vec::new();

    // The file module, whole.
    match text(NO_PAYMENT_FILE).map(String::as_str).map(tokens) {
        None => found.push(format!("{NO_PAYMENT_FILE}: is missing")),
        Some(Err(_)) => found.push(format!("{NO_PAYMENT_FILE}: does not tokenize")),
        Some(Ok(stream)) => {
            let mut actual = Vec::new();
            token_list(stream, &mut actual);
            let pinned = list_of_text(NO_PAYMENT_PIN);
            if actual != pinned {
                let at = actual
                    .iter()
                    .zip(&pinned)
                    .position(|(a, p)| a != p)
                    .unwrap_or(actual.len().min(pinned.len()));
                let near = |list: &[String]| {
                    list[at.min(list.len())..]
                        .iter()
                        .take(8)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                found.push(format!(
                    "{NO_PAYMENT_FILE}: differs from the pinned D32 route at token {at}: `{}` where the pin has `{}`",
                    near(&actual),
                    near(&pinned)
                ));
            }
        }
    }

    // `mod.rs`: the declaration, the registry entries, the bindings and the
    // literal.
    let Some(route) = text(ROUTE_FILE) else {
        found.push(format!("{ROUTE_FILE}: is missing"));
        return found;
    };
    let Ok(stream) = tokens(route) else {
        found.push(format!("{ROUTE_FILE}: does not tokenize"));
        return found;
    };
    let Ok(file) = parse_source(route) else {
        found.push(format!("{ROUTE_FILE}: does not parse"));
        return found;
    };
    let trees: Vec<TokenTree> = stream.into_iter().collect();
    let decls: Vec<Vec<String>> = trees
        .windows(2)
        .enumerate()
        .filter(|(_, w)| {
            matches!((&w[0], &w[1]), (TokenTree::Ident(m), TokenTree::Ident(n)) if m == "mod" && n == "no_payment")
        })
        .map(|(at, _)| list_of_trees(&trees[attr_start(&trees, at)..(at + 3).min(trees.len())]))
        .collect();
    if decls != [list_of_text(&no_payment_decl())] {
        found.push(format!(
            "{ROUTE_FILE}: the `no_payment` module must be declared once, exactly `{}`; found {decls:?}",
            no_payment_decl()
        ));
    }
    let mut handlers = Vec::new();
    handler_fields(&trees, &mut handlers);
    let expected: Vec<Vec<String>> = registry_handlers()
        .iter()
        .map(String::as_str)
        .map(list_of_text)
        .collect();
    if handlers != expected {
        found.push(format!(
            "{ROUTE_FILE}: the registry entries must be exactly {:?}; found {handlers:?}",
            registry_handlers()
        ));
    }
    found.extend(binding_findings(&file));
    if d32_literal(&file).is_none() {
        found.push(format!(
            "{ROUTE_FILE}: `D32_REFUSAL` is not one `const D32_REFUSAL: &str = \"..\";` string literal"
        ));
    }
    found
}

/// The value of `const D32_REFUSAL: &str = "..";` when it is exactly a
/// string literal of type `&str` with no attribute but its docs.
fn d32_literal(file: &syn::File) -> Option<String> {
    let consts: Vec<&syn::ItemConst> = file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Const(c) if c.ident == "D32_REFUSAL" => Some(c),
            _ => None,
        })
        .collect();
    let [item] = consts.as_slice() else {
        return None;
    };
    let is_str = matches!(&*item.ty, Type::Reference(r)
        if r.lifetime.is_none() && r.mutability.is_none()
            && matches!(&*r.elem, Type::Path(p) if p.qself.is_none() && p.path.is_ident("str")));
    let docs_only = item.attrs.iter().all(|a| a.path().is_ident("doc"));
    match &*item.expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(lit),
            attrs,
        }) if is_str && docs_only && attrs.is_empty() => Some(lit.value()),
        _ => None,
    }
}

/// Renames, used by the `src/agents_pay/` scan.
struct Renames(Vec<String>);
impl<'ast> Visit<'ast> for Renames {
    fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
        if rename.rename != "_" {
            self.0
                .push(format!("{} as {}", rename.ident, rename.rename));
        }
        visit::visit_use_rename(self, rename);
    }
}

fn hidden_definitions(path: &str, stream: TokenStream, found: &mut Vec<String>) {
    let trees: Vec<TokenTree> = stream.into_iter().collect();
    for (index, tree) in trees.iter().enumerate() {
        let next = trees.get(index + 1);
        match tree {
            TokenTree::Ident(i) if i == "macro_rules" => {
                if matches!(next, Some(TokenTree::Punct(p)) if p.as_char() == '!') {
                    found.push(format!("{path}: defines a macro_rules! macro"));
                }
            }
            TokenTree::Ident(i) if i == "extern" => {
                if matches!(next, Some(TokenTree::Ident(n)) if n == "crate") {
                    found.push(format!("{path}: has an extern crate item"));
                }
            }
            TokenTree::Group(group) => {
                let is_attr = group.delimiter() == proc_macro2::Delimiter::Bracket
                    && matches!(index.checked_sub(1).map(|i| &trees[i]), Some(TokenTree::Punct(p)) if p.as_char() == '#');
                if is_attr {
                    let mut words = Vec::new();
                    idents(group.stream(), &mut words);
                    for word in words {
                        if HIDDEN_ENTRY_ATTRS.contains(&word.as_str()) {
                            found.push(format!("{path}: has a #[{word}] attribute"));
                        }
                    }
                }
                hidden_definitions(path, group.stream(), found);
            }
            _ => {}
        }
    }
}

/// Every raw identifier (`r#name`) in the source as written: the one scan
/// that reads the tokens before [`tokens`] unraws them.
fn raw_identifiers(stream: TokenStream, out: &mut Vec<String>) {
    for tree in stream {
        match tree {
            TokenTree::Group(group) => raw_identifiers(group.stream(), out),
            TokenTree::Ident(ident) if ident.to_string().starts_with("r#") => {
                out.push(ident.to_string());
            }
            _ => {}
        }
    }
}

/// Every source under `src/agents_pay/`: no raw identifier (a raw spelling
/// names the same item as the plain one, #5400), no `macro_rules!`
/// definition, no `use .. as` rename (so no aliased std/core macro), no
/// `extern crate` and no attribute that runs or exports code outside a call.
fn pay_dir_findings(sources: &[Source]) -> Vec<String> {
    let mut found = Vec::new();
    for source in sources {
        if !source.path.starts_with("agents_pay/") {
            continue;
        }
        let Ok(raw) = source.text.parse::<TokenStream>() else {
            found.push(format!("{}: does not tokenize", source.path));
            continue;
        };
        let mut raws = Vec::new();
        raw_identifiers(raw, &mut raws);
        for name in raws {
            found.push(format!(
                "{}: raw identifier `{name}` (it names the same item as `{}`)",
                source.path,
                &name[2..]
            ));
        }
        let Ok(stream) = tokens(&source.text) else {
            continue;
        };
        hidden_definitions(&source.path, stream, &mut found);
        match parse_source(&source.text) {
            Ok(file) => {
                let mut renames = Renames(Vec::new());
                renames.visit_file(&file);
                for rename in renames.0 {
                    found.push(format!("{}: renames an import: {rename}", source.path));
                }
            }
            Err(_) => found.push(format!("{}: does not parse", source.path)),
        }
    }
    found
}

/// The absolute paths of `no_payment.rs` resolve to the real crates: no
/// source of the crate has an `extern crate` item, and the crate manifest
/// binds no dependency under a root crate name (`core`, `std`, `alloc`)
/// and does not rename `toolkit_core`.
fn root_crate_findings(sources: &[Source], manifest: &str) -> Vec<String> {
    let mut found = Vec::new();
    for source in sources {
        let Ok(stream) = tokens(&source.text) else {
            found.push(format!("{}: does not tokenize", source.path));
            continue;
        };
        let mut words = Vec::new();
        idents(stream, &mut words);
        if words
            .windows(2)
            .any(|w| w[0] == "extern" && w[1] == "crate")
        {
            found.push(format!("{}: has an extern crate item", source.path));
        }
    }
    // Each dependency key: `NAME = ..` (or `NAME.workspace = ..`) under a
    // `*dependencies` table, or the NAME of a `[*dependencies.NAME]` table.
    // A root crate name is never bound, and `toolkit_core` is never a rename.
    let mut table = String::new();
    let mut dependency: Option<String> = None;
    for line in manifest.lines().map(toml_line) {
        let line = line.as_str();
        let key = if let Some(header) = line.strip_prefix('[') {
            header.trim_end_matches(']').clone_into(&mut table);
            dependency = table
                .rsplit_once('.')
                .filter(|(head, _)| head.ends_with("dependencies"))
                .map(|(_, name)| name.trim_matches('"').replace('-', "_"));
            match &dependency {
                Some(name) => name.clone(),
                None => continue,
            }
        } else if table.ends_with("dependencies") {
            let Some((key, _)) = line.split_once('=') else {
                continue;
            };
            let name = key.split('.').next().unwrap_or("");
            name.trim().trim_matches('"').replace('-', "_")
        } else if dependency.is_some() && line.starts_with("package") {
            dependency.clone().unwrap_or_default()
        } else {
            continue;
        };
        if ROOT_CRATES.contains(&key.as_str()) {
            found.push(format!(
                "Cargo.toml: a dependency binds the root crate name `{key}`"
            ));
        }
        let renamed = line.contains("package") && !line.starts_with('[');
        if key == "toolkit_core" && renamed {
            found.push(format!(
                "Cargo.toml: `toolkit_core` is a renamed package: {line}"
            ));
        }
    }
    found
}

/// A path with no leading `::` and no generic arguments, as text.
fn plain_path(path: &syn::Path) -> Option<String> {
    (path.leading_colon.is_none() && path.segments.iter().all(|s| s.arguments.is_none()))
        .then(|| path_text(path))
}

/// A `cfg(..)` attribute whose whitespace-free arguments are `args`.
fn is_cfg(attr: &syn::Attribute, args: &str) -> bool {
    matches!(attr.style, syn::AttrStyle::Outer)
        && matches!(&attr.meta, syn::Meta::List(list)
            if plain_path(&list.path).as_deref() == Some("cfg")
                && list.tokens.to_string().chars().filter(|c| !c.is_whitespace()).collect::<String>() == args)
}

/// `expr` is the struct literal `path { .. }`, every field named in full
/// (no shorthand) and none taken by `..` struct update (#5421).
fn full_literal<'a>(
    expr: &'a syn::Expr,
    path: &str,
    found: &mut Vec<String>,
) -> Option<&'a syn::ExprStruct> {
    let syn::Expr::Struct(literal) = expr else {
        found.push(format!("{ROUTE_FILE}: `{path}` is not a struct literal"));
        return None;
    };
    if literal.qself.is_some() || plain_path(&literal.path).as_deref() != Some(path) {
        found.push(format!(
            "{ROUTE_FILE}: a `{path}` literal names `{}`",
            path_text(&literal.path)
        ));
    }
    if !literal.attrs.is_empty() {
        found.push(format!(
            "{ROUTE_FILE}: the `{path}` literal has an attribute"
        ));
    }
    if literal.dot2_token.is_some() || literal.rest.is_some() {
        found.push(format!(
            "{ROUTE_FILE}: the `{path}` literal takes fields by `..` struct update"
        ));
    }
    for field in &literal.fields {
        if field.colon_token.is_none() {
            found.push(format!(
                "{ROUTE_FILE}: the `{path}` literal has a shorthand field"
            ));
        }
    }
    Some(literal)
}

fn field_name(field: &syn::FieldValue) -> String {
    match &field.member {
        syn::Member::Named(name) => name.to_string(),
        syn::Member::Unnamed(index) => index.index.to_string(),
    }
}

/// `&[a, b, ..]`, with no attribute: its elements.
fn array_ref(expr: &syn::Expr) -> Option<&Punctuated<syn::Expr, Token![,]>> {
    match expr {
        syn::Expr::Reference(r) if r.attrs.is_empty() && r.mutability.is_none() => match &*r.expr {
            syn::Expr::Array(array) if array.attrs.is_empty() => Some(&array.elems),
            _ => None,
        },
        _ => None,
    }
}

/// The registry pin is bound to what the registry compiles (#5421):
/// `crate::registry::SLICES` lists `crate::agents_pay::SLICE` (and no other
/// payment table); `mod.rs` has one top-level `const SLICE` with docs only,
/// a `crate::registry::Slice` literal whose `commands` is `&[..]` of one
/// `crate::CommandSpec` literal for `agents-pay fetch`; both literals name
/// every field in full, with no `..` struct update; and the only
/// attributes in either are the two pinned `cfg`s, one on each of the two
/// `handler` fields, which name the pinned handlers. Together with the
/// token pin of every `handler:` field in `mod.rs`, a pinned entry cannot
/// sit in dead code while another handler is compiled.
#[allow(clippy::too_many_lines)] // One walk down the registry table.
fn slice_findings(sources: &[Source]) -> Vec<String> {
    let text = |path: &str| {
        sources
            .iter()
            .find(|s| s.path == path)
            .map(|s| s.text.as_str())
    };
    let mut found = Vec::new();

    // The shared registry lists this slice, and no other payment table.
    let listed: Option<Vec<String>> = text("registry.rs")
        .and_then(|t| parse_source(t).ok())
        .and_then(|file| {
            file.items.into_iter().find_map(|item| match item {
                syn::Item::Const(c) if c.ident == "SLICES" => Some(c),
                _ => None,
            })
        })
        .and_then(|slices| {
            array_ref(&slices.expr).map(|elems| {
                elems
                    .iter()
                    .map(|e| match e {
                        syn::Expr::Path(p) if p.attrs.is_empty() && p.qself.is_none() => {
                            plain_path(&p.path).unwrap_or_default()
                        }
                        _ => "<not a plain path>".to_owned(),
                    })
                    .filter(|p| !p.starts_with("crate::") || p.starts_with("crate::agents_pay"))
                    .collect()
            })
        });
    let tables: Vec<String> = PAYMENT_TABLES.iter().map(|t| (*t).to_owned()).collect();
    if listed.as_deref() != Some(&tables[..]) {
        found.push(format!(
            "registry.rs: `SLICES` must list exactly the payment tables {PAYMENT_TABLES:?}, in that order; found {listed:?}"
        ));
    }

    let Some(Ok(file)) = text(ROUTE_FILE).map(parse_source) else {
        found.push(format!("{ROUTE_FILE}: does not parse"));
        return found;
    };
    let slices: Vec<&syn::ItemConst> = file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Const(c) if c.ident == "SLICE" => Some(c),
            _ => None,
        })
        .collect();
    let [slice] = slices.as_slice() else {
        found.push(format!(
            "{ROUTE_FILE}: needs exactly one top-level `const SLICE`, found {}",
            slices.len()
        ));
        return found;
    };
    if !slice.attrs.iter().all(|a| a.path().is_ident("doc")) {
        found.push(format!(
            "{ROUTE_FILE}: `SLICE` has an attribute other than docs"
        ));
    }
    let Some(table) = full_literal(&slice.expr, "crate::registry::Slice", &mut found) else {
        return found;
    };
    let mut commands = Vec::new();
    for field in &table.fields {
        if !field.attrs.is_empty() {
            found.push(format!(
                "{ROUTE_FILE}: the `SLICE` field `{}` has an attribute",
                field_name(field)
            ));
        }
        if field_name(field) == "commands" {
            commands.push(&field.expr);
        }
    }
    let specs = match commands.as_slice() {
        [expr] => array_ref(expr),
        _ => None,
    };
    let elems: Vec<&syn::Expr> = specs.map(|e| e.iter().collect()).unwrap_or_default();
    let [fetch, bridge] = elems.as_slice() else {
        found.push(format!(
            "{ROUTE_FILE}: `SLICE.commands` must be `&[crate::CommandSpec {{ .. }}, crate::CommandSpec {{ .. }}]`, one literal for `agents-pay fetch` and one for `agents-pay bridge`"
        ));
        return found;
    };
    let fetch_handlers = [
        (format!("feature=\"{LIVE_FEATURE}\""), "fetch_command"),
        (
            format!("not(feature=\"{LIVE_FEATURE}\")"),
            "self::no_payment::fetch_command",
        ),
    ];
    spec_findings(fetch, "fetch", &fetch_handlers, &[], &mut found);
    let bridge_handlers = [
        (
            format!("feature=\"{LIVE_FEATURE}\""),
            "self::bridge::live_command",
        ),
        (
            format!("not(feature=\"{LIVE_FEATURE}\")"),
            "self::bridge::no_payment::bridge_command",
        ),
    ];
    // R-BR-ROUTE: no flag, no value flag and no positional.
    let bridge_fields = [
        ("flags", "&[]"),
        ("value_flags", "&[]"),
        ("repeatable_flags", "&[]"),
        ("max_positionals", "0"),
        ("passthrough", "false"),
    ];
    spec_findings(
        bridge,
        "bridge",
        &bridge_handlers,
        &bridge_fields,
        &mut found,
    );
    found
}

/// One `crate::CommandSpec` literal of the payment table: every field named
/// in full, `path: &["agents-pay", word]`, no attribute but the two pinned
/// `cfg`s on the two pinned `handler` fields, and each of `fixed` exactly
/// as given (whitespace-free).
fn spec_findings(
    spec: &syn::Expr,
    word: &str,
    pinned: &[(String, &str)],
    fixed: &[(&str, &str)],
    found: &mut Vec<String>,
) {
    let Some(spec) = full_literal(spec, "crate::CommandSpec", found) else {
        return;
    };
    let mut handlers = Vec::new();
    let mut paths = Vec::new();
    for field in &spec.fields {
        match field_name(field).as_str() {
            "handler" => handlers.push(field),
            name => {
                if !field.attrs.is_empty() {
                    found.push(format!(
                        "{ROUTE_FILE}: the {word} `CommandSpec` field `{name}` has an attribute"
                    ));
                }
                if name == "path" {
                    paths.push(&field.expr);
                }
                if let Some((_, value)) = fixed.iter().find(|(key, _)| *key == name) {
                    if expr_text(&field.expr).as_deref() != Some(*value) {
                        found.push(format!(
                            "{ROUTE_FILE}: the {word} `CommandSpec` must have `{name}: {value}`"
                        ));
                    }
                }
            }
        }
    }
    for (name, value) in fixed {
        if spec
            .fields
            .iter()
            .filter(|f| field_name(f) == *name)
            .count()
            != 1
        {
            found.push(format!(
                "{ROUTE_FILE}: the {word} `CommandSpec` must have `{name}: {value}`"
            ));
        }
    }
    let path_words: Option<Vec<String>> = match paths.as_slice() {
        [expr] => array_ref(expr).and_then(|elems| {
            elems
                .iter()
                .map(|e| match e {
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        attrs,
                    }) if attrs.is_empty() => Some(s.value()),
                    _ => None,
                })
                .collect()
        }),
        _ => None,
    };
    if path_words.as_deref() != Some(&["agents-pay".to_owned(), word.to_owned()][..]) {
        found.push(format!(
            "{ROUTE_FILE}: the {word} `CommandSpec` must have `path: &[\"agents-pay\", \"{word}\"]`"
        ));
    }
    let bound = handlers.len() == pinned.len()
        && handlers.iter().zip(pinned).all(|(field, (cfg, handler))| {
            matches!(field.attrs.as_slice(), [attr] if is_cfg(attr, cfg))
                && matches!(&field.expr, syn::Expr::Path(p)
                    if p.attrs.is_empty() && p.qself.is_none()
                        && plain_path(&p.path).as_deref() == Some(*handler))
        });
    if !bound {
        found.push(format!(
            "{ROUTE_FILE}: the {word} `CommandSpec` must hold exactly the two pinned `handler` entries {:?}",
            registry_handlers()
        ));
    }
}

/// The text of a `&[]`, an integer or a boolean literal field value.
fn expr_text(expr: &syn::Expr) -> Option<String> {
    if let Some(elems) = array_ref(expr) {
        return elems.is_empty().then(|| "&[]".to_owned());
    }
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit, attrs }) if attrs.is_empty() => match lit {
            syn::Lit::Int(int) if int.suffix().is_empty() => Some(int.base10_digits().to_owned()),
            syn::Lit::Bool(flag) => Some(flag.value.to_string()),
            _ => None,
        },
        _ => None,
    }
}

// ------------------- R-BR-WORLD / T-OC-FINAL-ROUTE and T-BR-ONE-1..4
//
// The bridge's pure route (the pinned default entry, `serve`,
// `serve_frame`, `serve_quiet`, the protocol, render and scrub modules)
// names no world, no process world constructor and no payment step, so
// `sanitize_output` and every refusal are decided before `Deps` exists.
// The bridge reaches spending only through `flow::prove` (R-BR-ONE-1);
// `prove` is the only caller of `agentcore::process_payment` (R-BR-ONE-2);
// `live.rs` keeps its public items (R-BR-ONE-3); and
// `VerifiedProof::into_trusted_glue` has one caller, in `bridge/render.rs`
// (R-BR-ONE-4). The bridge's in-crate test files are fakes of those traits
// and are not production sources.

/// The bridge's production files that hold the pure route whole.
const BRIDGE_PURE_FILES: &[&str] = &[
    "agents_pay/bridge/no_payment.rs",
    "agents_pay/bridge/protocol.rs",
    "agents_pay/bridge/render.rs",
    "agents_pay/bridge/output_scrub.rs",
];

/// The pure-route functions of `bridge/mod.rs`.
const BRIDGE_PURE_FNS: &[&str] = &["serve", "serve_frame", "serve_quiet"];

/// Names the pure route never spells: the world, its constructors, the
/// payment steps and the process environment.
const WORLD_NAMES: &[&str] = &[
    "Deps",
    "for_process",
    "World",
    "live",
    "handle_service",
    "prove",
    "flow",
    "agentcore",
    "PaymentsConnector",
    "PaymentsApi",
    "ConfigStore",
    "MerchantHttp",
    "HostResolver",
    "PayClock",
    "env",
    "var",
    "var_os",
    "fs",
    "File",
    "OpenOptions",
    "net",
    "TcpStream",
    "UdpSocket",
    "Command",
    "home_dir",
];

/// Names no bridge production file spells (R-BR-ONE-1).
const SPEND_NAMES: &[&str] = &[
    "Approved",
    "ProcessPaymentRequest",
    "process_payment",
    "derive_client_token",
    "from_approved",
];

fn is_bridge_production(path: &str) -> bool {
    path.starts_with("agents_pay/bridge/") && !path.ends_with("tests.rs")
}

/// `stream` without its inline or declared `#[cfg(test)] mod NAME { .. }`
/// / `#[cfg(test)] mod NAME;` items: a production file's own unit tests
/// (fixture files, `env!`) are not its route.
fn without_test_modules(stream: TokenStream) -> TokenStream {
    let trees: Vec<TokenTree> = stream.into_iter().collect();
    let mut out = Vec::new();
    let mut at = 0;
    while at < trees.len() {
        let is_test_attr = matches!(&trees[at], TokenTree::Punct(p) if p.as_char() == '#')
            && matches!(trees.get(at + 1), Some(TokenTree::Group(g))
                if g.delimiter() == proc_macro2::Delimiter::Bracket && compact(&g.stream()) == "cfg(test)")
            && matches!(trees.get(at + 2), Some(TokenTree::Ident(m)) if m == "mod");
        if is_test_attr {
            let end = trees[at..]
                .iter()
                .position(|t| {
                    matches!(t, TokenTree::Group(g) if g.delimiter() == proc_macro2::Delimiter::Brace)
                        || matches!(t, TokenTree::Punct(p) if p.as_char() == ';')
                })
                .map_or(trees.len(), |p| at + p + 1);
            at = end;
            continue;
        }
        out.push(trees[at].clone());
        at += 1;
    }
    out.into_iter().collect()
}

fn bridge_world_findings(sources: &[Source]) -> Vec<String> {
    let mut found = Vec::new();
    for source in sources {
        if !is_bridge_production(&source.path) {
            continue;
        }
        let Ok(stream) = tokens(&source.text) else {
            found.push(format!("{}: does not tokenize", source.path));
            continue;
        };
        let stream = without_test_modules(stream);
        let mut words = Vec::new();
        idents(stream.clone(), &mut words);
        for word in &words {
            if SPEND_NAMES.contains(&word.as_str()) {
                found.push(format!(
                    "{}: names `{word}`; the bridge spends only through flow::prove",
                    source.path
                ));
            }
        }
        let mut pure = Vec::new();
        if BRIDGE_PURE_FILES.contains(&source.path.as_str()) {
            pure = words;
        } else if source.path == BRIDGE_FILE {
            for name in BRIDGE_PURE_FNS {
                fn_bodies(stream.clone(), name, &mut pure);
                if !words.windows(2).any(|w| w[0] == "fn" && w[1] == *name) {
                    found.push(format!(
                        "{BRIDGE_FILE}: the pure route `fn {name}` is missing"
                    ));
                }
            }
        }
        for word in pure {
            if WORLD_NAMES.contains(&word.as_str()) {
                found.push(format!(
                    "{}: the pure bridge route names `{word}`",
                    source.path
                ));
            }
        }
    }
    for path in BRIDGE_PURE_FILES {
        if !sources.iter().any(|s| s.path == *path) {
            found.push(format!("{path}: is missing"));
        }
    }
    // The handler reaches spending through `flow::prove`, once.
    let prove_calls = sources
        .iter()
        .filter(|s| s.path == BRIDGE_FILE)
        .filter_map(|s| tokens(&s.text).ok())
        .map(|stream| {
            let mut words = Vec::new();
            idents(stream, &mut words);
            words
                .windows(4)
                .filter(|w| w[0] == "flow" && w[1] == ":" && w[2] == ":" && w[3] == "prove")
                .count()
        })
        .sum::<usize>();
    if prove_calls != 1 {
        found.push(format!(
            "{BRIDGE_FILE}: must call `flow::prove` exactly once, found {prove_calls}"
        ));
    }
    found
}

/// R-BR-ONE-2: outside `agentcore.rs` (the definition, the fakes and the
/// client) and `live.rs` (the live client), `process_payment` is only a
/// trait method definition or the one path call in `flow.rs`'s `prove`.
fn one_path_findings(sources: &[Source]) -> Vec<String> {
    let mut found = Vec::new();
    let mut prove_calls = 0;
    for source in sources {
        if matches!(
            source.path.as_str(),
            "agents_pay/agentcore.rs" | "agents_pay/live.rs"
        ) {
            continue;
        }
        let Ok(stream) = tokens(&source.text) else {
            continue;
        };
        let mut words = Vec::new();
        idents(stream.clone(), &mut words);
        let uses = words
            .iter()
            .enumerate()
            .filter(|(at, w)| *w == "process_payment" && (*at == 0 || words[at - 1] != "fn"))
            .count();
        if uses == 0 {
            continue;
        }
        if source.path == "agents_pay/flow.rs" {
            let mut body = Vec::new();
            fn_bodies(stream, "prove", &mut body);
            let in_prove = body
                .windows(4)
                .filter(|w| {
                    w[0] == "agentcore" && w[1] == ":" && w[2] == ":" && w[3] == "process_payment"
                })
                .count();
            prove_calls += in_prove;
            if uses != in_prove {
                found.push(
                    "agents_pay/flow.rs: `process_payment` is used outside `fn prove`".to_owned(),
                );
            }
        } else {
            found.push(format!(
                "{}: uses `process_payment`; only flow::prove may",
                source.path
            ));
        }
    }
    if prove_calls != 1 {
        found.push(format!(
            "agents_pay/flow.rs: `fn prove` must call `agentcore::process_payment` exactly once, found {prove_calls}"
        ));
    }
    found
}

/// R-BR-ONE-3: the public items of `live.rs`, in order. The first three are
/// the fetch world; `LiveTerminal`, `AdminWorld` and its `for_process` are the
/// x402 S6 admin world (#5462, `admin::run_process`). The list stays exact, so
/// the bridge (B2) still adds none.
const LIVE_PUBLIC_ITEMS: &[&str] = &[
    "World",
    "for_process",
    "deps",
    "LiveTerminal",
    "AdminWorld",
    "for_process",
];

fn live_items_findings(sources: &[Source]) -> Vec<String> {
    struct Public(Vec<String>);
    impl<'ast> Visit<'ast> for Public {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            let named = match item {
                syn::Item::Fn(i) => Some((&i.vis, i.sig.ident.to_string())),
                syn::Item::Struct(i) => Some((&i.vis, i.ident.to_string())),
                syn::Item::Enum(i) => Some((&i.vis, i.ident.to_string())),
                syn::Item::Const(i) => Some((&i.vis, i.ident.to_string())),
                syn::Item::Static(i) => Some((&i.vis, i.ident.to_string())),
                syn::Item::Type(i) => Some((&i.vis, i.ident.to_string())),
                syn::Item::Trait(i) => Some((&i.vis, i.ident.to_string())),
                syn::Item::Mod(i) => Some((&i.vis, i.ident.to_string())),
                syn::Item::Use(i) => Some((&i.vis, "use".to_owned())),
                _ => None,
            };
            if let Some((vis, name)) = named {
                if !matches!(vis, Visibility::Inherited) {
                    self.0.push(name);
                }
            }
            visit::visit_item(self, item);
        }
        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            if !matches!(item.vis, Visibility::Inherited) {
                self.0.push(item.sig.ident.to_string());
            }
            visit::visit_impl_item_fn(self, item);
        }
    }
    let Some(Ok(file)) = sources
        .iter()
        .find(|s| s.path == "agents_pay/live.rs")
        .map(|s| parse_source(&s.text))
    else {
        return vec!["agents_pay/live.rs: is missing or does not parse".to_owned()];
    };
    let mut public = Public(Vec::new());
    public.visit_file(&file);
    if public.0 == LIVE_PUBLIC_ITEMS {
        Vec::new()
    } else {
        vec![format!(
            "agents_pay/live.rs: the public items must stay {LIVE_PUBLIC_ITEMS:?}; found {:?}",
            public.0
        )]
    }
}

/// R-BR-ONE-4: `into_trusted_glue` is defined in `proof.rs` and called once,
/// in `bridge/render.rs`.
fn glue_findings(sources: &[Source]) -> Vec<String> {
    let mut found = Vec::new();
    let (mut defined, mut called) = (0, 0);
    for source in sources {
        let Ok(stream) = tokens(&source.text) else {
            continue;
        };
        let mut words = Vec::new();
        idents(stream, &mut words);
        for (at, word) in words.iter().enumerate() {
            if word != "into_trusted_glue" {
                continue;
            }
            let prev = at.checked_sub(1).map(|i| words[i].as_str());
            match (prev, source.path.as_str()) {
                (Some("fn"), PROOF_FILE) => defined += 1,
                (Some("."), "agents_pay/bridge/render.rs") => called += 1,
                _ => found.push(format!(
                    "{}: names `into_trusted_glue`; its one caller is bridge/render.rs",
                    source.path
                )),
            }
        }
    }
    if (defined, called) != (1, 1) {
        found.push(format!(
            "into_trusted_glue must be defined once in proof.rs and called once in bridge/render.rs; found {defined} and {called}"
        ));
    }
    found
}

/// Code that runs before `main` or outside a call, anywhere in the crate
/// (#5423): `#[global_allocator]`, `global_asm!`, `asm!`, `naked_asm!` and
/// `#[naked]` / `#[unsafe(naked)]` are banned by name, after unraw. The
/// workspace `unsafe_code = "forbid"` lint rejects them too ([`lint_findings`]
/// pins it); this ban does not rest on it.
fn premain_findings(sources: &[Source]) -> Vec<String> {
    let mut found = Vec::new();
    for source in sources {
        let Ok(stream) = tokens(&source.text) else {
            found.push(format!("{}: does not tokenize", source.path));
            continue;
        };
        let mut words = Vec::new();
        idents(stream, &mut words);
        for (at, word) in words.iter().enumerate() {
            let banned = match word.as_str() {
                "global_allocator" | "global_asm" | "naked_asm" | "naked" => true,
                // `asm` is an ordinary name; the macro is `asm!` or `arch::asm`.
                "asm" => {
                    words.get(at + 1).is_some_and(|w| w == "!")
                        || (at >= 2 && words[at - 1] == ":" && words[at - 2] == ":")
                }
                _ => false,
            };
            if banned {
                found.push(format!(
                    "{}: names `{word}`, which runs code outside a call",
                    source.path
                ));
            }
        }
    }
    found
}

/// One manifest line with comments and the whitespace outside strings
/// removed, so `[dependencies . core]` reads `[dependencies.core]`.
fn toml_line(line: &str) -> String {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in line.chars() {
        match quote {
            Some(q) => {
                out.push(c);
                if escaped {
                    escaped = false;
                } else if c == '\\' && q == '"' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None if c == '#' => break,
            None if c.is_whitespace() => {}
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                }
                out.push(c);
            }
        }
    }
    out
}

/// The manifest as (table, normalised line) pairs; a header has its own
/// table name.
fn toml_lines(manifest: &str) -> Vec<(String, String)> {
    let mut table = String::new();
    let mut out = Vec::new();
    for line in manifest.lines().map(toml_line) {
        if line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            header
                .trim_start_matches('[')
                .trim_end_matches(']')
                .clone_into(&mut table);
        }
        out.push((table.clone(), line));
    }
    out
}

/// The unsafe lint is pinned (#5423): the crate manifest inherits the
/// workspace lints (`[lints]` holds exactly `workspace = true`, and no
/// other line names `lints`), and the workspace sets `unsafe_code =
/// "forbid"` in `[workspace.lints.rust]`, its only `unsafe_code` setting.
/// `forbid` cannot be lowered by an attribute in the source.
fn lint_findings(manifest: &str, workspace: &str) -> Vec<String> {
    let mut found = Vec::new();
    let lines = toml_lines(manifest);
    let headers = lines.iter().filter(|(_, l)| l == "[lints]").count();
    let in_lints: Vec<&str> = lines
        .iter()
        .filter(|(t, l)| t == "lints" && !l.starts_with('['))
        .map(|(_, l)| l.as_str())
        .collect();
    let elsewhere = lines.iter().any(|(t, l)| {
        l != "[lints]"
            && (t.starts_with("lints.") || l.trim_start_matches('[').starts_with("lints"))
    });
    if headers != 1 || in_lints != ["workspace=true"] || elsewhere {
        found.push(
            "Cargo.toml: the crate must inherit the workspace lints, exactly `[lints]` `workspace = true`"
                .to_owned(),
        );
    }
    let unsafe_lines: Vec<(String, String)> = toml_lines(workspace)
        .into_iter()
        .filter(|(_, l)| l.contains("unsafe_code"))
        .collect();
    if unsafe_lines
        != [(
            "workspace.lints.rust".to_owned(),
            "unsafe_code=\"forbid\"".to_owned(),
        )]
    {
        found.push(format!(
            "../../Cargo.toml: the workspace must set `unsafe_code = \"forbid\"` in `[workspace.lints.rust]`, and nowhere else; found {unsafe_lines:?}"
        ));
    }
    found
}

fn crate_manifest() -> String {
    fs::read_to_string(manifest_dir().join("Cargo.toml")).expect("the toolkit-cli manifest")
}

fn workspace_manifest() -> String {
    fs::read_to_string(repo_root().join("Cargo.toml")).expect("the workspace manifest")
}

/// The whole D32 gate over a source set and the real manifests.
fn route_gate(sources: &[Source]) -> Vec<String> {
    full_gate(sources, &crate_manifest(), &workspace_manifest())
}

/// The whole D32 gate over a source set, the crate manifest and the
/// workspace manifest.
fn full_gate(sources: &[Source], manifest: &str, workspace: &str) -> Vec<String> {
    d32_findings(sources)
        .into_iter()
        .chain(slice_findings(sources))
        .chain(admin_findings(sources))
        .chain(bridge_route_findings(sources))
        .chain(bridge_world_findings(sources))
        .chain(pay_dir_findings(sources))
        .chain(premain_findings(sources))
        .chain(root_crate_findings(sources, manifest))
        .chain(lint_findings(manifest, workspace))
        .collect()
}

fn real_source(path: &str) -> String {
    real_sources()
        .into_iter()
        .find(|source| source.path == path)
        .unwrap_or_else(|| panic!("{path} is missing"))
        .text
}

/// The real sources with `edit` applied at `anchor` in `path`; the anchor
/// must exist.
fn sources_with(path: &str, anchor: &str, edit: impl Fn(&str) -> String) -> Vec<Source> {
    let mut sources = real_sources();
    let at = sources
        .iter()
        .position(|s| s.path == path)
        .unwrap_or_else(|| panic!("{path} is missing"));
    assert!(
        sources[at].text.contains(anchor),
        "the anchor moved in {path}: {anchor}"
    );
    sources[at].text = sources[at].text.replacen(anchor, &edit(anchor), 1);
    sources
}

const HANDLER_ANCHOR: &str = "    self::refuse_without_payment_support(out)\n}";
const REFUSAL_ANCHOR: &str = "    out.line(super::D32_REFUSAL)?;\n";
const REFUSAL_FN_ANCHOR: &str = "fn refuse_without_payment_support(";
const NO_PAYMENT_START: &str = "pub(super) fn fetch_command(";
const PARSE_ANCHOR: &str = "fn parse(invocation: &Invocation";

#[test]
fn t_ac3_noread_the_disabled_route_reads_nothing_before_d32() {
    let sources = real_sources();
    let found = route_gate(&sources);
    assert!(found.is_empty(), "{found:#?}");
    // Not vacuous: the pin is the whole route, both functions included.
    let pinned = list_of_text(NO_PAYMENT_PIN);
    assert!(pinned.len() > 60, "{}", pinned.len());
    assert!(pinned.contains(&"refuse_without_payment_support".to_owned()));
    let file = parse_source(&real_source(ROUTE_FILE)).unwrap();
    assert_eq!(
        d32_literal(&file).as_deref(),
        Some(
            format!(
                "{{\"paid\": false, \"refused\": true, \"reason\": \"{}\"}}",
                toolkit_cli::agents_pay::deps::NO_PAYMENT_SUPPORT
            )
            .as_str()
        ),
        "D32_REFUSAL is not the rendered D32 document"
    );
}

#[test]
#[allow(clippy::too_many_lines)] // A table of mutations, each reported by label.
fn t_ac3_noread_catches_a_read_before_the_d32_refusal() {
    let disabled_entry = format!("#[cfg(not(feature = \"{LIVE_FEATURE}\"))]\n            handler:");
    let live_entry =
        format!("#[cfg(feature = \"{LIVE_FEATURE}\")]\n            handler: fetch_command,");
    let decl = no_payment_decl();
    let np = NO_PAYMENT_FILE;
    let mutations: Vec<(&str, Vec<Source>)> = vec![
        // The #5376 mutation, at each pre-D32 statement position.
        (
            "env read in the disabled handler",
            sources_with(np, HANDLER_ANCHOR, |a| {
                format!("    let _ = ::std::env::var_os(\"PAYMENT_SESSION_ID\");\n{a}")
            }),
        ),
        (
            "file read in the disabled handler",
            sources_with(np, HANDLER_ANCHOR, |a| {
                format!("    let _ = ::std::fs::read_to_string(\"/nonexistent\");\n{a}")
            }),
        ),
        (
            "env read in the D32 refusal",
            sources_with(np, REFUSAL_ANCHOR, |a| {
                format!("    let _ = ::std::env::var_os(\"AWS_PROFILE\");\n{a}")
            }),
        ),
        (
            "the disabled handler parses argv first",
            sources_with(np, HANDLER_ANCHOR, |a| {
                format!("    let _ = super::parse(_invocation, out)?;\n{a}")
            }),
        ),
        (
            "the disabled handler builds the process world",
            sources_with(np, HANDLER_ANCHOR, |a| {
                a.replace(
                    "self::refuse_without_payment_support(out)",
                    "{ let _ = super::deps::Deps::for_process(); self::refuse_without_payment_support(out) }",
                )
            }),
        ),
        (
            "the refusal text is read, not a literal",
            sources_with(np, REFUSAL_ANCHOR, |a| {
                a.replace(
                    "super::D32_REFUSAL",
                    "&::std::env::var(\"X\").unwrap_or_default()",
                )
            }),
        ),
        (
            "an extra attribute on the D32 refusal",
            sources_with(np, REFUSAL_FN_ANCHOR, |a| format!("#[inline(never)]\n{a}")),
        ),
        (
            "a doc comment in the pinned file",
            sources_with(np, REFUSAL_FN_ANCHOR, |a| format!("/// Refuses.\n{a}")),
        ),
        (
            "a generic D32 refusal",
            sources_with(np, REFUSAL_FN_ANCHOR, |_| {
                "fn refuse_without_payment_support<T>(".to_owned()
            }),
        ),
        (
            "a use in the pinned file",
            sources_with(np, NO_PAYMENT_START, |a| {
                format!("use ::core::result::Result;\n{a}")
            }),
        ),
        (
            "an inner attribute in the pinned file",
            sources_with(np, NO_PAYMENT_START, |a| format!("#![allow(unused)]\n{a}")),
        ),
        (
            "an extra item in the pinned file",
            sources_with(np, NO_PAYMENT_START, |a| format!("const _: () = ();\n{a}")),
        ),
        (
            "a macro invocation in the D32 refusal",
            sources_with(np, REFUSAL_ANCHOR, |a| {
                format!("    ::std::eprintln!(\"x\");\n{a}")
            }),
        ),
        (
            "a relative, shadowable Ok",
            sources_with(np, "    ::core::result::Result::Ok(())\n", |_| {
                "    Ok(())\n".to_owned()
            }),
        ),
        // `mod.rs`: the literal, the declaration and the registry entries.
        (
            "the D32 literal becomes an expression",
            sources_with(ROUTE_FILE, "pub const D32_REFUSAL: &str = ", |a| {
                format!("{a}env!(\"HOME\");\nconst UNUSED: &str = ")
            }),
        ),
        (
            "the default entry is no longer cfg(not(..))",
            sources_with(ROUTE_FILE, &disabled_entry, |a| a.replace("not(", "all(")),
        ),
        (
            "the registry dispatches elsewhere",
            sources_with(
                ROUTE_FILE,
                "handler: self::no_payment::fetch_command,",
                |_| "handler: run_command,".to_owned(),
            ),
        ),
        (
            "the default entry is a relative path",
            sources_with(
                ROUTE_FILE,
                "handler: self::no_payment::fetch_command,",
                |_| "handler: no_payment::fetch_command,".to_owned(),
            ),
        ),
        (
            "the live entry loses its cfg",
            sources_with(ROUTE_FILE, &live_entry, |_| {
                "handler: fetch_command,".to_owned()
            }),
        ),
        (
            "the module is read from another path",
            sources_with(ROUTE_FILE, &decl, |a| format!("#[path = \"flow.rs\"]\n{a}")),
        ),
        (
            "the module is inline, not the pinned file",
            sources_with(ROUTE_FILE, &decl, |a| {
                a.replace("mod no_payment;", "mod no_payment {}")
            }),
        ),
        (
            "the module is public",
            sources_with(ROUTE_FILE, &decl, |a| {
                a.replace("mod no_payment;", "pub mod no_payment;")
            }),
        ),
        (
            "the module declaration has a doc comment",
            sources_with(ROUTE_FILE, &decl, |a| format!("/// Docs.\n{a}")),
        ),
        (
            "the pinned file is missing",
            real_sources()
                .into_iter()
                .filter(|s| s.path != NO_PAYMENT_FILE)
                .collect(),
        ),
        (
            "mod.rs has no route at all",
            seeded(&[
                (ROUTE_FILE, "fn parse() {}"),
                (NO_PAYMENT_FILE, NO_PAYMENT_PIN),
            ]),
        ),
    ];
    let survived: Vec<&str> = mutations
        .iter()
        .filter(|(_, sources)| d32_findings(sources).is_empty())
        .map(|(label, _)| *label)
        .collect();
    assert!(survived.is_empty(), "survived the gate: {survived:#?}");
}

/// The re-review #5389 mutations, verbatim, and their variants at the D32
/// surface: each is killed by the committed gate.
#[test]
#[allow(clippy::too_many_lines)] // A table of mutations, each reported by label.
fn t_ac3_noread_catches_the_review_5389_helper_and_macro_mutations() {
    const HELPER: &str = "fn reason() -> Option<std::ffi::OsString> {\n    std::env::var_os(\"RSI_REVIEW_5389_NON_SECRET\")\n}\n\n";
    const SHADOW: &str = "macro_rules! matches {\n    ($($tokens:tt)*) => {{\n        let _ = std::env::var_os(\"RSI_REVIEW_5389_NON_SECRET\");\n        std::matches!($($tokens)*)\n    }};\n}\n\n";
    let killed = |sources: &[Source]| !route_gate(sources).is_empty();
    let mut survived: Vec<&str> = Vec::new();

    // #5390: the env-reading helper, called first on the default path.
    let np = NO_PAYMENT_FILE;
    let with_helper = |mut sources: Vec<Source>| {
        let at = sources.iter().position(|s| s.path == np).unwrap();
        sources[at].text.push('\n');
        sources[at].text.push_str(HELPER);
        sources
    };
    if !killed(&with_helper(sources_with(np, HANDLER_ANCHOR, |a| {
        format!("    let _ = reason();\n{a}")
    }))) {
        survived.push("#5390 helper in the disabled handler");
    }
    if !killed(&with_helper(sources_with(np, REFUSAL_ANCHOR, |a| {
        format!("    let _ = reason();\n{a}")
    }))) {
        survived.push("#5390 helper in the D32 refusal");
    }
    // ... or a name the route resolves, shadowed so the call site is
    // unchanged.
    for (label, path, anchor, hide) in [
        (
            "an imported helper with the D32 name",
            np,
            NO_PAYMENT_START,
            "mod hide {\n    pub fn refuse_without_payment_support(_: &mut crate::Output<'_>) -> Result<(), toolkit_core::ToolkitError> { let _ = std::env::var_os(\"X\"); Ok(()) }\n}\nuse self::hide::refuse_without_payment_support;\n",
        ),
        (
            "a glob import in the pinned file",
            np,
            NO_PAYMENT_START,
            "use super::*;\n",
        ),
        (
            "a shadowed Ok in the pinned file",
            np,
            NO_PAYMENT_START,
            "#[allow(non_snake_case)]\nfn Ok(_: ()) -> Result<(), toolkit_core::ToolkitError> { let _ = std::env::var_os(\"X\"); Result::Ok(()) }\n",
        ),
        (
            "an Output alias in the pinned file",
            np,
            NO_PAYMENT_START,
            "use crate::Invocation as Output;\n",
        ),
        (
            "a shadowed Ok in mod.rs",
            ROUTE_FILE,
            PARSE_ANCHOR,
            "#[allow(non_snake_case)]\nfn Ok(_: ()) -> Result<(), ToolkitError> { let _ = std::env::var_os(\"X\"); Result::Ok(()) }\n",
        ),
        (
            "a shadowed Result in mod.rs",
            ROUTE_FILE,
            PARSE_ANCHOR,
            "type Result<T, E> = std::result::Result<T, E>;\n",
        ),
        (
            "a glob import in mod.rs",
            ROUTE_FILE,
            PARSE_ANCHOR,
            "mod hide {}\nuse hide::*;\n",
        ),
        (
            "an item macro in mod.rs",
            ROUTE_FILE,
            PARSE_ANCHOR,
            "thread_local! { static X: u8 = 0; }\n",
        ),
    ] {
        if !killed(&sources_with(path, anchor, |a| format!("{hide}\n{a}"))) {
            survived.push(label);
        }
    }

    // #5391: the module-level `matches!` shadow, verbatim, in mod.rs and in
    // the pinned file.
    let shadowed = sources_with(ROUTE_FILE, PARSE_ANCHOR, |a| format!("{SHADOW}{a}"));
    if !pay_dir_findings(&shadowed)
        .iter()
        .any(|f| f.contains("macro_rules!"))
    {
        survived.push("#5391 macro shadow in mod.rs");
    }
    if !killed(&sources_with(np, NO_PAYMENT_START, |a| {
        format!("{SHADOW}{a}")
    })) {
        survived.push("#5391 macro shadow in the pinned file");
    }
    // The same shadow in any other payment source, or an aliased std macro.
    for (path, insert) in [
        ("agents_pay/flow.rs", SHADOW),
        ("agents_pay/deps.rs", "use std::matches as matches_any;\n"),
        ("agents_pay/net.rs", "use core::write as w;\n"),
        (
            "agents_pay/vault.rs",
            "#[macro_use]\nextern crate core as std_core;\n",
        ),
        (
            "agents_pay/proof.rs",
            "#[used]\n#[link_section = \".init_array\"]\nstatic INIT: fn() = || {};\n",
        ),
    ] {
        let mut sources = real_sources();
        let at = sources.iter().position(|s| s.path == path).unwrap();
        // After the inner `//!` docs, so the mutated source still parses.
        sources[at].text = format!("{}\n{insert}", sources[at].text);
        assert!(
            syn::parse_file(&sources[at].text).is_ok(),
            "the fixture does not parse: {insert}"
        );
        if pay_dir_findings(&sources).is_empty() {
            survived.push(insert);
        }
    }
    assert!(survived.is_empty(), "survived the gate: {survived:#?}");
}

/// The re-review #5397 mutation (#5400), verbatim, and the other raw
/// identifier forms: a raw spelling names the same item as the plain one,
/// so every gate compares identifiers after unraw. Each fixture is reported
/// by label when it survives.
#[test]
#[allow(clippy::too_many_lines)] // A table of mutations, each reported by label.
fn t_ac3_noread_catches_raw_identifier_mutations() {
    const RAW_OK: &str = "#[allow(non_snake_case)]\nfn r#Ok<T, E>(value: T) -> Result<T, E> {\n    let _ = std::env::var_os(\"RSI_REVIEW_5397_NON_SECRET\");\n    Result::Ok(value)\n}\n\n";
    const RAW_SHADOW: &str = "macro_rules! r#matches {\n    ($($tokens:tt)*) => {{\n        let _ = std::env::var_os(\"RSI_REVIEW_5397_NON_SECRET\");\n        std::matches!($($tokens)*)\n    }};\n}\n\n";
    let pay_with = |path: &str, insert: &str| -> Vec<Source> {
        let mut sources = real_sources();
        let at = sources.iter().position(|s| s.path == path).unwrap();
        sources[at].text = if path == ROUTE_FILE {
            sources[at]
                .text
                .replacen(PARSE_ANCHOR, &format!("{insert}{PARSE_ANCHOR}"), 1)
        } else {
            // After the inner `//!` docs: an item before them does not parse.
            format!("{}\n{insert}", sources[at].text)
        };
        assert!(
            syn::parse_file(&sources[at].text).is_ok(),
            "the fixture does not parse: {insert}"
        );
        sources
    };
    let route_killed = |sources: &[Source]| {
        !d32_findings(sources).is_empty() || !pay_dir_findings(sources).is_empty()
    };
    let mut survived: Vec<&str> = Vec::new();

    // The route: a raw shadow of a name the D32 surface resolves.
    for (label, path, insert) in [
        ("#5400 fn r#Ok in mod.rs", ROUTE_FILE, RAW_OK),
        (
            "type r#Result alias in mod.rs",
            ROUTE_FILE,
            "type r#Result<T, E> = ::core::result::Result<T, E>;\n\n",
        ),
        ("macro_rules! r#matches in mod.rs", ROUTE_FILE, RAW_SHADOW),
        (
            "r#thread_local! item macro in mod.rs",
            ROUTE_FILE,
            "r#thread_local! { static X: u8 = 0; }\n\n",
        ),
        ("macro_rules! r#matches in flow.rs", "agents_pay/flow.rs", RAW_SHADOW),
        (
            "raw macro path ::core::r#matches! in deps.rs",
            "agents_pay/deps.rs",
            "fn raw_macro() -> bool {\n    ::core::r#matches!(1, 1)\n}\n",
        ),
        (
            "#[r#used] #[r#link_section] static in proof.rs",
            "agents_pay/proof.rs",
            "#[r#used]\n#[r#link_section = \".init_array\"]\nstatic INIT: extern \"C\" fn() = init;\nextern \"C\" fn init() {}\n",
        ),
        (
            "#[r#cfg_attr(all(), r#used)] static in proof.rs",
            "agents_pay/proof.rs",
            "#[r#cfg_attr(all(), r#used)]\nstatic KEEP: u8 = 0;\n",
        ),
        (
            "#[r#no_mangle] fn in net.rs",
            "agents_pay/net.rs",
            "#[r#no_mangle]\npub extern \"C\" fn exported() {}\n",
        ),
        (
            "#[r#export_name] fn in vault.rs",
            "agents_pay/vault.rs",
            "#[r#export_name = \"exported\"]\npub extern \"C\" fn exported() {}\n",
        ),
        (
            "#[r#macro_use] extern crate in vault.rs",
            "agents_pay/vault.rs",
            "#[r#macro_use]\nextern crate core as std_core;\n",
        ),
        // The pinned file itself.
        ("#5400 fn r#Ok in no_payment.rs", NO_PAYMENT_FILE, RAW_OK),
        (
            "r#matches! in no_payment.rs",
            NO_PAYMENT_FILE,
            "fn raw_macro() -> bool {\n    ::core::r#matches!(1, 1)\n}\n",
        ),
    ] {
        if !route_killed(&pay_with(path, insert)) {
            survived.push(label);
        }
    }
    // Each layer on its own: the unraw-normalised binding check and the raw
    // identifier ban both see the #5400 and `r#Result` shadows in mod.rs.
    for (label, insert, name) in [
        ("#5400 binding check", RAW_OK, "`Ok` is shadowed"),
        (
            "r#Result binding check",
            "type r#Result<T, E> = ::core::result::Result<T, E>;\n\n",
            "`Result` is shadowed",
        ),
    ] {
        let sources = pay_with(ROUTE_FILE, insert);
        if !d32_findings(&sources).iter().any(|f| f.contains(name)) {
            survived.push(label);
        }
        if !pay_dir_findings(&sources)
            .iter()
            .any(|f| f.contains("raw identifier"))
        {
            survived.push("raw identifier ban in mod.rs");
        }
    }
    // A raw spelling of the pinned path is the same path: the pin, compared
    // after unraw, accepts it, and the raw identifier ban still reports it.
    let same = sources_with(NO_PAYMENT_FILE, "::core::result::Result::Ok(())", |_| {
        "::core::result::Result::r#Ok(())".to_owned()
    });
    assert!(d32_findings(&same).is_empty(), "the pin does not unraw");
    if pay_dir_findings(&same).is_empty() {
        survived.push("raw identifier ban in no_payment.rs");
    }

    // G4: a raw spelling of a live constructor in a payment test.
    let deps = real_source("agents_pay/deps.rs");
    let anchor = "    fn g3_test_identities_are_recognized() {\n";
    for (label, live) in [
        ("G4 Deps::r#for_process", "let _ = Deps::r#for_process();"),
        (
            "G4 r#SystemResolver",
            "let _ = toolkit_aws::r#SystemResolver;",
        ),
    ] {
        let bad = deps.replacen(anchor, &format!("{anchor}        {live}\n"), 1);
        let mut modules = Vec::new();
        inline_test_modules(tokens(&bad).unwrap(), &mut modules);
        let files: Vec<(String, String)> = modules
            .into_iter()
            .map(|m| ("agents_pay/deps.rs (inline tests)".to_owned(), m))
            .collect();
        if g4_unexpected(&files).is_empty() {
            survived.push(label);
        }
    }
    // The proof gate and R-TOK-NOFRESH.
    let flow_raw = [
        (
            "proof a.r#into_parts()",
            "fn f(a: Authorized) { let _ = a.r#into_parts(); }",
        ),
        (
            "proof r#VerifiedProof literal",
            "fn f(p: Json) -> VerifiedProof { r#VerifiedProof { payload: p } }",
        ),
    ];
    for (label, bad) in flow_raw {
        if proof_findings(&seeded(&[("agents_pay/flow.rs", bad)])).is_empty() {
            survived.push(label);
        }
    }
    let nofresh = "fn n() -> String { uuid::r#Uuid::new_v4().to_string() }";
    if nofresh_findings(&seeded(&[("agents_pay/flow.rs", nofresh)])).is_empty() {
        survived.push("nofresh uuid::r#Uuid");
    }
    // T-ORDER-CTOR: a raw constructor path outside `authorize`.
    for (label, flow) in [
        (
            "ctor Approved::r#new",
            "fn pay(token: String) { let _a = Approved::r#new(token); }",
        ),
        (
            "ctor r#Approved::new",
            "fn pay(token: String) { let _a = r#Approved::new(token); }",
        ),
    ] {
        let found = findings_with(&[("agents_pay/flow.rs", flow)], GOOD_APPROVED, GOOD_POLICY);
        if !found.iter().any(|f| f.contains("exactly one use")) {
            survived.push(label);
        }
    }
    assert!(survived.is_empty(), "survived the gate: {survived:#?}");
}

/// The absolute `::core` and `::toolkit_core` paths cannot be rebound from
/// outside `src/agents_pay/` either: no `extern crate` in the crate, and no
/// manifest dependency under a root crate's name.
#[test]
fn t_ac3_noread_absolute_paths_resolve_to_the_real_crates() {
    let sources = real_sources();
    let manifest = crate_manifest();
    assert!(manifest.contains("toolkit-core"), "the manifest moved");
    let found = root_crate_findings(&sources, &manifest);
    assert!(found.is_empty(), "{found:#?}");
    let mut survived: Vec<&str> = Vec::new();
    let mut lib = real_sources();
    let at = lib.iter().position(|s| s.path == "lib.rs").unwrap();
    lib[at].text = format!("extern crate std as core;\n{}", lib[at].text);
    if root_crate_findings(&lib, &manifest).is_empty() {
        survived.push("extern crate std as core in lib.rs");
    }
    for (label, edit) in [
        (
            "a dependency named core",
            manifest.replacen(
                "[dependencies]\n",
                "[dependencies]\ncore = { path = \"../evil\" }\n",
                1,
            ),
        ),
        (
            "toolkit_core renamed from another package",
            manifest.replacen(
                "[dependencies]\n",
                "[dependencies]\ntoolkit_core = { package = \"evil\", path = \"../evil\" }\n",
                1,
            ),
        ),
        (
            "a [dependencies.std] table",
            format!("{manifest}\n[dependencies.std]\npath = \"../evil\"\n"),
        ),
    ] {
        assert_ne!(edit, manifest, "{label}: the manifest anchor moved");
        if root_crate_findings(&sources, &edit).is_empty() {
            survived.push(label);
        }
    }
    assert!(survived.is_empty(), "survived the gate: {survived:#?}");
}

/// The re-review #5419 mutations (#5421, #5423), verbatim, and their
/// variants: each is a committed negative fixture, appended at a file end
/// (or replacing the registry entries) so the mutated file still parses,
/// and each is reported by label when it survives [`full_gate`].
#[test]
#[allow(clippy::too_many_lines)] // A table of mutations, each reported by label.
fn t_ac3_noread_catches_the_review_5419_mutations() {
    let entries = format!(
        "            #[cfg(feature = \"{LIVE_FEATURE}\")]\n            handler: fetch_command,\n            #[cfg(not(feature = \"{LIVE_FEATURE}\"))]\n            handler: self::no_payment::fetch_command,\n"
    );
    // The pinned entries, kept in a never-compiled item.
    let dead_pin = format!(
        "\n#[cfg(any())]\nconst _REVIEW_5419_PINNED: () = crate::CommandSpec {{\n{entries}}};\n"
    );
    let env_handler = "fn handler(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {\n    if std::env::var_os(\"PAYMENT_MANAGER_ARN\").is_some() { eprint!(\"RSI_REVIEW_5419_ENV_READ_BEFORE_D32\"); }\n    self::no_payment::fetch_command(invocation, out)\n}\n";
    let flow_spec = "\npub(crate) const REVIEW_BASE: crate::CommandSpec = crate::CommandSpec {\n    path: &[\"agents-pay\", \"fetch\"],\n    flags: &[],\n    value_flags: &[],\n    strict_values: true,\n    repeatable_flags: &[],\n    max_positionals: 1,\n    passthrough: false,\n    handler: review_handler,\n};\npub(crate) const SPEC: crate::CommandSpec = REVIEW_BASE;\nfn review_handler(_: &crate::Invocation, out: &mut crate::Output<'_>) -> Result<(), toolkit_core::ToolkitError> {\n    let _ = std::env::var_os(\"PAYMENT_MANAGER_ARN\");\n    out.line(super::D32_REFUSAL)\n}\n";
    let edit = |mut sources: Vec<Source>, path: &str, change: &dyn Fn(&str) -> String| {
        let at = sources
            .iter()
            .position(|s| s.path == path)
            .unwrap_or_else(|| panic!("{path} is missing"));
        let changed = change(&sources[at].text);
        assert_ne!(changed, sources[at].text, "the {path} anchor moved");
        assert!(
            syn::parse_file(&changed).is_ok(),
            "the fixture does not parse: {path}"
        );
        sources[at].text = changed;
        sources
    };
    let append = |insert: String| move |text: &str| format!("{text}{insert}");
    let manifest = crate_manifest();
    let workspace = workspace_manifest();
    let lints = "[lints]\nworkspace = true\n";
    let forbid = "unsafe_code = \"forbid\"\n";
    assert!(manifest.contains(lints), "the crate lints moved");
    assert!(workspace.contains(forbid), "the workspace lint moved");
    let no_lints = manifest.replacen(lints, "", 1);
    let mut survived: Vec<&str> = Vec::new();

    // #5421: the registry pin binds to the entries `SLICE` dispatches.
    let shorthand = edit(real_sources(), ROUTE_FILE, &|text| {
        format!(
            "{}\n{env_handler}{dead_pin}",
            text.replacen(&entries, "            handler,\n", 1)
        )
    });
    let struct_update = edit(
        edit(real_sources(), ROUTE_FILE, &|text| {
            format!(
                "{}{dead_pin}",
                text.replacen(
                    &entries,
                    "        ..crate::agents_pay::flow::REVIEW_BASE\n",
                    1
                )
            )
        }),
        "agents_pay/flow.rs",
        &append(flow_spec.to_owned()),
    );
    let foreign_table = edit(
        edit(real_sources(), ROUTE_FILE, &|text| {
            let start = text.find("    commands: &[\n").unwrap();
            let end = start + text[start..].find("    ],\n").unwrap() + "    ],\n".len();
            let table = &text[start + "    commands: ".len()..end - ",\n".len()];
            format!(
                "{}    commands: &[crate::agents_pay::flow::SPEC],\n{}\n#[cfg(any())]\nconst _REVIEW_5419_PINNED: &[crate::CommandSpec] = {table};\n",
                &text[..start],
                &text[end..]
            )
        }),
        "agents_pay/flow.rs",
        &append(flow_spec.to_owned()),
    );
    let foreign_slice = edit(
        edit(real_sources(), "registry.rs", &|text| {
            text.replacen(
                "    crate::agents_pay::SLICE,\n",
                "    crate::agents_pay::flow::REVIEW_SLICE,\n",
                1,
            )
        }),
        "agents_pay/flow.rs",
        &append(format!(
            "{flow_spec}pub(crate) const REVIEW_SLICE: crate::registry::Slice = crate::registry::Slice {{ commands: &[SPEC], ..crate::agents_pay::SLICE }};\n"
        )),
    );
    let cfg_attr_slice = edit(real_sources(), ROUTE_FILE, &|text| {
        text.replacen(
            "pub(crate) const SLICE",
            "#[cfg_attr(all(), cfg(any()))]\npub(crate) const SLICE",
            1,
        )
    });
    let extra_attr = edit(real_sources(), ROUTE_FILE, &|text| {
        text.replacen(
            "        passthrough: false,\n",
            "        #[cfg(all())]\n        passthrough: false,\n",
            1,
        )
    });
    // #5423: no pre-main code, and the unsafe lint that also forbids it is
    // pinned.
    let premain_asm = "\nextern \"C\" fn review_premain() { let _ = std::env::var_os(\"PAYMENT_MANAGER_ARN\"); }\n::core::arch::global_asm!(\".pushsection .init_array,\\\"aw\\\"\", \".balign 8\", \".quad {f}\", \".popsection\", f = sym review_premain);\n";
    let premain_alloc = "\nstruct ReviewAlloc;\nunsafe impl ::core::alloc::GlobalAlloc for ReviewAlloc {\n    unsafe fn alloc(&self, layout: ::core::alloc::Layout) -> *mut u8 {\n        let _ = std::env::var_os(\"PAYMENT_MANAGER_ARN\");\n        unsafe { std::alloc::System.alloc(layout) }\n    }\n    unsafe fn dealloc(&self, ptr: *mut u8, layout: ::core::alloc::Layout) {\n        unsafe { std::alloc::System.dealloc(ptr, layout) }\n    }\n}\n#[global_allocator]\nstatic REVIEW_ALLOC: ReviewAlloc = ReviewAlloc;\n";
    let in_flow = |insert: &str| {
        edit(
            real_sources(),
            "agents_pay/flow.rs",
            &append(insert.to_owned()),
        )
    };
    let in_lib = |insert: &str| edit(real_sources(), "lib.rs", &append(insert.to_owned()));
    let real = real_sources;

    let cases: Vec<(&str, Vec<Source>, String, String)> = vec![
        ("#5421 A: handler shorthand + dead pinned entries", shorthand, manifest.clone(), workspace.clone()),
        ("#5421 B: struct update from flow.rs", struct_update, manifest.clone(), workspace.clone()),
        ("#5421: command table from flow.rs", foreign_table, manifest.clone(), workspace.clone()),
        ("#5421: the registry lists another slice", foreign_slice, manifest.clone(), workspace.clone()),
        ("#5421: cfg_attr on SLICE", cfg_attr_slice, manifest.clone(), workspace.clone()),
        ("#5421: an attribute on another fetch field", extra_attr, manifest.clone(), workspace.clone()),
        ("#5423 C1: lints dropped + global_asm! in flow.rs", in_flow(premain_asm), no_lints.clone(), workspace.clone()),
        ("#5423 C2: lints dropped + #[global_allocator] in flow.rs", in_flow(premain_alloc), no_lints.clone(), workspace.clone()),
        ("#5423: global_asm! in flow.rs", in_flow(premain_asm), manifest.clone(), workspace.clone()),
        ("#5423: #[global_allocator] in lib.rs", in_lib(premain_alloc), manifest.clone(), workspace.clone()),
        ("#5423: asm! in lib.rs", in_lib("\nfn review_asm() { unsafe { ::core::arch::asm!(\"nop\") } }\n"), manifest.clone(), workspace.clone()),
        ("#5423: an asm import in lib.rs", in_lib("\nuse ::core::arch::asm as review_asm;\n"), manifest.clone(), workspace.clone()),
        ("#5423: a naked fn in lib.rs", in_lib("\n#[unsafe(naked)]\nextern \"C\" fn review_naked() { ::core::arch::naked_asm!(\"ret\") }\n"), manifest.clone(), workspace.clone()),
        ("#5423: crate lints dropped", real(), no_lints.clone(), workspace.clone()),
        ("#5423: crate lints not inherited", real(), manifest.replacen(lints, "[lints]\nworkspace = false\n", 1), workspace.clone()),
        ("#5423: crate lints overridden", real(), format!("{manifest}\n[lints.rust]\nunsafe_code = \"allow\"\n"), workspace.clone()),
        ("#5423: workspace allows unsafe", real(), manifest.clone(), workspace.replacen(forbid, "unsafe_code = \"allow\"\n", 1)),
        ("#5423: workspace only warns on unsafe", real(), manifest.clone(), workspace.replacen(forbid, "unsafe_code = \"warn\"\n", 1)),
        // #5423, non-blocking: TOML whitespace in a dependency table header.
        ("#5423 E: [dependencies . core]", real(), format!("{manifest}\n[dependencies . core]\npath = \"../evil\"\n"), workspace.clone()),
        ("#5423 E: [dependencies . toolkit-core] renamed", real(), format!("{manifest}\n[dependencies . toolkit-core]\npackage = \"evil\"\npath = \"../evil\"\n"), workspace.clone()),
    ];
    for (label, sources, manifest, workspace) in &cases {
        if full_gate(sources, manifest, workspace).is_empty() {
            survived.push(label);
        }
    }
    assert!(survived.is_empty(), "survived the gate: {survived:#?}");
}

// ------------ D32 for the admin routes (x402 S6, #5462, AC3)
//
// Every `agents-pay admin *` route and `agents-build setup-payment-user`
// follow the fetch route's structure: in a build without the live feature
// each registry entry is `self::no_payment::admin_command` in
// `admin/mod.rs`, a file module whose whole token stream is pinned, which
// prints `D32_ADMIN_REFUSAL` on stderr and sets exit 1. Both admin tables
// live in `admin/mod.rs` (`SLICE` and `BUILD_SLICE`), so one file holds
// every admin registry entry.

/// The payment tables `crate::registry::SLICES` lists, in its (name) order.
const PAYMENT_TABLES: &[&str] = &[
    "crate::agents_pay::admin::BUILD_SLICE",
    "crate::agents_pay::SLICE",
    "crate::agents_pay::admin::SLICE",
];

const ADMIN_FILE: &str = "agents_pay/admin/mod.rs";
const ADMIN_NO_PAYMENT_FILE: &str = "agents_pay/admin/no_payment.rs";

/// The whole of `admin/no_payment.rs`, compared token for token.
const ADMIN_NO_PAYMENT_PIN: &str = r"
#[allow(clippy::unnecessary_wraps)]
pub(super) fn admin_command(
    _invocation: &crate::Invocation,
    out: &mut crate::Output<'_>,
) -> ::core::result::Result<(), ::toolkit_core::ToolkitError> {
    self::refuse_without_payment_support(out);
    ::core::result::Result::Ok(())
}

fn refuse_without_payment_support(out: &mut crate::Output<'_>) {
    out.diagnostic(super::D32_ADMIN_REFUSAL);
    out.set_exit_status(1);
}
";

/// The admin tables and the routes of each, in order.
const ADMIN_TABLES: &[(&str, &[&[&str]])] = &[
    (
        "SLICE",
        &[
            &["agents-pay", "admin", "init-config"],
            &["agents-pay", "admin", "show-config"],
            &["agents-pay", "admin", "create-instrument"],
            &["agents-pay", "admin", "new-session"],
            &["agents-pay", "admin", "preflight"],
            &["agents-pay", "admin", "setup-openclaw"],
        ],
    ),
    ("BUILD_SLICE", &[&["agents-build", "setup-payment-user"]]),
];

const ADMIN_D32_TEXT: &str =
    "This toolkit build has no payment support; install a release build of toolkit.";

/// The value of `const NAME: &str = "..";` when it is exactly one string
/// literal with no attribute but its docs.
fn const_literal(file: &syn::File, name: &str) -> Option<String> {
    let consts: Vec<&syn::ItemConst> = file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Const(c) if c.ident == name => Some(c),
            _ => None,
        })
        .collect();
    let [item] = consts.as_slice() else {
        return None;
    };
    let is_str = matches!(&*item.ty, Type::Reference(r)
        if r.lifetime.is_none() && r.mutability.is_none()
            && matches!(&*r.elem, Type::Path(p) if p.qself.is_none() && p.path.is_ident("str")));
    let docs_only = item.attrs.iter().all(|a| a.path().is_ident("doc"));
    match &*item.expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(lit),
            attrs,
        }) if is_str && docs_only && attrs.is_empty() => Some(lit.value()),
        _ => None,
    }
}

/// The string-literal words of `path: &["a", "b"]`.
fn path_words(expr: &syn::Expr) -> Option<Vec<String>> {
    array_ref(expr).and_then(|elems| {
        elems
            .iter()
            .map(|e| match e {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    attrs,
                }) if attrs.is_empty() => Some(s.value()),
                _ => None,
            })
            .collect()
    })
}

/// One admin `CommandSpec` literal: its path and exactly the two cfg-split
/// handlers, the second the pinned D32 route.
fn admin_spec_findings(table: &str, spec: &syn::Expr, route: &[&str], found: &mut Vec<String>) {
    let Some(spec) = full_literal(spec, "crate::CommandSpec", found) else {
        return;
    };
    let mut handlers = Vec::new();
    let mut paths = Vec::new();
    for field in &spec.fields {
        match field_name(field).as_str() {
            "handler" => handlers.push(field),
            name => {
                if !field.attrs.is_empty() {
                    found.push(format!(
                        "{ADMIN_FILE}: the {table} `CommandSpec` field `{name}` has an attribute"
                    ));
                }
                if name == "path" {
                    paths.push(&field.expr);
                }
            }
        }
    }
    let words = match paths.as_slice() {
        [expr] => path_words(expr),
        _ => None,
    };
    let expected: Vec<String> = route.iter().map(|w| (*w).to_owned()).collect();
    if words.as_deref() != Some(&expected[..]) {
        found.push(format!(
            "{ADMIN_FILE}: a {table} `CommandSpec` must have `path: &{route:?}`; found {words:?}"
        ));
    }
    let live = format!("feature=\"{LIVE_FEATURE}\"");
    let disabled = format!("not(feature=\"{LIVE_FEATURE}\")");
    let bound = match handlers.as_slice() {
        [first, second] => {
            matches!(first.attrs.as_slice(), [attr] if is_cfg(attr, &live))
                && matches!(&first.expr, syn::Expr::Path(p)
                    if p.attrs.is_empty() && p.qself.is_none() && p.path.segments.len() == 1
                        && p.path.leading_colon.is_none())
                && matches!(second.attrs.as_slice(), [attr] if is_cfg(attr, &disabled))
                && matches!(&second.expr, syn::Expr::Path(p)
                    if p.attrs.is_empty() && p.qself.is_none()
                        && plain_path(&p.path).as_deref() == Some("self::no_payment::admin_command"))
        }
        _ => false,
    };
    if !bound {
        found.push(format!(
            "{ADMIN_FILE}: the `{}` `CommandSpec` must hold exactly a live-only handler then `#[cfg(not(feature = \"{LIVE_FEATURE}\"))] handler: self::no_payment::admin_command`",
            route.join(" ")
        ));
    }
}

/// The admin routes: the pinned file module, its one declaration, the two
/// tables and every route's two handlers, the D32 literal, and no binding
/// that could shadow `no_payment`, `D32_ADMIN_REFUSAL` or a prelude name.
#[allow(clippy::too_many_lines)] // One pass over the pinned file and the tables.
fn admin_findings(sources: &[Source]) -> Vec<String> {
    let text = |path: &str| {
        sources
            .iter()
            .find(|s| s.path == path)
            .map(|s| s.text.as_str())
    };
    let mut found = Vec::new();
    match text(ADMIN_NO_PAYMENT_FILE).map(tokens) {
        None => found.push(format!("{ADMIN_NO_PAYMENT_FILE}: is missing")),
        Some(Err(_)) => found.push(format!("{ADMIN_NO_PAYMENT_FILE}: does not tokenize")),
        Some(Ok(stream)) => {
            let mut actual = Vec::new();
            token_list(stream, &mut actual);
            if actual != list_of_text(ADMIN_NO_PAYMENT_PIN) {
                found.push(format!(
                    "{ADMIN_NO_PAYMENT_FILE}: differs from the pinned admin D32 route"
                ));
            }
        }
    }
    let Some(admin) = text(ADMIN_FILE) else {
        found.push(format!("{ADMIN_FILE}: is missing"));
        return found;
    };
    let (Ok(stream), Ok(file)) = (tokens(admin), parse_source(admin)) else {
        found.push(format!("{ADMIN_FILE}: does not parse"));
        return found;
    };
    let trees: Vec<TokenTree> = stream.into_iter().collect();
    let decls: Vec<Vec<String>> = trees
        .windows(2)
        .enumerate()
        .filter(|(_, w)| {
            matches!((&w[0], &w[1]), (TokenTree::Ident(m), TokenTree::Ident(n)) if m == "mod" && n == "no_payment")
        })
        .map(|(at, _)| list_of_trees(&trees[attr_start(&trees, at)..(at + 3).min(trees.len())]))
        .collect();
    if decls != [list_of_text(&no_payment_decl())] {
        found.push(format!(
            "{ADMIN_FILE}: the `no_payment` module must be declared once, exactly `{}`; found {decls:?}",
            no_payment_decl()
        ));
    }
    let mut handlers = Vec::new();
    handler_fields(&trees, &mut handlers);
    let routes: usize = ADMIN_TABLES.iter().map(|(_, r)| r.len()).sum();
    let disabled = list_of_text(&format!(
        "#[cfg(not(feature = \"{LIVE_FEATURE}\"))]\nhandler: self::no_payment::admin_command,"
    ));
    let disabled_count = handlers.iter().filter(|h| **h == disabled).count();
    if handlers.len() != 2 * routes || disabled_count != routes {
        found.push(format!(
            "{ADMIN_FILE}: expected {routes} live and {routes} pinned D32 `handler:` entries; found {} entries, {disabled_count} pinned",
            handlers.len()
        ));
    }
    if const_literal(&file, "D32_ADMIN_REFUSAL").as_deref() != Some(ADMIN_D32_TEXT) {
        found.push(format!(
            "{ADMIN_FILE}: `D32_ADMIN_REFUSAL` is not one `const D32_ADMIN_REFUSAL: &str = \"{ADMIN_D32_TEXT}\";`"
        ));
    }
    for (table, routes) in ADMIN_TABLES {
        let consts: Vec<&syn::ItemConst> = file
            .items
            .iter()
            .filter_map(|item| match item {
                syn::Item::Const(c) if c.ident == table => Some(c),
                _ => None,
            })
            .collect();
        let [slice] = consts.as_slice() else {
            found.push(format!(
                "{ADMIN_FILE}: needs exactly one top-level `const {table}`, found {}",
                consts.len()
            ));
            continue;
        };
        if !slice.attrs.iter().all(|a| a.path().is_ident("doc")) {
            found.push(format!(
                "{ADMIN_FILE}: `{table}` has an attribute other than docs"
            ));
        }
        let Some(literal) = full_literal(&slice.expr, "crate::registry::Slice", &mut found) else {
            continue;
        };
        let mut commands = Vec::new();
        for field in &literal.fields {
            if !field.attrs.is_empty() {
                found.push(format!(
                    "{ADMIN_FILE}: the `{table}` field `{}` has an attribute",
                    field_name(field)
                ));
            }
            if field_name(field) == "commands" {
                commands.push(&field.expr);
            }
        }
        let specs = match commands.as_slice() {
            [expr] => array_ref(expr),
            _ => None,
        };
        match specs {
            Some(specs) if specs.len() == routes.len() => {
                for (spec, route) in specs.iter().zip(routes.iter()) {
                    admin_spec_findings(table, spec, route, &mut found);
                }
            }
            _ => found.push(format!(
                "{ADMIN_FILE}: `{table}.commands` must be `&[..]` of {} `crate::CommandSpec` literals",
                routes.len()
            )),
        }
    }
    // Bindings: the surface names are bound by their own items only.
    let mut items: Vec<(String, &'static str)> = Vec::new();
    let mut imports: Vec<(String, String)> = Vec::new();
    for item in &file.items {
        match item {
            syn::Item::Use(item) => use_leaves("", &item.tree, &mut imports),
            syn::Item::Macro(item) => found.push(format!(
                "{ADMIN_FILE}: a top-level macro item `{}!` can define any name",
                path_text(&item.mac.path)
            )),
            other => items.extend(item_binding(other)),
        }
    }
    if imports.iter().any(|(name, _)| name == "*") {
        found.push(format!(
            "{ADMIN_FILE}: a glob import can shadow a surface name"
        ));
    }
    let bound = |name: &str| {
        items.iter().filter(|(n, _)| n == name).count()
            + imports.iter().filter(|(n, _)| n == name).count()
    };
    for (name, kind) in [("no_payment", "mod"), ("D32_ADMIN_REFUSAL", "const")] {
        let of_kind = items
            .iter()
            .filter(|(n, k)| n == name && *k == kind)
            .count();
        if of_kind != 1 || bound(name) != 1 {
            found.push(format!(
                "{ADMIN_FILE}: `{name}` must be bound by exactly one top-level {kind} item, found {} binding(s)",
                bound(name)
            ));
        }
    }
    for name in PRELUDE_NAMES {
        if bound(name) != 0 {
            found.push(format!("{ADMIN_FILE}: `{name}` is shadowed"));
        }
    }
    found
}

#[test]
fn t_ac3_admin_routes_the_real_sources_keep_every_d32_pin() {
    let sources = real_sources();
    let found = admin_findings(&sources);
    assert!(found.is_empty(), "{found:#?}");
    let pinned = list_of_text(ADMIN_NO_PAYMENT_PIN);
    assert!(pinned.len() > 60, "{}", pinned.len());
    assert_eq!(
        ADMIN_D32_TEXT,
        toolkit_cli::agents_pay::deps::NO_PAYMENT_SUPPORT
    );
    assert_eq!(
        ADMIN_D32_TEXT,
        toolkit_cli::agents_pay::admin::D32_ADMIN_REFUSAL
    );
}

#[test]
fn t_ac3_admin_routes_catch_every_seeded_violation() {
    let admin =
        |anchor: &str, edit: &dyn Fn(&str) -> String| sources_with(ADMIN_FILE, anchor, edit);
    let refusal = "    out.diagnostic(super::D32_ADMIN_REFUSAL);\n";
    let disabled = format!(
        "            #[cfg(not(feature = \"{LIVE_FEATURE}\"))]\n            handler: self::no_payment::admin_command,\n"
    );
    let mutations: Vec<(&str, Vec<Source>)> = vec![
        (
            "env read before the admin refusal",
            sources_with(ADMIN_NO_PAYMENT_FILE, refusal, |a| {
                format!("    let _ = ::std::env::var_os(\"HOME\");\n{a}")
            }),
        ),
        (
            "admin refusal exits 0",
            sources_with(
                ADMIN_NO_PAYMENT_FILE,
                "    out.set_exit_status(1);\n",
                |_| "    out.set_exit_status(0);\n".to_owned(),
            ),
        ),
        (
            "a disabled handler that runs the live route",
            admin(&disabled, &|_| {
                format!(
                    "            #[cfg(not(feature = \"{LIVE_FEATURE}\"))]\n            handler: init_config_command,\n"
                )
            }),
        ),
        (
            "a handler without its cfg split",
            admin(&disabled, &|_| String::new()),
        ),
        (
            "the D32 text changed",
            admin("install a release build of toolkit.\";", &|_| {
                "install a release build.\";".to_owned()
            }),
        ),
        (
            "a second declaration of no_payment",
            admin("mod no_payment;\n", &|a| {
                format!("{a}#[cfg(any())]\nmod no_payment;\n")
            }),
        ),
        (
            "an import that shadows the D32 text",
            admin("use std::path::{Path, PathBuf};\n", &|a| {
                format!("{a}use crate::agents_pay::D32_REFUSAL as D32_ADMIN_REFUSAL;\n")
            }),
        ),
        (
            "a route moved out of its table",
            admin(
                "            path: &[\"agents-pay\", \"admin\", \"preflight\"],",
                &|_| "            path: &[\"agents-pay\", \"admin\", \"preflight-x\"],".to_owned(),
            ),
        ),
        (
            "an attribute on the build table",
            admin("pub(crate) const BUILD_SLICE", &|a| {
                format!("#[cfg_attr(all(), cfg(any()))]\n{a}")
            }),
        ),
        (
            "an admin table missing from the registry",
            sources_with(
                "registry.rs",
                "    crate::agents_pay::admin::BUILD_SLICE,\n",
                |_| String::new(),
            ),
        ),
    ];
    for (label, sources) in mutations {
        let found = route_gate(&sources);
        assert!(!found.is_empty(), "{label}: the gate found nothing");
    }
}

// ------------------------------------------- OpenClaw B2 bridge (#5443)

/// The real sources with `text` appended to `path`.
fn appended(path: &str, text: &str) -> Vec<Source> {
    let mut sources = real_sources();
    let at = sources
        .iter()
        .position(|s| s.path == path)
        .unwrap_or_else(|| panic!("{path} is missing"));
    sources[at].text.push_str(text);
    sources
}

/// T-BR-D32 (structural part): the bridge route is in the compiled table,
/// its default-build entry is the pinned file, its live entry is pinned,
/// and its pure route names no world.
#[test]
fn t_br_d32_the_bridge_route_is_pinned_and_bound() {
    let sources = real_sources();
    let found: Vec<String> = bridge_route_findings(&sources)
        .into_iter()
        .chain(bridge_world_findings(&sources))
        .chain(slice_findings(&sources))
        .collect();
    assert!(found.is_empty(), "{found:#?}");
    let pinned = list_of_text(BRIDGE_NO_PAYMENT_PIN);
    assert!(pinned.contains(&"refuse_without_payment_support".to_owned()));
    assert!(pinned.contains(&"UNAVAILABLE_FRAME".to_owned()));
    assert!(list_of_text(BRIDGE_LIVE_PIN).contains(&"for_process".to_owned()));
    assert_eq!(
        toolkit_cli::agents_pay::bridge::UNAVAILABLE_FRAME,
        BRIDGE_UNAVAILABLE
    );
}

#[test]
#[allow(clippy::too_many_lines)] // A table of mutations, each reported by label.
fn t_br_d32_catches_bridge_route_mutations() {
    let np = BRIDGE_NO_PAYMENT_FILE;
    let mutations: Vec<(&str, Vec<Source>)> = vec![
        (
            "env read in the default bridge entry",
            sources_with(np, "    super::serve(\n", |a| {
                format!("    let _ = ::std::env::var_os(\"PAYMENT_SESSION_ID\");\n{a}")
            }),
        ),
        (
            "the default refusal is not the literal",
            sources_with(np, "super::UNAVAILABLE_FRAME", |_| {
                "::std::env::var(\"X\").unwrap_or_default().as_bytes()".to_owned()
            }),
        ),
        (
            "a doc comment in the pinned bridge file",
            sources_with(np, "fn refuse_without_payment_support(", |a| {
                format!("/// Refuses.\n{a}")
            }),
        ),
        (
            "an extra item in the pinned bridge file",
            appended(np, "\nconst _: () = ();\n"),
        ),
        (
            "the bridge D32 frame becomes an expression",
            sources_with(BRIDGE_FILE, "pub const UNAVAILABLE_FRAME: &[u8] = ", |a| {
                format!("{a}env!(\"HOME\").as_bytes();\nconst UNUSED: &[u8] = ")
            }),
        ),
        (
            "the bridge D32 frame changes",
            sources_with(BRIDGE_FILE, "payment support unavailable", |_| {
                "payment support missing".to_owned()
            }),
        ),
        (
            "the bridge no_payment declaration loses its cfg",
            sources_with(
                BRIDGE_FILE,
                &format!("#[cfg(not(feature = \"{LIVE_FEATURE}\"))]\npub(super) mod no_payment;"),
                |_| "pub(super) mod no_payment;".to_owned(),
            ),
        ),
        (
            "a second serve binding in bridge/mod.rs",
            appended(BRIDGE_FILE, "\nuse self::render::frame as serve;\n"),
        ),
        (
            "a shadowed Ok in bridge/mod.rs",
            appended(BRIDGE_FILE, "\nfn Ok() {}\n"),
        ),
        (
            "the live entry answers before the pure route",
            sources_with(
                BRIDGE_FILE,
                "self::handle_service(&world.deps(), request)",
                |_| "self::render::error_frame(self::protocol::BridgeError::Input)".to_owned(),
            ),
        ),
        (
            "the bridge reads a buffered stdin",
            sources_with(
                BRIDGE_FILE,
                "::std::boxed::Box::new(::std::fs::File::from(owned))",
                |_| "::std::boxed::Box::new(::std::io::stdin())".to_owned(),
            ),
        ),
        (
            "a second live entry",
            appended(
                BRIDGE_FILE,
                "\npub(in crate::agents_pay) fn live_command() {}\n",
            ),
        ),
        (
            "a third registry command preserves the pinned handlers",
            sources_with(
                ROUTE_FILE,
                "handler: self::bridge::no_payment::bridge_command,\n        },",
                |a| {
                    format!(
                        "{a}\n        crate::CommandSpec {{ ..crate::agents_pay::flow::SPEC }},"
                    )
                },
            ),
        ),
        (
            "the default registry entry dispatches elsewhere",
            sources_with(
                ROUTE_FILE,
                "handler: self::bridge::no_payment::bridge_command,",
                |_| "handler: self::bridge::live_command,".to_owned(),
            ),
        ),
        (
            "the bridge takes a positional",
            sources_with(ROUTE_FILE, "max_positionals: 0,", |_| {
                "max_positionals: 1,".to_owned()
            }),
        ),
        (
            "the bridge takes a flag",
            sources_with(
                ROUTE_FILE,
                "path: &[\"agents-pay\", \"bridge\"],\n            flags: &[],",
                |a| a.replace("flags: &[]", "flags: &[\"--debug\"]"),
            ),
        ),
        (
            "the bridge command is renamed",
            sources_with(ROUTE_FILE, "&[\"agents-pay\", \"bridge\"]", |_| {
                "&[\"agents-pay\", \"glue\"]".to_owned()
            }),
        ),
        (
            "the pure route reads the environment",
            appended(
                "agents_pay/bridge/output_scrub.rs",
                "\nfn smuggle() { let _ = ::std::env::var(\"HOME\"); }\n",
            ),
        ),
        (
            "the protocol builds a world",
            appended(
                "agents_pay/bridge/protocol.rs",
                "\nfn smuggle() { let _ = super::super::deps::Deps::for_process(); }\n",
            ),
        ),
        (
            "the scrub reaches a payment step",
            appended(
                "agents_pay/bridge/output_scrub.rs",
                "\nfn smuggle() { let _ = crate::agents_pay::flow::prove; }\n",
            ),
        ),
    ];
    let mut survivors = Vec::new();
    for (label, sources) in &mutations {
        if route_gate(sources).is_empty() {
            survivors.push(*label);
        }
    }
    assert!(survivors.is_empty(), "survived: {survivors:#?}");
}

#[test]
fn t_oc_final_route_the_pure_route_names_no_world() {
    let bridge = |serve_frame: &str| {
        format!(
            "pub fn serve() {{}}\npub fn serve_quiet() {{}}\npub fn serve_frame() {{ {serve_frame} }}\nfn f() {{ flow::prove(); }}\n"
        )
    };
    let clean = [
        (BRIDGE_FILE, bridge("render::frame()")),
        ("agents_pay/bridge/no_payment.rs", String::new()),
        ("agents_pay/bridge/protocol.rs", String::new()),
        ("agents_pay/bridge/render.rs", String::new()),
        ("agents_pay/bridge/output_scrub.rs", String::new()),
    ];
    let seeded_set = |files: &[(&str, String)]| {
        seeded(
            &files
                .iter()
                .map(|(p, t)| (*p, t.as_str()))
                .collect::<Vec<_>>(),
        )
    };
    assert!(bridge_world_findings(&seeded_set(&clean)).is_empty());
    for (path, text) in [
        (BRIDGE_FILE, bridge("let _ = Deps::for_process();")),
        (BRIDGE_FILE, bridge("handle_service(deps, request)")),
        (BRIDGE_FILE, bridge("::std::fs::read(\"x\")")),
        (
            "agents_pay/bridge/render.rs",
            "fn f() { std::env::var(\"X\"); }".to_owned(),
        ),
        (
            "agents_pay/bridge/protocol.rs",
            "fn f(c: &dyn ConfigStore) {}".to_owned(),
        ),
        (
            "agents_pay/bridge/output_scrub.rs",
            "fn f() { std::net::TcpStream::connect(\"x\"); }".to_owned(),
        ),
        (
            "agents_pay/bridge/mod.rs",
            "pub fn serve() {}\npub fn serve_quiet() {}\nfn f() { flow::prove(); }".to_owned(),
        ),
    ] {
        let mut files = clean.to_vec();
        files.retain(|(p, _)| *p != path);
        files.push((path, text.clone()));
        assert!(
            !bridge_world_findings(&seeded_set(&files)).is_empty(),
            "{path}: {text}"
        );
    }
}

#[test]
fn t_br_one_1_the_bridge_names_no_spending_constructor() {
    let found = bridge_world_findings(&real_sources());
    assert!(found.is_empty(), "{found:#?}");
    for bad in [
        "fn f(a: Approved) {}",
        "fn f() { let _ = Approved::new; }",
        "fn f(r: &ProcessPaymentRequest) {}",
        "fn f(api: &dyn PaymentsApi) { api.process_payment(r); }",
        "fn f() { agentcore::process_payment(api, a); }",
        "fn f() { policy::derive_client_token(s, u, a, None); }",
    ] {
        let sources = appended("agents_pay/bridge/mod.rs", &format!("\n{bad}\n"));
        assert!(!bridge_world_findings(&sources).is_empty(), "{bad}");
    }
    // A second route to spending: prove called twice, or not at all.
    let twice = appended(BRIDGE_FILE, "\nfn again() { super::flow::prove(); }\n");
    assert!(!bridge_world_findings(&twice).is_empty());
}

#[test]
fn t_br_one_2_prove_is_the_only_caller_of_process_payment() {
    let found = one_path_findings(&real_sources());
    assert!(found.is_empty(), "{found:#?}");
    for (path, bad) in [
        (
            "agents_pay/flow.rs",
            "\nfn shortcut() { let _ = agentcore::process_payment(api, approved); }\n",
        ),
        (
            "agents_pay/mod.rs",
            "\nfn shortcut() { let _ = agentcore::process_payment(api, approved); }\n",
        ),
        (
            "agents_pay/vault.rs",
            "\nfn shortcut() { api.process_payment(&request); }\n",
        ),
        (
            "agents_pay/bridge/mod.rs",
            "\nfn shortcut() { process_payment(api, approved); }\n",
        ),
    ] {
        assert!(
            !one_path_findings(&appended(path, bad)).is_empty(),
            "{path}: {bad}"
        );
    }
    // A trait method definition in a fake is not a call.
    let fake = appended(
        "agents_pay/vault.rs",
        "\nimpl X for Y { fn process_payment(&self) {} }\n",
    );
    assert!(one_path_findings(&fake).is_empty());
}

#[test]
fn t_br_one_3_live_rs_gains_no_public_item() {
    let found = live_items_findings(&real_sources());
    assert!(found.is_empty(), "{found:#?}");
    for bad in [
        "\npub fn bridge_world() {}\n",
        "\nimpl World { pub fn for_bridge() -> Self { todo!() } }\n",
        "\npub struct BridgeWorld;\n",
        "\npub use super::deps::Deps as Exported;\n",
    ] {
        assert!(
            !live_items_findings(&appended("agents_pay/live.rs", bad)).is_empty(),
            "{bad}"
        );
    }
}

#[test]
fn t_br_one_4_into_trusted_glue_has_one_caller() {
    let found = glue_findings(&real_sources());
    assert!(found.is_empty(), "{found:#?}");
    for (path, bad) in [
        (
            "agents_pay/bridge/mod.rs",
            "\nfn f(p: VerifiedProof) { let _ = p.into_trusted_glue(); }\n",
        ),
        (
            "agents_pay/flow.rs",
            "\nfn f(p: VerifiedProof) { let _ = p.into_trusted_glue(); }\n",
        ),
        (
            "agents_pay/bridge/render.rs",
            "\nfn g(p: VerifiedProof) { let _ = p.into_trusted_glue(); }\n",
        ),
        (
            "agents_pay/vault.rs",
            "\nfn f() { let _ = VerifiedProof::into_trusted_glue; }\n",
        ),
    ] {
        assert!(
            !glue_findings(&appended(path, bad)).is_empty(),
            "{path}: {bad}"
        );
    }
}
