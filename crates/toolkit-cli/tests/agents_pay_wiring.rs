//! T-PROOTS-USE (x402 S5 #5352, design R-TLS-PAY "Which roots each plane
//! uses"): the merchant and `AgentCore` data-plane transports trust the very
//! `PaymentRoots` allocation the wiring was given, including provider HTTP.
//! Roots come from injected loaders over a checked-in
//! test certificate; nothing is opened or sent (G4).
#![cfg(feature = "aws-sdk")]
#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::path::Path;
use std::sync::Arc;

use toolkit_aws::sdk::roots::{LoadedCerts, RootEnv};
use toolkit_aws::sdk::roots_payment::{
    validate_payment_roots, PaymentRootLoader, PaymentRootPlatform, PaymentRoots,
};
use toolkit_aws::sdk::SdkRuntime;
use toolkit_cli::agents_pay::wiring;

/// A resolver that is `Send + Sync`, as the SDK transport requires.
struct PublicResolver;
impl toolkit_aws::endpoint::HostResolver for PublicResolver {
    fn resolve(&self, _host: &str) -> Result<Vec<std::net::IpAddr>, String> {
        Ok(vec!["93.184.216.34".parse().unwrap()])
    }
}

fn test_root() -> LoadedCerts {
    let pem = include_str!("data/mcp_proxy/test-root.pem");
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    let der = toolkit_core::encoding::base64_standard_decode(&body).expect("PEM body");
    LoadedCerts {
        certs: vec![der.into()],
        errors: 0,
    }
}

struct TableLoader;
impl PaymentRootLoader for TableLoader {
    fn load_paths(&self, _file: Option<&Path>, _dir: Option<&Path>) -> LoadedCerts {
        test_root()
    }
    fn native_store(&self) -> LoadedCerts {
        test_root()
    }
    fn exists(&self, path: &Path) -> bool {
        path == Path::new("/etc/ssl/certs/ca-certificates.crt")
    }
}

fn payment_roots() -> PaymentRoots {
    validate_payment_roots(
        &RootEnv::default(),
        &TableLoader,
        PaymentRootPlatform::current(),
    )
    .expect("payment roots")
}

#[test]
fn t_proots_use_every_payment_plane_shares_platform_roots() {
    let runtime = SdkRuntime::new().unwrap();
    let payment = payment_roots();
    let planes =
        wiring::PaymentPlanes::build(&runtime, &payment, Arc::new(PublicResolver)).unwrap();
    let merchant = planes.merchant.roots().expect("merchant roots");
    let agentcore = planes.agentcore.roots().expect("agentcore roots");
    assert!(merchant.same_allocation(payment.roots()));
    assert!(agentcore.same_allocation(payment.roots()));
    let provider_plane = planes.provider.roots().expect("provider roots");
    assert!(provider_plane.same_allocation(payment.roots()));
}

#[test]
fn live_fetch_and_admin_constructors_only_load_platform_payment_roots() {
    use syn::visit::Visit;
    struct Constructors(usize);
    impl<'ast> Visit<'ast> for Constructors {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(path) = call.func.as_ref() {
                let names: Vec<_> = path
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect();
                assert!(
                    !names
                        .iter()
                        .any(|s| s == "ValidatedRoots" || s == "RootEnv"),
                    "live payment constructor accepts environment roots: {names:?}"
                );
                if names == ["PaymentRoots", "from_process"] {
                    self.0 += 1;
                }
            }
            syn::visit::visit_expr_call(self, call);
        }
    }
    let source = syn::parse_file(include_str!("../src/agents_pay/live.rs")).unwrap();
    let mut constructors = Constructors(0);
    constructors.visit_file(&source);
    assert_eq!(
        constructors.0, 2,
        "fetch and admin must each load payment roots"
    );
}
