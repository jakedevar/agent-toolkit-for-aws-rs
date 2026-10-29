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

/// The registry's `handler:` entries in `mod.rs`, in order.
fn registry_handlers() -> Vec<String> {
    vec![
        format!("#[cfg(feature = \"{LIVE_FEATURE}\")]\nhandler: fetch_command,"),
        format!(
            "#[cfg(not(feature = \"{LIVE_FEATURE}\"))]\nhandler: self::no_payment::fetch_command,"
        ),
    ]
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
    let mut found = Vec::new();
    let mut items: Vec<(String, &'static str)> = Vec::new();
    let mut imports: Vec<(String, String)> = Vec::new();
    for item in &file.items {
        match item {
            syn::Item::Use(item) => use_leaves("", &item.tree, &mut imports),
            syn::Item::Macro(item) => found.push(format!(
                "{ROUTE_FILE}: a top-level macro item `{}!` can define any name",
                path_text(&item.mac.path)
            )),
            other => items.extend(item_binding(other)),
        }
    }
    for (name, path) in &imports {
        if name == "*" {
            found.push(format!(
                "{ROUTE_FILE}: glob import `{path}` can shadow a surface name"
            ));
        }
    }
    let bound = |name: &str| {
        items.iter().filter(|(n, _)| n == name).count()
            + imports.iter().filter(|(n, _)| n == name).count()
    };
    for (name, kind) in SURFACE_ITEMS {
        let of_kind = items.iter().filter(|(n, k)| n == name && k == kind).count();
        if of_kind != 1 || bound(name) != 1 {
            found.push(format!(
                "{ROUTE_FILE}: `{name}` must be bound by exactly one top-level {kind} item, found {} binding(s)",
                bound(name)
            ));
        }
    }
    for name in PRELUDE_NAMES {
        if bound(name) != 0 {
            found.push(format!("{ROUTE_FILE}: `{name}` is shadowed"));
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
    for line in manifest.lines().map(str::trim) {
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

fn crate_manifest() -> String {
    fs::read_to_string(manifest_dir().join("Cargo.toml")).expect("the toolkit-cli manifest")
}

/// The whole D32 gate over a source set and the real manifest.
fn route_gate(sources: &[Source]) -> Vec<String> {
    d32_findings(sources)
        .into_iter()
        .chain(pay_dir_findings(sources))
        .chain(root_crate_findings(sources, &crate_manifest()))
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
    let disabled_entry = format!("#[cfg(not(feature = \"{LIVE_FEATURE}\"))]\n        handler:");
    let live_entry =
        format!("#[cfg(feature = \"{LIVE_FEATURE}\")]\n        handler: fetch_command,");
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
