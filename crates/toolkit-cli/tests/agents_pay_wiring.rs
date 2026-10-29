//! T-PROOTS-USE (x402 S5 #5352, design R-TLS-PAY "Which roots each plane
//! uses"): the merchant and `AgentCore` data-plane transports trust the very
//! `PaymentRoots` allocation the wiring was given, and provider HTTP trusts
//! the S1 provider roots. Roots come from injected loaders over a checked-in
//! test certificate; nothing is opened or sent (G4).
#![cfg(feature = "aws-sdk")]
#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::path::Path;
use std::sync::Arc;

use toolkit_aws::sdk::roots::{
    validate_roots, LoadedCerts, RootEnv, RootLoader, RootPlatform, ValidatedRoots,
};
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

struct ProviderLoader;
impl RootLoader for ProviderLoader {
    fn load_file(&self, _path: &Path) -> LoadedCerts {
        test_root()
    }
    fn load_dir(&self, _path: &Path) -> LoadedCerts {
        LoadedCerts::default()
    }
    fn load_native(&self) -> LoadedCerts {
        test_root()
    }
    fn load_probe(&self) -> LoadedCerts {
        test_root()
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

fn provider_roots() -> ValidatedRoots {
    validate_roots(
        &RootEnv {
            file: Some("/fixture/provider.pem".into()),
            dir: None,
        },
        &ProviderLoader,
        RootPlatform::current(),
    )
    .expect("provider roots")
}

#[test]
fn t_proots_use_proof_planes_share_the_payment_roots_and_providers_keep_s1_roots() {
    let runtime = SdkRuntime::new().unwrap();
    let payment = payment_roots();
    let provider = provider_roots();
    let planes =
        wiring::PaymentPlanes::build(&runtime, &payment, &provider, Arc::new(PublicResolver))
            .unwrap();
    let merchant = planes.merchant.roots().expect("merchant roots");
    let agentcore = planes.agentcore.roots().expect("agentcore roots");
    assert!(merchant.same_allocation(payment.roots()));
    assert!(agentcore.same_allocation(payment.roots()));
    let provider_plane = planes.provider.roots().expect("provider roots");
    assert!(provider_plane.same_allocation(&provider));
    assert!(!provider_plane.same_allocation(payment.roots()));
    assert!(!merchant.same_allocation(&provider));
}
