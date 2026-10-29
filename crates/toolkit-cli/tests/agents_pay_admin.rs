//! x402 S6 (#5462): the admin commands and `agents-build setup-payment-user`
//! over fixture `AdminDeps` (design section 7, R-RES-ADM, R-ADM-WRITE).
//!
//! Ported: `AdminRecipientMode`, `test_resource_resolution.py` (every
//! assert), `test_setup_openclaw.py` (`usd_to_atomic`,
//! `build_openclaw_config`, `discover_deployed` with `--project-dir`,
//! `format_duration`). New: T-TTY, T-TTY-CI-1..5 (D33), T-WRITE, T-VAL
//! (D29), T-PREFLIGHT, D27, D28, D30, D31. Nothing here opens a socket or
//! touches a path outside its temporary root; the D32 process side is in
//! `agents_pay_ac3.rs`.
#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::cell::Cell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use toolkit_cli::agents_pay::admin;
use toolkit_cli::agents_pay::admin::deployed::{self, Discovered};
use toolkit_cli::agents_pay::admin::fixtures::{
    CountingRandom, FakeAdmin, FakeEnv, FakeTerminal, TempAdminFs,
};
use toolkit_cli::agents_pay::admin::setup_openclaw::{
    build_openclaw_config, format_duration, parse_positive_decimal, usd_to_atomic, OpenClawInputs,
};
use toolkit_cli::agents_pay::admin::{preflight::leaked_names, write, AdminCommand, AdminDeps};
use toolkit_cli::agents_pay::agentcore::{AdminCall, FakePaymentsApi, PaymentError};
use toolkit_cli::agents_pay::deps::Unavailable;
use toolkit_cli::{route, Output, Routed, COMMANDS};
use toolkit_core::{parse_json, Json};

const MANAGER: &str =
    "arn:aws:bedrock-agentcore:us-east-1:111111111111:payment-manager/fixture-abcdefghij";
const CONNECTOR: &str = "connector-TESTabcdef";
const MERCHANT: &str = "0x1111111111111111111111111111111111111111";
const GENERATED_USER: &str = "agents-pay-000102030405";
const INSTRUMENT_ID: &str = "payment-instrument-TESTabcdefghijk";
const SESSION_ID: &str = "payment-session-TESTabcdefghijk";
const WALLET: &str = "0x9999999999999999999999999999999999999999";

fn instrument_response() -> Json {
    parse_json(&format!(
        r#"{{"paymentInstrument":{{"paymentInstrumentId":"{INSTRUMENT_ID}","paymentInstrumentDetails":{{"embeddedCryptoWallet":{{"walletAddress":"{WALLET}","redirectUrl":"https://delegate.test/start"}}}}}}}}"#
    ))
    .unwrap()
}

fn session_response() -> Json {
    parse_json(&format!(
        r#"{{"paymentSession":{{"paymentSessionId":"{SESSION_ID}"}}}}"#
    ))
    .unwrap()
}

fn fake_admin() -> FakeAdmin {
    FakeAdmin::new(
        FakePaymentsApi::with_session(session_response()).with_instrument(instrument_response()),
    )
}

/// One fixture world.
struct World {
    env: FakeEnv,
    fs: TempAdminFs,
    terminal: FakeTerminal,
    admin: FakeAdmin,
    random: CountingRandom,
}

impl World {
    fn new(label: &str, terminal: FakeTerminal) -> Self {
        Self::with_env(label, terminal, FakeEnv::new())
    }
    fn with_env(label: &str, terminal: FakeTerminal, env: FakeEnv) -> Self {
        Self {
            env,
            fs: TempAdminFs::new(label),
            terminal,
            admin: fake_admin(),
            random: CountingRandom::new(),
        }
    }
    fn deps(&self) -> AdminDeps<'_> {
        AdminDeps {
            env: &self.env,
            fs: &self.fs,
            terminal: &self.terminal,
            api: &self.admin,
            random: &self.random,
        }
    }
    /// The admin config of this world (`$HOME` is unset: the account home).
    fn config_path(&self) -> PathBuf {
        self.fs.home().join(".agents-pay/config.json")
    }
    fn work(&self, relative: &str) -> PathBuf {
        self.fs.root().join("work").join(relative)
    }
    fn config(&self) -> Json {
        parse_json(&fs::read_to_string(self.config_path()).unwrap()).unwrap()
    }
    fn write_config(&self, text: &str) {
        let path = self.config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn instruments(&self) -> usize {
        self.admin.api().instrument_create_calls()
    }
    fn sessions(&self) -> usize {
        self.admin.api().session_create_calls()
    }
}

struct Ran {
    code: u8,
    stdout: String,
    stderr: String,
}

/// Route `argv` through the real registry, then run the admin route over
/// the fixture world.
fn run(world: &World, argv: &[&str]) -> Ran {
    let words: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = match route(COMMANDS, &words) {
        Ok(Routed::Run(spec, invocation)) => {
            let command = AdminCommand::from_path(spec.path).expect("an admin route");
            let mut output = Output::with_diagnostics(&mut out, &mut err);
            match admin::run_with(command, &invocation, &world.deps(), &mut output) {
                Ok(()) => output.exit_status(),
                Err(error) => {
                    drop(output);
                    err.extend_from_slice(error.message().as_bytes());
                    error.exit_code()
                }
            }
        }
        Ok(_) => panic!("not a command: {argv:?}"),
        Err(error) => {
            err.extend_from_slice(error.message().as_bytes());
            error.exit_code()
        }
    };
    assert!(
        world.fs.refused().is_empty(),
        "a path outside the fixture root: {:?}",
        world.fs.refused()
    );
    Ran {
        code,
        stdout: String::from_utf8(out).unwrap(),
        stderr: String::from_utf8(err).unwrap(),
    }
}

fn get<'j>(value: &'j Json, path: &[&str]) -> Option<&'j Json> {
    path.iter().try_fold(value, |v, key| v.get(key))
}

fn text<'j>(value: &'j Json, path: &[&str]) -> Option<&'j str> {
    get(value, path).and_then(Json::as_str)
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn temp_files(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(write::TEMP_PREFIX) && n.ends_with(write::TEMP_SUFFIX))
        .collect()
}

// ------------------------------------------------- AdminRecipientMode (ported)

