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
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agents_pay/agentcore/request.rs");
    let source = fs::read_to_string(path).unwrap();
    check_process_request(&source);
}

fn check_process_request(source: &str) {
    let file = syn::parse_file(source).unwrap();
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
    let Ok(file) = syn::parse_file(text) else {
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
        let Ok(stream) = text.parse::<TokenStream>() else {
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
            if let Ok(file) = syn::parse_file(text) {
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
            let at_mod = at_mod.filter(|&at| matches!(&rest[at], TokenTree::Ident(i) if i == "mod"));
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
        } else if let Ok(stream) = source.text.parse::<TokenStream>() {
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

/// The one live name a test may spell: the D32 unit test in `deps.rs`
/// asserts that the disabled constructor returns `Unavailable` (it cannot
/// return anything else without the live feature, G1a).
const G4_ALLOWED: &str = "agents_pay/deps.rs (inline tests): names Deps::for_process";

/// G4 findings without the single allowed D32 assertion.
fn g4_unexpected(files: &[(String, String)]) -> Vec<String> {
    let mut found = g4_findings(files);
    if let Some(at) = found.iter().position(|f| f == G4_ALLOWED) {
        found.remove(at);
    }
    found
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
    let anchor = "    fn d32_for_process_is_unavailable_in_verification_builds() {\n";
    assert!(deps.contains(anchor), "the D32 unit test moved");
    // The #5377 mutation: a live resolver named inside the inline test.
    let bad = deps.replacen(
        anchor,
        &format!("{anchor}        let _ = toolkit_aws::SystemResolver;\n"),
        1,
    );
    let mut modules = Vec::new();
    inline_test_modules(bad.parse().unwrap(), &mut modules);
    let files: Vec<(String, String)> = modules
        .into_iter()
        .map(|m| ("agents_pay/deps.rs (inline tests)".to_owned(), m))
        .collect();
    assert!(!g4_unexpected(&files).is_empty(), "inline mutation survived");
    // A second for_process naming is not covered by the one allowance.
    let twice = deps.replacen(
        anchor,
        &format!("{anchor}        let _ = Deps::for_process();\n"),
        1,
    );
    let mut modules = Vec::new();
    inline_test_modules(twice.parse().unwrap(), &mut modules);
    let files: Vec<(String, String)> = modules
        .into_iter()
        .map(|m| ("agents_pay/deps.rs (inline tests)".to_owned(), m))
        .collect();
    assert!(!g4_unexpected(&files).is_empty(), "second naming survived");
    // Production code outside the test module is not a test file.
    let mut modules = Vec::new();
    inline_test_modules(
        "fn live() { let _ = SystemResolver; } #[cfg(test)] mod tests { fn t() {} }"
            .parse()
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
        let Ok(stream) = source.text.parse::<TokenStream>() else {
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
            let Ok(stream) = source.text.parse::<TokenStream>() else {
                continue;
            };
            let mut words = Vec::new();
            idents(stream, &mut words);
            words
        } else if source.path == "agents_pay/agentcore.rs" {
            let Ok(stream) = source.text.parse::<TokenStream>() else {
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

// ------------------------------------- D32 read boundary (#5376, AC3)

/// The disabled route: everything `toolkit agents-pay fetch` evaluates
/// before it prints D32 in a build without the live feature.
const ROUTE_FILE: &str = "agents_pay/mod.rs";
const DEPS_FILE: &str = "agents_pay/deps.rs";
const ROUTE_FNS: &[&str] = &["fetch_command", "parse", "emit"];

/// Every path the route functions may evaluate: argv, the credential-flag
/// check over argv, the D32 call, the JSON document and its rendering, and
/// local bindings. Fail closed: anything else, such as `std::env::var_os`,
/// `fs::read_to_string` or `std::io::stdin`, is a finding.
const ROUTE_PATHS: &[&str] = &[
    "Deps::for_process",
    "Err",
    "FETCH_USAGE",
    "Json::Bool",
    "Json::object",
    "Json::string",
    "None",
    "Ok",
    "Request::Browser",
    "Request::Fetch",
    "Request::Status",
    "Some",
    "ToolkitError::usage",
    "document",
    "emit",
    "flag",
    "flow::render",
    "invocation",
    "method",
    "out",
    "parse",
    "purchase_id",
    "reason",
    "request",
    "run_request",
    "status",
    "str::to_owned",
    "toolkit_aws::reject_credential_arguments",
    "url",
    "words",
    "world",
];
const ROUTE_METHODS: &[&str] = &[
    "clone",
    "cloned",
    "deps",
    "diagnostic",
    "extend",
    "filter",
    "first",
    "has_flag",
    "is_empty",
    "iter",
    "line",
    "map",
    "map_err",
    "set_exit_status",
    "to_owned",
    "unwrap_or",
    "value",
];
const ROUTE_MACROS: &[&str] = &["matches", "vec"];

/// The paths, method names and macro names an expression tree evaluates.
#[derive(Default)]
struct Evaluated {
    paths: Vec<String>,
    methods: Vec<String>,
    macros: Vec<String>,
}

impl<'ast> Visit<'ast> for Evaluated {
    fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
        // A qualified self (`<T as Trait>::f`) is never part of the route.
        if expr.qself.is_some() {
            self.paths.push("<qualified self>".to_owned());
        }
        self.paths.push(path_text(&expr.path));
        visit::visit_expr_path(self, expr);
    }
    fn visit_expr_struct(&mut self, expr: &'ast syn::ExprStruct) {
        self.paths.push(path_text(&expr.path));
        visit::visit_expr_struct(self, expr);
    }
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.methods.push(call.method.to_string());
        visit::visit_expr_method_call(self, call);
    }
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        // Only allowed macros pass, and their bodies are parsed and checked
        // as expressions: `vec![a, b]` and `matches!(e, PAT if guard)`.
        let name = path_text(&mac.path);
        let parsed = match name.as_str() {
            "vec" => mac
                .parse_body_with(Punctuated::<syn::Expr, Token![,]>::parse_terminated)
                .map(|exprs| exprs.into_iter().collect::<Vec<_>>()),
            "matches" => mac.parse_body_with(|input: syn::parse::ParseStream<'_>| {
                let scrutinee: syn::Expr = input.parse()?;
                input.parse::<Token![,]>()?;
                syn::Pat::parse_multi_with_leading_vert(input)?;
                let mut exprs = vec![scrutinee];
                if input.parse::<Option<Token![if]>>()?.is_some() {
                    exprs.push(input.parse()?);
                }
                input.parse::<Option<Token![,]>>()?;
                Ok(exprs)
            }),
            _ => Ok(Vec::new()),
        };
        match parsed {
            Ok(exprs) => {
                for expr in &exprs {
                    self.visit_expr(expr);
                }
            }
            Err(_) => self.macros.push(format!("{name} (unparsed body)")),
        }
        self.macros.push(name);
    }
    fn visit_item(&mut self, item: &'ast syn::Item) {
        // A nested item (fn, static, impl) inside a route fn hides code.
        self.paths
            .push(format!("<nested item {}>", item_kind(item)));
    }
}

fn item_kind(item: &syn::Item) -> &'static str {
    match item {
        syn::Item::Fn(_) => "fn",
        syn::Item::Static(_) => "static",
        syn::Item::Const(_) => "const",
        syn::Item::Impl(_) => "impl",
        syn::Item::Use(_) => "use",
        _ => "item",
    }
}

/// The whitespace-free spelling of a token stream.
fn compact(stream: &TokenStream) -> String {
    stream
        .to_string()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// `for_process` must be exactly the two `cfg` arms, and the arm without the
/// live feature exactly the D32 error: no statement can run before it.
fn for_process_findings(text: &str) -> Vec<String> {
    let Ok(stream) = text.parse::<TokenStream>() else {
        return vec![format!("{DEPS_FILE}: does not tokenize")];
    };
    let mut bodies = Vec::new();
    fn_body_trees(stream, "for_process", &mut bodies);
    if bodies.len() != 1 {
        return vec![format!(
            "{DEPS_FILE}: {} `fn for_process` bodies, expected 1",
            bodies.len()
        )];
    }
    let trees = &bodies[0];
    let live = format!("cfg(feature=\"{LIVE_FEATURE}\")");
    let disabled = format!("cfg(not(feature=\"{LIVE_FEATURE}\"))");
    let shape: Vec<String> = trees
        .iter()
        .map(|tree| match tree {
            TokenTree::Group(group) => match group.delimiter() {
                proc_macro2::Delimiter::Bracket => format!("[{}]", compact(&group.stream())),
                proc_macro2::Delimiter::Brace => "{}".to_owned(),
                _ => "()".to_owned(),
            },
            other => other.to_string(),
        })
        .collect();
    let expected = ["#", &format!("[{live}]"), "{}", "#", &format!("[{disabled}]"), "{}"];
    if shape != expected {
        return vec![format!(
            "{DEPS_FILE}: for_process is not exactly the two cfg arms: {shape:?}"
        )];
    }
    let TokenTree::Group(arm) = &trees[5] else {
        unreachable!("checked above");
    };
    let body = compact(&arm.stream());
    if body == "Err(Unavailable(NO_PAYMENT_SUPPORT.to_owned()))" {
        Vec::new()
    } else {
        vec![format!(
            "{DEPS_FILE}: the arm without the live feature does more than return D32: {body}"
        )]
    }
}

/// The top-level token trees of every `fn NAME` body, at any depth.
fn fn_body_trees(stream: TokenStream, name: &str, out: &mut Vec<Vec<TokenTree>>) {
    let trees: Vec<TokenTree> = stream.into_iter().collect();
    for (index, tree) in trees.iter().enumerate() {
        if let TokenTree::Group(group) = tree {
            fn_body_trees(group.stream(), name, out);
        }
        let is_fn = matches!(tree, TokenTree::Ident(i) if i == "fn");
        let named = matches!(trees.get(index + 1), Some(TokenTree::Ident(i)) if i == name);
        if is_fn && named {
            let body = trees[index + 2..].iter().find_map(|t| match t {
                TokenTree::Group(g) if g.delimiter() == proc_macro2::Delimiter::Brace => {
                    Some(g.stream().into_iter().collect())
                }
                _ => None,
            });
            if let Some(body) = body {
                out.push(body);
            }
        }
    }
}

/// Every route function exists once at the top level of `mod.rs` and
/// evaluates only allowed paths, methods and macros.
fn route_findings(text: &str) -> Vec<String> {
    let Ok(file) = syn::parse_file(text) else {
        return vec![format!("{ROUTE_FILE}: does not parse")];
    };
    let mut found = Vec::new();
    for name in ROUTE_FNS {
        let fns: Vec<&syn::ItemFn> = file
            .items
            .iter()
            .filter_map(|item| match item {
                syn::Item::Fn(function) if function.sig.ident == name => Some(function),
                _ => None,
            })
            .collect();
        let [function] = fns.as_slice() else {
            found.push(format!("{ROUTE_FILE}: {} `fn {name}`, expected 1", fns.len()));
            continue;
        };
        let mut evaluated = Evaluated::default();
        evaluated.visit_block(&function.block);
        for path in evaluated.paths {
            if !ROUTE_PATHS.contains(&path.as_str()) {
                found.push(format!("{ROUTE_FILE}: {name} evaluates {path}"));
            }
        }
        for method in evaluated.methods {
            if !ROUTE_METHODS.contains(&method.as_str()) {
                found.push(format!("{ROUTE_FILE}: {name} calls .{method}()"));
            }
        }
        for mac in evaluated.macros {
            if !ROUTE_MACROS.contains(&mac.as_str()) {
                found.push(format!("{ROUTE_FILE}: {name} expands {mac}!"));
            }
        }
    }
    found
}

fn real_source(path: &str) -> String {
    real_sources()
        .into_iter()
        .find(|source| source.path == path)
        .unwrap_or_else(|| panic!("{path} is missing"))
        .text
}

#[test]
fn t_ac3_noread_the_disabled_route_reads_nothing_before_d32() {
    let found: Vec<String> = route_findings(&real_source(ROUTE_FILE))
        .into_iter()
        .chain(for_process_findings(&real_source(DEPS_FILE)))
        .collect();
    assert!(found.is_empty(), "{found:#?}");
}

const GOOD_FOR_PROCESS: &str = r#"
impl Deps<'static> {
    pub fn for_process() -> Result<ProcessWorld, Unavailable> {
        #[cfg(feature = "LIVE")]
        {
            super::live::World::for_process().map(|world| ProcessWorld::Live(Box::new(world)))
        }
        #[cfg(not(feature = "LIVE"))]
        {
            Err(Unavailable(NO_PAYMENT_SUPPORT.to_owned()))
        }
    }
}
"#;

fn seeded_for_process(edit: impl Fn(&str) -> String) -> String {
    edit(&GOOD_FOR_PROCESS.replace("LIVE", LIVE_FEATURE))
}

#[test]
fn t_ac3_noread_catches_a_read_before_the_d32_refusal() {
    assert!(for_process_findings(&seeded_for_process(str::to_owned)).is_empty());
    for (label, bad) in [
        // The #5376 mutation: an environment read before the D32 error.
        (
            "env read in the disabled arm",
            seeded_for_process(|good| {
                good.replace(
                    "            Err(Unavailable",
                    "            let _ = std::env::var_os(\"PAYMENT_SESSION_ID\");\n            Err(Unavailable",
                )
            }),
        ),
        (
            "file read before both arms",
            seeded_for_process(|good| {
                good.replace(
                    "Unavailable> {\n",
                    "Unavailable> {\n        let _ = std::fs::read(\"/etc/hosts\");\n",
                )
            }),
        ),
        (
            "a reason read from the environment",
            seeded_for_process(|good| {
                good.replace(
                    "NO_PAYMENT_SUPPORT.to_owned()",
                    "std::env::var(\"X\").unwrap_or_default()",
                )
            }),
        ),
        (
            "no disabled arm",
            seeded_for_process(|good| good.replace("not(", "all(")),
        ),
        ("no for_process", "impl Deps<'static> {}".to_owned()),
    ] {
        assert!(!for_process_findings(&bad).is_empty(), "{label}: {bad}");
    }
}

#[test]
fn t_ac3_noread_catches_a_read_in_the_route_functions() {
    let real = real_source(ROUTE_FILE);
    assert!(route_findings(&real).is_empty());
    let anchor = "    let Some(request) = parse(invocation, out)? else {";
    assert!(real.contains(anchor), "the route anchor moved");
    for (label, insert) in [
        (
            "env read",
            "    let _ = std::env::var_os(\"PAYMENT_SESSION_ID\");\n",
        ),
        (
            "file read",
            "    let _ = std::fs::read_to_string(\"config.json\");\n",
        ),
        ("stdin", "    let _ = std::io::stdin();\n"),
        (
            "macro-hidden read",
            "    let _ = format!(\"{:?}\", std::env::var(\"HOME\"));\n",
        ),
        (
            "unlisted method",
            "    let _ = invocation.positionals.first().map(|p| p.read_file());\n",
        ),
        (
            "nested fn",
            "    fn hidden() {}\n",
        ),
        (
            "read inside vec!",
            "    let _ = vec![std::env::var_os(\"X\")];\n",
        ),
        (
            "read in a matches! guard",
            "    let _ = matches!(1, 1 if std::env::var_os(\"X\").is_some());\n",
        ),
    ] {
        let bad = real.replacen(anchor, &format!("{insert}{anchor}"), 1);
        assert!(!route_findings(&bad).is_empty(), "{label}");
    }
    let parse_anchor = "    let method = invocation.value(\"--method\").unwrap_or(\"GET\");";
    assert!(real.contains(parse_anchor), "the parse anchor moved");
    let bad = real.replacen(
        parse_anchor,
        &format!("    let _ = std::env::var(\"AWS_PROFILE\");\n{parse_anchor}"),
        1,
    );
    assert!(!route_findings(&bad).is_empty(), "env read in parse");
    assert!(!route_findings("fn parse() {}").is_empty(), "missing fns");
}
