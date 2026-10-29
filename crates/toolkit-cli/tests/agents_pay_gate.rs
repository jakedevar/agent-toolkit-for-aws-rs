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

use std::fs;
use std::path::Path;

use proc_macro2::{TokenStream, TokenTree};
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
        let Ok(file) = syn::parse_file(&source.text) else {
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

        let stream: TokenStream = source.text.parse().expect("tokens of a parsed file");
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
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agents_pay/agentcore.rs");
    let source = fs::read_to_string(path).unwrap();
    let file = syn::parse_file(&source).unwrap();
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
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0].sig.ident, "from_approved");
    assert!(matches!(methods[0].vis, Visibility::Inherited));
    assert_eq!(methods[0].sig.inputs.len(), 1);
    let syn::FnArg::Typed(arg) = &methods[0].sig.inputs[0] else {
        panic!("constructor must take Approved");
    };
    let Type::Reference(reference) = &*arg.ty else {
        panic!("constructor must borrow Approved");
    };
    assert!(matches!(&*reference.elem, Type::Path(p) if p.path.is_ident("Approved")));
}

#[test]
fn payment_service_model_is_embedded() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agents_pay/agentcore.rs");
    let source = fs::read_to_string(path).unwrap();
    let file = syn::parse_file(&source).unwrap();
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