#[test]
fn admin_recipient_mode_recipient_flag_writes_an_allowlist() {
    let world = World::new("recipient", FakeTerminal::headless());
    let ran = run(
        &world,
        &[
            "agents-pay",
            "admin",
            "init-config",
            "--recipient",
            MERCHANT,
        ],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let config = world.config();
    let recipients = get(&config, &["policy", "allowed_recipients"]).unwrap();
    assert_eq!(recipients, &Json::Array(vec![Json::string(MERCHANT)]));
    assert!(get(&config, &["policy", "allow_any_recipient"]).is_none());
}

#[test]
fn admin_recipient_mode_allow_any_recipient_writes_explicit_mode() {
    let world = World::new("any", FakeTerminal::headless());
    let ran = run(
        &world,
        &[
            "agents-pay",
            "admin",
            "init-config",
            "--allow-any-recipient",
        ],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let config = world.config();
    assert_eq!(
        get(&config, &["policy", "allow_any_recipient"]),
        Some(&Json::Bool(true))
    );
    assert!(get(&config, &["policy", "allowed_recipients"]).is_none());
    assert!(ran
        .stdout
        .contains("WARNING: allow_any_recipient is enabled."));
}

#[test]
fn admin_recipient_mode_recipient_modes_cannot_be_combined() {
    let world = World::new("both", FakeTerminal::headless());
    let ran = run(
        &world,
        &[
            "agents-pay",
            "admin",
            "init-config",
            "--recipient",
            MERCHANT,
            "--allow-any-recipient",
        ],
    );
    assert_eq!(ran.code, 2);
    assert!(!world.config_path().exists());
}

#[test]
fn admin_recipient_mode_one_recipient_mode_is_required() {
    let world = World::new("none", FakeTerminal::headless());
    let ran = run(&world, &["agents-pay", "admin", "init-config"]);
    assert_eq!(ran.code, 2);
    assert!(!world.config_path().exists());
}

#[test]
fn init_config_writes_the_legacy_document_byte_for_byte_at_0600() {
    let world = World::new("bytes", FakeTerminal::headless());
    let ran = run(
        &world,
        &[
            "agents-pay",
            "admin",
            "init-config",
            "--recipient",
            MERCHANT,
        ],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let expected = format!(
        "{{\n  \"resources\": {{\n    \"user_id\": \"{GENERATED_USER}\"\n  }},\n  \"policy\": {{\n    \"max_per_payment_usd\": \"0.10\",\n    \"allowed_networks\": [\n      \"eip155:84532\"\n    ],\n    \"allowed_assets\": {{\n      \"eip155:84532\": [\n        \"0x036CbD53842c5426634e7929541eC2318f3dCF7e\"\n      ]\n    }},\n    \"allowed_schemes\": [\n      \"exact\"\n    ],\n    \"allowed_recipients\": [\n      \"{MERCHANT}\"\n    ]\n  }}\n}}\n"
    );
    assert_eq!(fs::read_to_string(world.config_path()).unwrap(), expected);
    assert_eq!(mode(&world.config_path()), 0o600);
    assert_eq!(mode(world.config_path().parent().unwrap()), 0o700);
    assert!(ran.stdout.starts_with(&format!(
        "Wrote config to {} (mode 0600, in a 0700 directory)\nPayer identity: {GENERATED_USER}  (generated \u{2014} single tenant, no need to pass it again)\n",
        world.config_path().display()
    )));
    assert!(ran.stdout.contains("No allowed_origins set"));
    assert_eq!(world.admin.calls(), 0, "init-config contacts no service");
}

#[test]
fn init_config_refuses_to_overwrite_and_force_keeps_the_payer() {
    let world = World::new("force", FakeTerminal::headless());
    world.write_config(r#"{"resources":{"user_id":"agents-pay-keepme","payment_instrument_id":"pi-1"},"policy":{"max_per_payment_usd":"9"}}"#);
    let before = fs::read_to_string(world.config_path()).unwrap();
    let ran = run(
        &world,
        &[
            "agents-pay",
            "admin",
            "init-config",
            "--allow-any-recipient",
        ],
    );
    assert_eq!(ran.code, 1);
    assert_eq!(
        ran.stderr,
        format!(
            "Refusing to overwrite existing policy at {} (pass --force).\n",
            world.config_path().display()
        )
    );
    assert_eq!(fs::read_to_string(world.config_path()).unwrap(), before);

    let ran = run(
        &world,
        &[
            "agents-pay",
            "admin",
            "init-config",
            "--allow-any-recipient",
            "--force",
            "--origin",
            "https://merchant.test",
            "--region",
            "us-west-2",
            "--max-per-payment-usd",
            "0.25",
        ],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let config = world.config();
    assert_eq!(
        text(&config, &["resources", "user_id"]),
        Some("agents-pay-keepme")
    );
    assert_eq!(
        text(&config, &["resources", "payment_instrument_id"]),
        Some("pi-1")
    );
    assert_eq!(text(&config, &["resources", "region"]), Some("us-west-2"));
    assert_eq!(
        text(&config, &["policy", "max_per_payment_usd"]),
        Some("0.25")
    );
    assert_eq!(
        get(&config, &["policy", "allowed_origins"]),
        Some(&Json::Array(vec![Json::string("https://merchant.test")]))
    );
    assert!(ran.stdout.contains("Payer identity: agents-pay-keepme\n"));
}

#[test]
fn init_config_refuses_an_unknown_network() {
    let world = World::new("network", FakeTerminal::headless());
    let ran = run(
        &world,
        &[
            "agents-pay",
            "admin",
            "init-config",
            "--allow-any-recipient",
            "--network",
            "eip155:1",
        ],
    );
    assert_eq!(ran.code, 1);
    assert_eq!(ran.stderr, "Unknown network eip155:1. Known: eip155:84532, eip155:8453. Add the exact USDC contract to KNOWN_USDC rather than passing one in.\n");
    assert!(!world.config_path().exists());
}

#[test]
fn init_config_seeds_the_manager_from_the_deploy_record() {
    let world = World::new("seed", FakeTerminal::headless());
    world.fs.put(
        "work/agentcore/.cli/deployed-state.json",
        &format!(r#"{{"payments":[{{"managerArn":"{MANAGER}"}}]}}"#),
    );
    let ran = run(
        &world,
        &[
            "agents-pay",
            "admin",
            "init-config",
            "--allow-any-recipient",
        ],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(
        text(&world.config(), &["resources", "payment_manager_arn"]),
        Some(MANAGER)
    );
}

// ------------------------------------- test_resource_resolution.py (ported)

fn resources_file(world: &World, name: &str, resources: &str) -> PathBuf {
    world.fs.put(
        &format!("work/{name}"),
        &format!(r#"{{"resources": {resources}, "policy": {{}}}}"#),
    )
}

#[test]
fn resolve_region_prefers_flag_then_env_then_config_then_none() {
    let world = World::new("region", FakeTerminal::headless());
    let cfg = resources_file(&world, "region-config.json", r#"{"region": "us-east-1"}"#);
    let deps = world.deps();
    assert_eq!(
        admin::resolve_region(Some("eu-west-1"), &cfg, &deps).as_deref(),
        Some("eu-west-1"),
        "flag should win"
    );
    let env = FakeEnv::new().with("AWS_REGION", "ap-southeast-2");
    let with_env = AdminDeps { env: &env, ..deps };
    assert_eq!(
        admin::resolve_region(None, &cfg, &with_env).as_deref(),
        Some("ap-southeast-2"),
        "env should win over config"
    );
    let default = FakeEnv::new().with("AWS_DEFAULT_REGION", "sa-east-1");
    let with_default = AdminDeps {
        env: &default,
        ..deps
    };
    assert_eq!(
        admin::resolve_region(None, &cfg, &with_default).as_deref(),
        Some("sa-east-1")
    );
    assert_eq!(
        admin::resolve_region(None, &cfg, &deps).as_deref(),
        Some("us-east-1"),
        "config region should be used, not hardcoded us-west-2"
    );
    let empty = resources_file(&world, "region-empty.json", "{}");
    assert_eq!(
        admin::resolve_region(None, &empty, &deps),
        None,
        "should fall through to None, not us-west-2"
    );
}

#[test]
fn resolve_manager_arn_prefers_flag_then_env_then_config_then_deployed_state() {
    let world = World::new("arn", FakeTerminal::headless());
    let saved = "arn:aws:bedrock-agentcore:us-east-1:111111111111:payment-manager/config-saved";
    let cfg = resources_file(
        &world,
        "arn-config.json",
        &format!(r#"{{"payment_manager_arn": "{saved}"}}"#),
    );
    let deps = world.deps();
    assert_eq!(
        admin::resolve_manager_arn(Some("arn:flag"), &cfg, &deps).as_deref(),
        Some("arn:flag"),
        "flag should win"
    );
    let env = FakeEnv::new().with("PAYMENT_MANAGER_ARN", "arn:env");
    assert_eq!(
        admin::resolve_manager_arn(None, &cfg, &AdminDeps { env: &env, ..deps }).as_deref(),
        Some("arn:env"),
        "env should win over config"
    );
    assert_eq!(
        admin::resolve_manager_arn(None, &cfg, &deps).as_deref(),
        Some(saved),
        "config.json's payment_manager_arn must not be ignored"
    );
    let empty = resources_file(&world, "arn-empty.json", "{}");
    assert_eq!(
        admin::resolve_manager_arn(None, &empty, &deps),
        None,
        "should fall through to None when nothing is configured or discoverable"
    );
    world.fs.put(
        "work/.cli/deployed-state.json",
        r#"{"payments":{"p":{"managerArn":"arn:deployed"}}}"#,
    );
    assert_eq!(
        admin::resolve_manager_arn(None, &empty, &deps).as_deref(),
        Some("arn:deployed"),
        "the deploy record is the last step"
    );
}

#[test]
fn resolve_user_id_and_config_path_keep_the_legacy_chains() {
    let world = World::new("user", FakeTerminal::headless());
    let cfg = resources_file(&world, "u.json", r#"{"user_id": "agents-pay-config"}"#);
    let env = FakeEnv::new()
        .with("PAYMENT_USER_ID", "agents-pay-env")
        .with("AGENTS_PAY_CONFIG", "/env/config.json")
        .with("HOME", "/home/someone");
    let deps = world.deps();
    let with_env = AdminDeps { env: &env, ..deps };
    assert_eq!(
        admin::resolve_user_id(Some("flag"), &cfg, &with_env).as_deref(),
        Some("flag")
    );
    assert_eq!(
        admin::resolve_user_id(None, &cfg, &with_env).as_deref(),
        Some("agents-pay-config")
    );
    let empty = resources_file(&world, "u-empty.json", "{}");
    assert_eq!(
        admin::resolve_user_id(None, &empty, &with_env).as_deref(),
        Some("agents-pay-env")
    );
    assert_eq!(
        admin::admin_config_path(Some("/flag/c.json"), &with_env),
        PathBuf::from("/flag/c.json")
    );
    assert_eq!(
        admin::admin_config_path(None, &with_env),
        PathBuf::from("/env/config.json")
    );
    let home_only = FakeEnv::new().with("HOME", "/home/someone");
    assert_eq!(
        admin::admin_config_path(
            None,
            &AdminDeps {
                env: &home_only,
                ..deps
            }
        ),
        PathBuf::from("/home/someone/.agents-pay/config.json")
    );
    assert_eq!(admin::admin_config_path(None, &deps), world.config_path());
}

// --------------------------------------- test_setup_openclaw.py (ported)

#[test]
fn usd_to_atomic_is_exact_and_rejects_excess_precision() {
    let atomic = |text: &str| usd_to_atomic(&parse_positive_decimal(text, "Max").unwrap());
    assert_eq!(atomic("0.10").as_deref(), Ok("100000"));
    assert_eq!(atomic("0.000001").as_deref(), Ok("1"));
    let error = atomic("0.0000001").unwrap_err();
    assert!(error.contains("at most 6 decimal places"), "{error}");
    assert_eq!(atomic("5").as_deref(), Ok("5000000"));
    assert_eq!(atomic("1e-6").as_deref(), Ok("1"));
    assert_eq!(atomic("0.0000010").as_deref(), Ok("1"));
    assert_eq!(
        parse_positive_decimal("0", "Max").unwrap_err(),
        "Max must be greater than zero."
    );
    assert_eq!(
        parse_positive_decimal("ten", "Max").unwrap_err(),
        "Max must be a decimal number."
    );
}

#[test]
fn generated_config_preserves_policy_inputs() {
    let config = build_openclaw_config(&OpenClawInputs {
        region: "us-east-1".into(),
        manager_arn: "arn:manager".into(),
        instrument_id: "instrument-1".into(),
        session_id: "session-1".into(),
        user_id: "user-1".into(),
        network: "eip155:84532".into(),
        asset: "0xasset".into(),
        max_payment_atomic: "100000".into(),
        recipients: vec!["0xmerchant".into()],
        allow_any: false,
        origins: vec!["https://sandbox.example".into()],
        return_body: true,
    });
    let plugin = get(&config, &["plugins", "entries", "aws-agents-pay", "config"]).unwrap();
    assert_eq!(text(plugin, &["maxPaymentAmountAtomic"]), Some("100000"));
    assert_eq!(
        plugin.get("allowedOrigins"),
        Some(&Json::Array(vec![Json::string("https://sandbox.example")]))
    );
    assert_eq!(
        plugin.get("allowedRecipients"),
        Some(&Json::Array(vec![Json::string("0xmerchant")]))
    );
    assert_eq!(plugin.get("returnBody"), Some(&Json::Bool(true)));
    assert!(plugin.get("allowAnyRecipient").is_none());
    assert_eq!(
        get(&config, &["plugins", "allow"]),
        Some(&Json::Array(vec![Json::string("aws-agents-pay")]))
    );
    assert_eq!(
        get(
            &config,
            &["plugins", "entries", "aws-agents-pay", "enabled"]
        ),
        Some(&Json::Bool(true))
    );
}

#[test]
fn explicit_project_dir_finds_agentcore_deployment() {
    let world = World::new("project", FakeTerminal::headless());
    world.fs.put(
        "project/agentcore/.cli/deployed-state.json",
        r#"{"targets":{"default":{"resources":{"payments":{"payments":{"managerArn":"arn:manager","connectors":{"connector":{"connectorId":"connector-1"}}}}}}}}"#,
    );
    let project = world.fs.root().join("project");
    assert_eq!(
        deployed::discover(&world.deps(), Some(project.to_str().unwrap())),
        Discovered {
            manager_arn: Some("arn:manager".into()),
            connector_id: Some("connector-1".into()),
            role_arn: None,
        }
    );
    // Relative to the working directory, as Path.resolve() gives it.
    assert_eq!(
        deployed::discover(&world.deps(), Some("../project"))
            .manager_arn
            .as_deref(),
        Some("arn:manager")
    );
}

#[test]
fn duration_review_includes_hours_for_full_hours() {
    assert_eq!(format_duration(1440), "1440 minutes (24 hours)");
    assert_eq!(format_duration(90), "90 minutes");
    assert_eq!(format_duration(60), "60 minutes (1 hour)");
}

// ------------------------------------------------- deployed-state layouts

#[test]
fn deployed_state_reads_every_layout_and_fails_soft() {
    let full = |manager: &str, connector: Option<&str>, role: Option<&str>| Discovered {
        manager_arn: Some(manager.to_owned()),
        connector_id: connector.map(str::to_owned),
        role_arn: role.map(str::to_owned),
    };
    // 0.20.x: targets.<t>.resources.payments[].
    assert_eq!(
        deployed::parse(
            r#"{"targets":{"prod":{"resources":{"payments":[{"managerArn":"arn:a","connectors":[{"connectorId":"c-1"}],"processPaymentRoleArn":"arn:role"}]}}}}"#
        ),
        full("arn:a", Some("c-1"), Some("arn:role"))
    );
    // 0.26.x: keyed by name.
    assert_eq!(
        deployed::parse(
            r#"{"targets":{"default":{"resources":{"payments":{"main":{"managerArn":"arn:b","connectors":{"x":{"connectorId":"c-2"}}}}}}}}"#
        ),
        full("arn:b", Some("c-2"), None)
    );
    // Legacy top-level payments[].
    assert_eq!(
        deployed::parse(r#"{"payments":[{"managerArn":"arn:c"}]}"#),
        full("arn:c", None, None)
    );
    // Empty target payments fall back to the top level.
    assert_eq!(
        deployed::parse(
            r#"{"targets":{"default":{"resources":{"payments":[]}}},"payments":[{"managerArn":"arn:d"}]}"#
        ),
        full("arn:d", None, None)
    );
    // A later step that raises keeps what was already read (legacy partial).
    assert_eq!(
        deployed::parse(r#"{"payments":[{"managerArn":"arn:e","connectors":["not-a-dict"]}]}"#),
        Discovered {
            manager_arn: Some("arn:e".into()),
            connector_id: None,
            role_arn: None,
        }
    );
    for broken in [
        "not json",
        "[]",
        r#"{"targets":["x"]}"#,
        r#"{"payments":"x"}"#,
        r#"{"payments":["x"]}"#,
        r#"{"targets":{"default":{"resources":"x"}}}"#,
    ] {
        assert_eq!(deployed::parse(broken), Discovered::default(), "{broken}");
    }
}

#[test]
fn deployed_state_candidates_and_first_existing() {
    let world = World::new("candidates", FakeTerminal::headless());
    let deps = world.deps();
    assert_eq!(
        deployed::candidates(None, &deps),
        vec![
            PathBuf::from("agentcore/.cli/deployed-state.json"),
            PathBuf::from(".cli/deployed-state.json"),
            PathBuf::from("../.cli/deployed-state.json"),
        ]
    );
    let env = FakeEnv::new().with("AGENTCORE_PROJECT_DIR", "/srv/project");
    assert_eq!(
        deployed::candidates(None, &AdminDeps { env: &env, ..deps }),
        vec![
            PathBuf::from("/srv/project/agentcore/.cli/deployed-state.json"),
            PathBuf::from("/srv/project/.cli/deployed-state.json"),
        ]
    );
    // From inside agentcore/: the parent's .cli is the third candidate.
    world.fs.put(
        ".cli/deployed-state.json",
        r#"{"payments":[{"managerArn":"arn:parent"}]}"#,
    );
    assert_eq!(
        deployed::discover(&deps, None).manager_arn.as_deref(),
        Some("arn:parent")
    );
    world.fs.put(
        "work/.cli/deployed-state.json",
        r#"{"payments":[{"managerArn":"arn:here"}]}"#,
    );
    assert_eq!(
        deployed::discover(&deps, None).manager_arn.as_deref(),
        Some("arn:here")
    );
}

// ------------------------------------------------------------- T-WRITE

#[test]
fn t_write_creates_owner_only_modes_and_leaves_no_temporary() {
    let world = World::new("write", FakeTerminal::headless());
    let target = world.fs.root().join("a/b/c/config.json");
    write::atomic_write_0600(&target, "{}\n").unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "{}\n");
    assert_eq!(mode(&target), 0o600);
    assert_eq!(mode(target.parent().unwrap()), 0o700);
    assert!(temp_files(target.parent().unwrap()).is_empty());

    // An existing loose directory and file are tightened.
    let loose = world.fs.root().join("loose");
    fs::create_dir_all(&loose).unwrap();
    fs::set_permissions(&loose, fs::Permissions::from_mode(0o755)).unwrap();
    let file = loose.join("config.json");
    fs::write(&file, "old").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    write::atomic_write_0600(&file, "new").unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "new");
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&loose), 0o700);
}

#[test]
fn t_write_removes_the_temporary_file_on_failure() {
    let world = World::new("write-fail", FakeTerminal::headless());
    let dir = world.fs.root().join("cfg");
    let target = dir.join("config.json");
    write::atomic_write_0600(&target, "old").unwrap();
    let mut seen = None;
    let error = write::atomic_write_0600_with(&target, "new", |temp| {
        assert_eq!(fs::read_to_string(temp).unwrap(), "new");
        assert_eq!(mode(temp), 0o600, "the temporary file is owner-only");
        seen = Some(temp.to_path_buf());
        Err(std::io::Error::other("injected"))
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "injected");
    assert!(!seen.unwrap().exists());
    assert!(temp_files(&dir).is_empty());
    assert_eq!(fs::read_to_string(&target).unwrap(), "old");

    // A rename that fails (the target is a non-empty directory).
    let blocked = dir.join("blocked.json");
    fs::create_dir_all(blocked.join("inside")).unwrap();
    assert!(write::atomic_write_0600(&blocked, "x").is_err());
    assert!(temp_files(&dir).is_empty());
}

#[test]
fn t_write_refuses_a_symlinked_final_directory() {
    let world = World::new("write-link", FakeTerminal::headless());
    let real = world.fs.root().join("real");
    fs::create_dir_all(&real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
    let link = world.fs.root().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(write::atomic_write_0600(&link.join("config.json"), "x").is_err());
    assert!(fs::read_dir(&real).unwrap().next().is_none());
    assert_eq!(mode(&real), 0o755, "the link target is not chmodded");
}

#[test]
fn t_write_admin_updates_merge_resources_and_keep_the_policy() {
    let world = World::new("merge", FakeTerminal::headless());
    world.write_config(
        r#"{"resources":{"user_id":"u","region":"r"},"policy":{"max_per_payment_usd":"1"}}"#,
    );
    let deps = world.deps();
    admin::update_resources(
        &world.config_path(),
        &[
            ("payment_session_id", Some("s-1")),
            ("region", None),
            ("user_id", Some("")),
        ],
        &deps,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(world.config_path()).unwrap(),
        "{\n  \"resources\": {\n    \"user_id\": \"u\",\n    \"region\": \"r\",\n    \"payment_session_id\": \"s-1\"\n  },\n  \"policy\": {\n    \"max_per_payment_usd\": \"1\"\n  }\n}\n"
    );
    // A flat legacy policy is lifted into `policy`; invalid JSON is the skeleton.
    world.write_config(r#"{"max_per_payment_usd":"2"}"#);
    let raw = admin::load_raw_config(&world.config_path(), &deps);
    assert_eq!(text(&raw, &["policy", "max_per_payment_usd"]), Some("2"));
    world.write_config("{not json");
    let raw = admin::load_raw_config(&world.config_path(), &deps);
    assert_eq!(
        admin::config_text(&raw),
        "{\n  \"resources\": {},\n  \"policy\": {}\n}\n"
    );
}

// --------------------------------------------- show-config and preflight

#[test]
fn show_config_without_a_file_explains_and_exits_1() {
    let world = World::new("show-none", FakeTerminal::headless());
    let ran = run(&world, &["agents-pay", "admin", "show-config"]);
    assert_eq!(ran.code, 1);
    assert_eq!(
        ran.stdout,
        format!(
            "No config at {}. Payments are refused until one exists.\nCreate it with: toolkit agents-pay admin init-config --max-per-payment-usd 0.05 --recipient <payTo> (or explicitly use --allow-any-recipient)\n",
            world.config_path().display()
        )
    );
}

#[test]
fn show_config_reports_modes_owner_and_missing_resources() {
    let world = World::new("show", FakeTerminal::headless());
    world
        .write_config(r#"{"resources":{"user_id":"u-1"},"policy":{"max_per_payment_usd":"0.05"}}"#);
    let ran = run(&world, &["agents-pay", "admin", "show-config"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        ran.stdout.contains("File mode : -rw------- OK\n"),
        "{}",
        ran.stdout
    );
    assert!(
        ran.stdout.contains("Dir mode  : drwx------ OK\n"),
        "{}",
        ran.stdout
    );
    assert!(ran.stdout.contains(") (you)\n") || ran.stdout.contains(" (you)\n"));
    assert!(ran.stdout.contains(
        "\n--- policy (what MAY be paid) ---\n{\n  \"max_per_payment_usd\": \"0.05\"\n}\n"
    ));
    assert!(ran.stdout.contains(
        "\nNot yet set: payment_manager_arn, payment_instrument_id, payment_session_id\n"
    ));
    fs::set_permissions(world.config_path(), fs::Permissions::from_mode(0o644)).unwrap();
    let ran = run(&world, &["agents-pay", "admin", "show-config"]);
    assert!(ran
        .stdout
        .contains("File mode : -rw-r--r-- *** TOO OPEN \u{2014} chmod 600 ***\n"));
}

#[test]
fn t_preflight_secret_name_scan_matches_upper_cased_substrings() {
    let names: Vec<String> = [
        "HOME",
        "my_wallet_secret_x",
        "CDP_API_KEY_SECRET",
        "PRIVY_APP_SECRET",
        "X_AUTHORIZATION_PRIVATE_KEY_Y",
        "AWS_SECRET_ACCESS_KEY",
        "APP_SECRETS_DIR",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    assert_eq!(
        leaked_names(&names),
        vec![
            "APP_SECRETS_DIR",
            "CDP_API_KEY_SECRET",
            "PRIVY_APP_SECRET",
            "X_AUTHORIZATION_PRIVATE_KEY_Y",
            "my_wallet_secret_x",
        ]
    );
}

#[test]
fn t_preflight_fails_on_a_secret_named_variable_and_never_prints_its_value() {
    let env = FakeEnv::new()
        .with("PAYMENT_MANAGER_ARN", MANAGER)
        .with("PRIVY_APP_SECRET", "super-secret-value-123456");
    let world = World::with_env("preflight-leak", FakeTerminal::headless(), env);
    world.write_config(r#"{"resources":{},"policy":{"max_per_payment_usd":"0.05"}}"#);
    let ran = run(&world, &["agents-pay", "admin", "preflight"]);
    assert_eq!(ran.code, 1);
    assert!(ran
        .stdout
        .contains("[ok]   policy loaded, max_per_payment_usd=0.05\n"));
    assert!(ran.stdout.contains("[ok]   PAYMENT_MANAGER_ARN\n"));
    assert!(ran.stdout.contains("[--]   PAYMENT_SESSION_ID not set\n"));
    assert!(ran
        .stdout
        .contains("[FAIL] provider secrets present in this environment: PRIVY_APP_SECRET\n"));
    assert!(ran.stdout.ends_with("\nPreflight FAILED\n"));
    assert!(!ran.stdout.contains("super-secret-value"));
    assert!(!ran.stderr.contains("super-secret-value"));
    assert_eq!(world.admin.calls(), 0);
}

#[test]
fn t_preflight_passes_with_a_checked_policy_and_hints_the_deployed_manager() {
    let world = World::new("preflight-ok", FakeTerminal::headless());
    world.write_config(r#"{"resources":{},"policy":{"max_per_payment_usd":"0.05"}}"#);
    world.fs.put(
        "work/agentcore/.cli/deployed-state.json",
        &format!(r#"{{"payments":[{{"managerArn":"{MANAGER}"}}]}}"#),
    );
    let ran = run(&world, &["agents-pay", "admin", "preflight"]);
    assert_eq!(ran.code, 0, "{}", ran.stdout);
    assert!(ran.stdout.contains(&format!(
        "\n       Found in agentcore/.cli/deployed-state.json: run this to fix the above ->\n         export PAYMENT_MANAGER_ARN={MANAGER}\n"
    )));
    assert!(ran
        .stdout
        .contains("[ok]   no provider secrets in the runtime environment\n"));
    assert!(ran.stdout.ends_with("\nPreflight PASSED\n"));
    // The runtime checks apply: a group-readable config fails.
    fs::set_permissions(world.config_path(), fs::Permissions::from_mode(0o640)).unwrap();
    let ran = run(&world, &["agents-pay", "admin", "preflight"]);
    assert_eq!(ran.code, 1);
    assert!(ran.stdout.contains("[FAIL] policy: "), "{}", ran.stdout);
}

// ------------------------------------------------- T-TTY (new-session)

const NEW_SESSION: &[&str] = &[
    "agents-pay",
    "admin",
    "new-session",
    "--budget",
    "1.00",
    "--expiry-minutes",
    "30",
    "--manager-arn",
    MANAGER,
    "--user-id",
    "agents-pay-u1",
];

#[test]
fn t_tty_new_session_checks_the_terminal_first() {
    for argv in [NEW_SESSION, &["agents-pay", "admin", "new-session"][..]] {
        let world = World::new("tty-first", FakeTerminal::headless());
        let world = World {
            admin: fake_admin().sts_failing(PaymentError::Service("AccessDeniedException")),
            ..world
        };
        let ran = run(&world, argv);
        assert_eq!(ran.code, 1, "{argv:?}");
        assert_eq!(
            ran.stderr,
            "Refusing to create a payment session without an interactive terminal.\nSession creation requires a human typing 'approve' at a TTY. There is no\nnon-interactive mode: that would let an automated caller mint spending budget.\n"
        );
        assert!(ran.stdout.is_empty());
        assert_eq!(world.admin.calls(), 0);
        assert_eq!(world.terminal.reads(), 0);
    }
}

#[test]
fn t_tty_new_session_needs_exactly_approve() {
    for answer in ["yes", "Approve", "approve ", " approve", "", "approved"] {
        let world = World::new("tty-exact", FakeTerminal::tty(&[answer]));
        let ran = run(&world, NEW_SESSION);
        assert_eq!(ran.code, 1, "{answer:?}");
        assert!(
            ran.stdout.ends_with("Aborted. No session created.\n"),
            "{answer:?}"
        );
        assert_eq!(world.sessions(), 0, "{answer:?}");
    }
}

#[test]
fn t_tty_new_session_eof_aborts_d28() {
    let world = World::new("tty-eof", FakeTerminal::tty(&[]));
    let ran = run(&world, NEW_SESSION);
    assert_eq!(ran.code, 1);
    assert!(ran.stdout.ends_with("Aborted. No session created.\n"));
    assert_eq!(world.sessions(), 0);
    assert_eq!(world.terminal.reads(), 1);
}

#[test]
fn t_tty_new_session_approve_creates_one_session_and_records_it() {
    let world = World::new("tty-approve", FakeTerminal::tty(&["approve"]));
    let ran = run(&world, NEW_SESSION);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(ran.stdout.starts_with(&format!(
        "About to create a payment session:\n  manager   : {MANAGER}\n  payer     : agents-pay-u1\n  budget    : 1.00 USD (hard cap for the whole session)\n  expires in: 30 minutes\nType 'approve' to continue: \nPayment session created: {SESSION_ID}\n"
    )), "{}", ran.stdout);
    let calls = world.admin.api().admin_calls();
    let [AdminCall::CreateSession {
        manager,
        user,
        budget,
        expiry,
        token,
    }] = calls.as_slice()
    else {
        panic!("{calls:?}");
    };
    assert_eq!(
        (manager.as_str(), user.as_str(), budget.as_str(), *expiry),
        (MANAGER, "agents-pay-u1", "1.00", 30)
    );
    assert_eq!(
        token, "00010203-0405-4607-8809-0a0b0c0d0e0f",
        "a UUID4 clientToken"
    );
    assert_eq!(world.admin.sts_calls(), 1);
    let config = world.config();
    assert_eq!(
        text(&config, &["resources", "payment_session_id"]),
        Some(SESSION_ID)
    );
    assert_eq!(
        text(&config, &["resources", "user_id"]),
        Some("agents-pay-u1")
    );
}

#[test]
fn t_tty_no_creating_route_declares_a_bypass_flag() {
    let creating: [&[&str]; 4] = [
        &["agents-pay", "admin", "new-session"],
        &["agents-pay", "admin", "setup-openclaw"],
        &["agents-pay", "admin", "create-instrument"],
        &["agents-build", "setup-payment-user"],
    ];
    for path in creating {
        let spec = COMMANDS
            .iter()
            .find(|spec| spec.path == path)
            .unwrap_or_else(|| panic!("{path:?} is not registered"));
        for flag in spec
            .flags
            .iter()
            .chain(spec.value_flags)
            .chain(spec.repeatable_flags)
        {
            for banned in ["yes", "force", "non-interactive", "assume"] {
                assert!(!flag.contains(banned), "{path:?} declares {flag}");
            }
        }
    }
}

#[test]
fn t_tty_setup_openclaw_checks_the_terminal_first() {
    let world = World::new("wizard-tty", FakeTerminal::headless());
    let ran = run(
        &world,
        &[
            "agents-pay",
            "admin",
            "setup-openclaw",
            "--project-dir",
            "/nonexistent",
        ],
    );
    assert_eq!(ran.code, 1);
    assert_eq!(
        ran.stderr,
        "setup-openclaw requires an interactive terminal.\n"
    );
    assert_eq!(world.admin.calls(), 0);
    assert_eq!(world.terminal.reads(), 0);
}

// ------------------------- #5475: TTY first, before the process world

/// The roots failure a broken process world reports (a nonexistent
/// `SSL_CERT_FILE`, say): a headless creating route must never reach it.
const ROOTS_FAILURE: &str = "fixture roots failure: SSL_CERT_FILE does not exist";

impl admin::AdminWorldSource for &World {
    fn deps(&self) -> AdminDeps<'_> {
        World::deps(self)
    }
}

/// Route `argv` and run it through the process wrapper the live registry
/// handlers use: `terminal` gates it, and `factory` stands for the process
/// world (`AdminWorld::for_process` in a live build).
fn run_process<W: admin::AdminWorldSource>(
    terminal: &FakeTerminal,
    argv: &[&str],
    factory: impl FnOnce() -> Result<W, Unavailable>,
) -> Ran {
    let words: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = match route(COMMANDS, &words) {
        Ok(Routed::Run(spec, invocation)) => {
            let command = AdminCommand::from_path(spec.path).expect("an admin route");
            let mut output = Output::with_diagnostics(&mut out, &mut err);
            match admin::run_process(command, &invocation, terminal, factory, &mut output) {
                Ok(()) => output.exit_status(),
                Err(error) => {
                    drop(output);
                    err.extend_from_slice(error.message().as_bytes());
                    error.exit_code()
                }
            }
        }
        Ok(_) => panic!("not a command: {argv:?}"),
        Err(error) => {
            err.extend_from_slice(error.message().as_bytes());
            error.exit_code()
        }
    };
    Ran {
        code,
        stdout: String::from_utf8(out).unwrap(),
        stderr: String::from_utf8(err).unwrap(),
    }
}

/// A process-world factory that counts its calls and fails like broken
/// trust roots.
fn failing_factory(built: &Cell<usize>) -> impl FnOnce() -> Result<&'static World, Unavailable> + '_ {
    move || {
        built.set(built.get() + 1);
        Err(Unavailable(ROOTS_FAILURE.to_owned()))
    }
}

#[test]
fn t_tty_5475_headless_creating_routes_never_build_the_process_world() {
    let new_session = "Refusing to create a payment session without an interactive terminal.\nSession creation requires a human typing 'approve' at a TTY. There is no\nnon-interactive mode: that would let an automated caller mint spending budget.\n";
    let instrument =
        "create-instrument must be run by a human at a terminal; it creates a wallet user.\n";
    let wizard = "setup-openclaw requires an interactive terminal.\n";
    let payment_user =
        "setup-payment-user must be run by a human at a terminal; it creates a wallet user.\n";
    let cases: [(&[&str], &str); 8] = [
        (NEW_SESSION, new_session),
        (&["agents-pay", "admin", "new-session"], new_session),
        (CREATE_INSTRUMENT, instrument),
        (&["agents-pay", "admin", "create-instrument"], instrument),
        (&["agents-pay", "admin", "setup-openclaw"], wizard),
        (
            &["agents-pay", "admin", "setup-openclaw", "--project-dir", "/nonexistent"],
            wizard,
        ),
        (SETUP_PAYMENT_USER, payment_user),
        (&["agents-build", "setup-payment-user"], payment_user),
    ];
    for (argv, refusal) in cases {
        let terminal = FakeTerminal::headless();
        let built = Cell::new(0);
        let ran = run_process(&terminal, argv, failing_factory(&built));
        assert_eq!(built.get(), 0, "the process world was built: {argv:?}");
        assert_eq!(ran.code, 1, "{argv:?}");
        assert_eq!(ran.stderr, refusal, "{argv:?}");
        assert!(ran.stdout.is_empty(), "{argv:?}");
        assert_eq!(terminal.reads(), 0, "{argv:?}");
    }
}

#[test]
fn t_tty_5475_a_terminal_creating_route_reports_the_world_failure() {
    for argv in [NEW_SESSION, CREATE_INSTRUMENT, SETUP_PAYMENT_USER] {
        let terminal = FakeTerminal::tty(&["approve"]);
        let built = Cell::new(0);
        let ran = run_process(&terminal, argv, failing_factory(&built));
        assert_eq!(built.get(), 1, "{argv:?}");
        assert_eq!(ran.code, 1, "{argv:?}");
        assert_eq!(ran.stderr, format!("{ROOTS_FAILURE}\n"), "{argv:?}");
        assert!(ran.stdout.is_empty(), "{argv:?}");
        assert_eq!(terminal.reads(), 0, "{argv:?}");
    }
}

#[test]
fn t_tty_5475_read_only_routes_need_no_terminal() {
    for argv in [
        &["agents-pay", "admin", "show-config"][..],
        &["agents-pay", "admin", "preflight"][..],
    ] {
        let terminal = FakeTerminal::headless();
        let built = Cell::new(0);
        let ran = run_process(&terminal, argv, failing_factory(&built));
        assert_eq!(built.get(), 1, "{argv:?}");
        assert_eq!(ran.code, 1, "{argv:?}");
        assert_eq!(ran.stderr, format!("{ROOTS_FAILURE}\n"), "{argv:?}");
    }
}

#[test]
fn t_tty_5475_the_wrapper_runs_the_route_over_the_built_world() {
    let world = World::new("process-approve", FakeTerminal::tty(&["approve"]));
    let built = Cell::new(0);
    let ran = run_process(&world.terminal, NEW_SESSION, || {
        built.set(built.get() + 1);
        Ok::<_, Unavailable>(&world)
    });
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(built.get(), 1);
    assert_eq!(world.sessions(), 1);
    assert_eq!(world.terminal.reads(), 1);
    assert!(world.fs.refused().is_empty(), "{:?}", world.fs.refused());
}

// ------------------------------------------------------------- T-VAL (D29)

#[test]
fn t_val_budget_and_expiry_are_checked_before_any_call_or_prompt() {
    let cases: [(&str, &str, &str); 6] = [
        ("0", "30", "--budget must be greater than zero.\n"),
        ("abc", "30", "--budget must be a decimal number.\n"),
        ("Infinity", "30", "--budget must be a decimal number.\n"),
        ("1", "14", "--expiry-minutes must be between 15 and 480.\n"),
        ("1", "481", "--expiry-minutes must be between 15 and 480.\n"),
        ("1", "0", "--expiry-minutes must be between 15 and 480.\n"),
    ];
    for (budget, expiry, message) in cases {
        let world = World::new("val", FakeTerminal::tty(&["approve"]));
        let ran = run(
            &world,
            &[
                "agents-pay",
                "admin",
                "new-session",
                "--budget",
                budget,
                "--expiry-minutes",
                expiry,
                "--manager-arn",
                MANAGER,
                "--user-id",
                "u",
            ],
        );
        assert_eq!(ran.code, 1, "{budget} {expiry}");
        assert_eq!(ran.stderr, message, "{budget} {expiry}");
        assert_eq!(world.admin.calls(), 0, "{budget} {expiry}");
        assert_eq!(world.terminal.reads(), 0, "{budget} {expiry}");
    }
}

#[test]
fn t_val_boundaries_and_the_capdecimal_rendering_reach_the_service() {
    for (budget, expiry, sent) in [
        ("1e1", "15", "10"),
        ("0.50", "480", "0.50"),
        ("+2", "60", "2"),
    ] {
        let world = World::new("val-ok", FakeTerminal::tty(&["approve"]));
        let ran = run(
            &world,
            &[
                "agents-pay",
                "admin",
                "new-session",
                "--budget",
                budget,
                "--expiry-minutes",
                expiry,
                "--manager-arn",
                MANAGER,
                "--user-id",
                "u",
            ],
        );
        assert_eq!(ran.code, 0, "{}", ran.stderr);
        let calls = world.admin.api().admin_calls();
        assert!(
            matches!(calls.as_slice(), [AdminCall::CreateSession { budget: b, expiry: e, .. }] if b == sent && e.to_string() == expiry),
            "{calls:?}"
        );
    }
}

#[test]
fn t_val_malformed_arguments_are_usage_errors_after_the_terminal_check() {
    let world = World::new("val-usage", FakeTerminal::tty(&["approve"]));
    let ran = run(
        &world,
        &[
            "agents-pay",
            "admin",
            "new-session",
            "--budget",
            "1",
            "--expiry-minutes",
            "ten",
        ],
    );
    assert_eq!(ran.code, 2);
    assert!(ran
        .stderr
        .contains("argument --expiry-minutes: invalid int value: 'ten'"));
    let ran = run(&world, &["agents-pay", "admin", "new-session"]);
    assert_eq!(ran.code, 2);
    assert!(ran
        .stderr
        .contains("the following arguments are required: --budget"));
    assert_eq!(world.admin.calls(), 0);
}

// ---------------------------------------- T-TTY-CI-1..5 (D33): both routes

const CREATE_INSTRUMENT: &[&str] = &[
    "agents-pay",
    "admin",
    "create-instrument",
    "--email",
    "user@example.com",
    "--user-id",
    "agents-pay-u1",
    "--manager-arn",
    MANAGER,
    "--connector-id",
    CONNECTOR,
];

const SETUP_PAYMENT_USER: &[&str] = &[
    "agents-build",
    "setup-payment-user",
    "--user-id",
    "alice",
    "--email",
    "user@example.com",
    "--manager-arn",
    MANAGER,
    "--connector-id",
    CONNECTOR,
];

fn with(base: &[&'static str], extra: &[&'static str]) -> Vec<&'static str> {
    base.iter().chain(extra).copied().collect()
}

#[test]
fn t_tty_ci_1_headless_refuses_with_zero_calls_whatever_the_arguments() {
    let cases: [(&[&str], &str); 4] = [
        (
            CREATE_INSTRUMENT,
            "create-instrument must be run by a human at a terminal; it creates a wallet user.\n",
        ),
        (
            &["agents-pay", "admin", "create-instrument"],
            "create-instrument must be run by a human at a terminal; it creates a wallet user.\n",
        ),
        (
            SETUP_PAYMENT_USER,
            "setup-payment-user must be run by a human at a terminal; it creates a wallet user.\n",
        ),
        (
            &[
                "agents-build",
                "setup-payment-user",
                "--expiry-minutes",
                "ten",
            ],
            "setup-payment-user must be run by a human at a terminal; it creates a wallet user.\n",
        ),
    ];
    for (argv, refusal) in cases {
        let world = World::new("ci-1", FakeTerminal::headless());
        let world = World {
            admin: fake_admin().sts_failing(PaymentError::Transport(
                toolkit_cli::agents_pay::agentcore::PaymentErrorClass::NoCredentialsError,
            )),
            ..world
        };
        let ran = run(&world, argv);
        assert_eq!(ran.code, 1, "{argv:?}");
        assert_eq!(ran.stderr, refusal, "{argv:?}");
        assert!(ran.stdout.is_empty(), "{argv:?}");
        assert_eq!(world.admin.calls(), 0, "{argv:?}");
        assert_eq!(world.terminal.reads(), 0, "{argv:?}");
    }
}

#[test]
fn t_tty_ci_2_anything_but_approve_aborts_with_zero_creations() {
    for argv in [CREATE_INSTRUMENT, SETUP_PAYMENT_USER] {
        for answer in ["yes", "Approve", "approve "] {
            let world = World::new("ci-2", FakeTerminal::tty(&[answer]));
            let ran = run(&world, argv);
            assert_eq!(ran.code, 1, "{argv:?} {answer:?}");
            assert!(
                ran.stdout.ends_with("Aborted. No instrument created.\n"),
                "{argv:?} {answer:?}: {}",
                ran.stdout
            );
            assert_eq!(world.instruments(), 0);
            assert_eq!(world.sessions(), 0);
        }
    }
}

#[test]
fn t_tty_ci_3_eof_aborts_with_zero_creations() {
    for argv in [CREATE_INSTRUMENT, SETUP_PAYMENT_USER] {
        let world = World::new("ci-3", FakeTerminal::tty(&[]));
        let ran = run(&world, argv);
        assert_eq!(ran.code, 1, "{argv:?}");
        assert!(ran.stdout.ends_with("Aborted. No instrument created.\n"));
        assert_eq!(world.instruments() + world.sessions(), 0);
    }
}

#[test]
fn t_tty_ci_4_approve_creates_one_instrument_and_a_session_only_with_budget() {
    let world = World::new("ci-4a", FakeTerminal::tty(&["approve"]));
    let ran = run(&world, CREATE_INSTRUMENT);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!((world.instruments(), world.sessions()), (1, 0));
    let calls = world.admin.api().admin_calls();
    assert!(
        matches!(
            calls.as_slice(),
            [AdminCall::CreateInstrument { manager, user, connector, network, email, .. }]
                if manager == MANAGER && user == "agents-pay-u1" && connector == CONNECTOR
                    && network == "ETHEREUM" && email == "user@example.com"
        ),
        "{calls:?}"
    );
    let config = world.config();
    assert_eq!(
        text(&config, &["resources", "payment_instrument_id"]),
        Some(INSTRUMENT_ID)
    );
    assert_eq!(
        text(&config, &["resources", "payment_manager_arn"]),
        Some(MANAGER)
    );
    assert!(ran.stdout.contains(&format!(
        "Instrument created : {INSTRUMENT_ID}\nWallet address     : {WALLET}\n"
    )));
    assert!(ran.stdout.contains(&format!(
        "  1. Delegation: visit https://delegate.test/start, sign in, grant access to {WALLET}\n"
    )));
    assert!(ran.stdout.ends_with("\nThen authorize spending:\n  toolkit agents-pay admin new-session --budget 1.00 --expiry-minutes 120\n"));

    let world = World::new("ci-4b", FakeTerminal::tty(&["approve"]));
    let ran = run(&world, SETUP_PAYMENT_USER);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!((world.instruments(), world.sessions()), (1, 0));
    assert!(ran
        .stdout
        .contains("Session ID    : (none - use `agentcore invoke --auto-session`)\n"));
    assert!(
        !world.config_path().exists(),
        "setup-payment-user writes no config"
    );

    let world = World::new("ci-4c", FakeTerminal::tty(&["approve"]));
    let ran = run(
        &world,
        &with(
            SETUP_PAYMENT_USER,
            &["--budget", "5", "--expiry-minutes", "120"],
        ),
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!((world.instruments(), world.sessions()), (1, 1));
    assert_eq!(world.terminal.reads(), 1, "one approval covers both");
    let calls = world.admin.api().admin_calls();
    assert!(
        matches!(
            calls.as_slice(),
            [AdminCall::CreateInstrument { .. }, AdminCall::CreateSession { budget, expiry: 120, user, .. }]
                if budget == "5" && user == "alice"
        ),
        "{calls:?}"
    );
    assert_eq!(
        world.admin.opened(),
        vec![Some("us-east-1".to_owned())],
        "the legacy region default"
    );
    assert!(ran.stdout.contains(&format!(
        "Instrument ID : {INSTRUMENT_ID}\nWallet address: {WALLET}\nSession ID    : {SESSION_ID}\n\nExport these for the x402 tool (Step 8):\n  export PAYMENT_MANAGER_ARN={MANAGER}\n  export PAYMENT_INSTRUMENT_ID={INSTRUMENT_ID}\n  export PAYMENT_SESSION_ID={SESSION_ID}\n  export PAYMENT_USER_ID=alice\n  export AWS_REGION=us-east-1\n"
    )), "{}", ran.stdout);
}

#[test]
fn t_tty_ci_5_the_summary_shows_the_email_bounded_and_nothing_from_the_response() {
    let long = format!("{}@example.com", "a".repeat(300));
    let long: &'static str = Box::leak(long.into_boxed_str());
    for argv in [CREATE_INSTRUMENT, SETUP_PAYMENT_USER] {
        let argv: Vec<&str> = argv
            .iter()
            .map(|a| if *a == "user@example.com" { long } else { a })
            .collect();
        let world = World::new("ci-5", FakeTerminal::tty(&["no"]));
        let ran = run(&world, &argv);
        assert_eq!(ran.code, 1);
        assert!(!ran.stdout.contains(long), "the raw email is not echoed");
        assert!(ran
            .stdout
            .contains(&format!("  email     : {}...\n", &long[..200])));
        assert!(!ran.stdout.contains(WALLET) && !ran.stdout.contains(INSTRUMENT_ID));
        assert!(ran.stdout.contains("Type 'approve' to continue: "));
        assert_eq!(world.instruments(), 0);
    }
}

#[test]
fn t_tty_ci_validates_the_network_family_before_any_call() {
    let world = World::new("ci-net", FakeTerminal::tty(&["approve"]));
    let ran = run(
        &world,
        &with(CREATE_INSTRUMENT, &["--network-family", "BITCOIN"]),
    );
    assert_eq!(ran.code, 1);
    assert_eq!(ran.stderr, "--network-family must be ETHEREUM or SOLANA.\n");
    let ran = run(
        &world,
        &with(SETUP_PAYMENT_USER, &["--network", "ethereum"]),
    );
    assert_eq!(ran.code, 1);
    assert_eq!(world.admin.calls(), 0);
    let ran = run(&world, &with(SETUP_PAYMENT_USER, &["--budget", "0"]));
    assert_eq!(ran.stderr, "--budget must be greater than zero.\n");
    assert_eq!(world.admin.calls(), 0);
}

// ------------------------------------------------------------ D27, D31

#[test]
fn d27_setup_payment_user_reads_the_dict_layout_from_its_single_candidate() {
    let world = World::new("d27", FakeTerminal::tty(&["approve"]));
    world.fs.put(
        "work/agentcore/.cli/deployed-state.json",
        &format!(r#"{{"targets":{{"default":{{"resources":{{"payments":{{"main":{{"managerArn":"{MANAGER}","connectors":{{"c":{{"connectorId":"{CONNECTOR}"}}}}}}}}}}}}}}}}"#),
    );
    let ran = run(
        &world,
        &[
            "agents-build",
            "setup-payment-user",
            "--user-id",
            "alice",
            "--email",
            "a@example.com",
        ],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(matches!(
        world.admin.api().admin_calls().as_slice(),
        [AdminCall::CreateInstrument { manager, connector, .. }] if manager == MANAGER && connector == CONNECTOR
    ));

    // Only the single candidate: a record one level up is not read.
    let world = World::new("d27-single", FakeTerminal::tty(&["approve"]));
    world.fs.put(
        ".cli/deployed-state.json",
        &format!(r#"{{"payments":[{{"managerArn":"{MANAGER}","connectors":[{{"connectorId":"{CONNECTOR}"}}]}}]}}"#),
    );
    let ran = run(
        &world,
        &[
            "agents-build",
            "setup-payment-user",
            "--user-id",
            "alice",
            "--email",
            "a@example.com",
        ],
    );
    assert_eq!(ran.code, 1);
    assert_eq!(ran.stderr, "Could not resolve manager ARN / connector ID. Pass --manager-arn and --connector-id, or run from the project dir with agentcore/.cli/deployed-state.json present.\n");
    assert_eq!(world.admin.calls(), 0);
}

#[test]
fn d31_a_service_failure_prints_only_the_allowlisted_code() {
    let world = World::new("d31", FakeTerminal::tty(&["approve"]));
    let world = World {
        admin: fake_admin().create_failing(PaymentError::Service("ValidationException")),
        ..world
    };
    let ran = run(&world, CREATE_INSTRUMENT);
    assert_eq!(ran.code, 1);
    assert_eq!(
        ran.stderr,
        "CreatePaymentInstrument failed: ValidationException\n"
    );
    assert!(!world.config_path().exists());

    let world = World::new("d31-sts", FakeTerminal::tty(&["approve"]));
    let world = World {
        admin: fake_admin().sts_failing(PaymentError::Service("ExpiredTokenException")),
        ..world
    };
    let ran = run(&world, NEW_SESSION);
    assert_eq!(ran.code, 1);
    assert_eq!(
        ran.stderr,
        "AWS credentials are invalid, expired, or missing.\n  (ExpiredTokenException)\n\nRun `aws sso login` (or otherwise refresh your credentials), then re-run this command.\n"
    );
    assert_eq!(
        world.terminal.reads(),
        0,
        "the STS check precedes the prompt"
    );
    assert_eq!(world.sessions(), 0);
}

#[test]
fn create_instrument_without_a_payer_or_manager_refuses_after_the_sts_check() {
    let world = World::new("no-payer", FakeTerminal::tty(&["approve"]));
    let ran = run(
        &world,
        &[
            "agents-pay",
            "admin",
            "create-instrument",
            "--email",
            "a@example.com",
        ],
    );
    assert_eq!(ran.code, 1);
    assert_eq!(ran.stderr, "No payer identity found. Run init-config first (it generates one), or pass --user-id explicitly.\n");
    assert_eq!(world.admin.sts_calls(), 1);
    let ran = run(
        &world,
        &[
            "agents-pay",
            "admin",
            "create-instrument",
            "--email",
            "a@example.com",
            "--user-id",
            "u",
        ],
    );
    assert_eq!(ran.code, 1);
    assert!(ran.stderr.starts_with("Could not find deployed-state.json (checked: agentcore/.cli/deployed-state.json, .cli/deployed-state.json, ../.cli/deployed-state.json)."));
    assert_eq!(world.instruments(), 0);
    assert_eq!(world.terminal.reads(), 0);
}

// --------------------------------------------- setup-openclaw (the wizard)

const WIZARD_ANSWERS: &[&str] = &[
    "",                      // user id: generate
    "",                      // region: us-east-1
    "",                      // network: eip155:84532
    "1",                     // allowlist
    MERCHANT,                // payTo
    "",                      // end of recipients
    "0.05",                  // per-payment cap
    "5.00",                  // budget
    "120",                   // expiry
    "https://merchant.test", // origin
    "",                      // end of origins
    "n",                     // return body
    "user@example.com",      // email
    "",                      // delegation done
    "approve",               // session approval
];

#[test]
fn setup_openclaw_wizard_writes_creates_and_prints_the_plugin_config() {
    let world = World::new("wizard", FakeTerminal::tty(WIZARD_ANSWERS));
    world.fs.put(
        "project/agentcore/.cli/deployed-state.json",
        &format!(r#"{{"payments":[{{"managerArn":"{MANAGER}","connectors":[{{"connectorId":"{CONNECTOR}"}}]}}]}}"#),
    );
    let ran = run(
        &world,
        &[
            "agents-pay",
            "admin",
            "setup-openclaw",
            "--project-dir",
            "../project",
        ],
    );
    assert_eq!(ran.code, 0, "{}\n{}", ran.stderr, ran.stdout);
    assert_eq!(world.terminal.reads(), WIZARD_ANSWERS.len());
    assert!(
        ran.stdout
            .contains("Session expiry in minutes (15-480; 480 = 8 hours) [120]: "),
        "D30"
    );
    assert!(ran
        .stdout
        .contains(&format!("  Generated: {GENERATED_USER}\n")));
    assert!(ran
        .stdout
        .contains("  $0.05 USD = 50000 atomic units (USDC, 6 decimals)\n"));
    assert!(ran
        .stdout
        .contains("    Expiry:          120 minutes (2 hours)\n"));
    let config = world.config();
    assert_eq!(
        text(&config, &["resources", "user_id"]),
        Some(GENERATED_USER)
    );
    assert_eq!(
        text(&config, &["resources", "payment_manager_arn"]),
        Some(MANAGER)
    );
    assert_eq!(
        text(&config, &["resources", "payment_instrument_id"]),
        Some(INSTRUMENT_ID)
    );
    assert_eq!(
        text(&config, &["resources", "payment_session_id"]),
        Some(SESSION_ID)
    );
    assert_eq!(
        text(&config, &["policy", "max_per_payment_usd"]),
        Some("0.05")
    );
    assert_eq!(
        get(&config, &["policy", "return_body"]),
        Some(&Json::Bool(false))
    );
    let calls = world.admin.api().admin_calls();
    assert!(
        matches!(
            calls.as_slice(),
            [AdminCall::CreateInstrument { network, email, .. }, AdminCall::CreateSession { budget, expiry: 120, .. }]
                if network == "ETHEREUM" && email == "user@example.com" && budget == "5.00"
        ),
        "{calls:?}"
    );
    let start = ran
        .stdout
        .find("\n{\n  \"plugins\"")
        .expect("the plugin JSON");
    let end = ran.stdout.rfind("\n}\n").unwrap() + 2;
    let plugin = parse_json(&ran.stdout[start + 1..end]).unwrap();
    let inner = get(&plugin, &["plugins", "entries", "aws-agents-pay", "config"]).unwrap();
    assert_eq!(text(inner, &["maxPaymentAmountAtomic"]), Some("50000"));
    assert_eq!(text(inner, &["payment_session_id"]), Some(SESSION_ID));
    assert_eq!(inner.get("returnBody"), Some(&Json::Bool(false)));
    assert!(ran
        .stdout
        .ends_with("\nThen restart OpenClaw to activate the plugin.\n"));
}

#[test]
fn setup_openclaw_eof_aborts_before_anything_is_created_d28() {
    let world = World::new("wizard-eof", FakeTerminal::tty(&["", ""]));
    let ran = run(&world, &["agents-pay", "admin", "setup-openclaw"]);
    assert_eq!(ran.code, 1);
    assert!(ran.stdout.ends_with("Aborted. No session created.\n"));
    assert_eq!(world.instruments() + world.sessions(), 0);
    assert!(!world.config_path().exists());
}

#[test]
fn setup_openclaw_rejects_an_expiry_the_service_refuses_d29() {
    let mut answers = WIZARD_ANSWERS.to_vec();
    answers[8] = "1440";
    let world = World::new("wizard-expiry", FakeTerminal::tty(&answers));
    let ran = run(&world, &["agents-pay", "admin", "setup-openclaw"]);
    assert_eq!(ran.code, 1);
    assert_eq!(ran.stderr, "Expiry minutes must be between 15 and 480.\n");
    assert_eq!(world.instruments() + world.sessions(), 0);
    let mut answers = WIZARD_ANSWERS.to_vec();
    answers[6] = "6";
    let world = World::new("wizard-cap", FakeTerminal::tty(&answers));
    let ran = run(&world, &["agents-pay", "admin", "setup-openclaw"]);
    assert_eq!(ran.code, 1);
    assert!(ran
        .stderr
        .starts_with("Max per-payment USD cannot exceed the cumulative session budget."));
}

#[test]
fn setup_openclaw_refuses_a_missing_project_directory() {
    let world = World::new("wizard-dir", FakeTerminal::tty(&[]));
    let ran = run(
        &world,
        &[
            "agents-pay",
            "admin",
            "setup-openclaw",
            "--project-dir",
            "missing",
        ],
    );
    assert_eq!(ran.code, 1);
    assert_eq!(
        ran.stderr,
        format!(
            "AgentCore project directory does not exist: {}\n",
            world.work("missing").display()
        )
    );
    assert_eq!(world.admin.calls(), 0);
}
