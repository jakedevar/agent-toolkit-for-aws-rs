//! Payment documentation selects delivered release artifacts. Offline only:
//! source text and stand-in binaries, never a live build or payment.
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{repo_root, temp_dir, write};
use plugin_stage::stage::{install, BuildStep, Builder, Built, Provenance};
use plugin_stage::{Declaration, Destination, Role, DECLARATION_PATH};

const OPENCLAW: &str = "plugins/aws-agents/skills/agents-pay/packages/openclaw";

fn markdown(dir: &Path, paths: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("skills").map(Result::unwrap) {
        let path = entry.path();
        if path.is_dir() {
            markdown(&path, paths);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            paths.push(path);
        }
    }
}

#[test]
fn every_payment_example_selects_a_delivered_payment_binary() {
    let root = repo_root();
    let declaration = Declaration::load(&root.join(DECLARATION_PATH)).unwrap();
    let plugin = declaration.binary("aws-agents", "toolkit").unwrap();
    assert_eq!(plugin.destination, Destination::Plugin("aws-agents".into()));
    assert_eq!(plugin.features, ["agents-pay-live"]);
    assert_eq!(plugin.role, Role::Subprocess);
    let package = declaration.package_binaries(OPENCLAW).next().unwrap();
    assert_eq!(package.features, ["agents-pay-live"]);

    let mut pages = Vec::new();
    markdown(&root.join("plugins/aws-agents/skills"), &mut pages);
    let mut examples = 0;
    for path in pages {
        let text = fs::read_to_string(&path).unwrap();
        for (offset, _) in text.match_indices("toolkit") {
            let rest = text[offset + "toolkit".len()..].trim_start_matches('"');
            let route = rest.trim_start();
            if !rest.starts_with(char::is_whitespace)
                || !(route.starts_with("agents-pay") || route.starts_with("agents-build"))
            {
                continue;
            }
            examples += 1;
            let prefix = &text[..offset];
            let plugin_path = prefix.ends_with("\"${CLAUDE_PLUGIN_ROOT}/bin/");
            let standalone_page = path.ends_with("references/openclaw-setup.md")
                || path.ends_with("packages/openclaw/README.md");
            let package_path = (standalone_page && prefix.ends_with("./bin/"))
                || prefix.ends_with("~/.openclaw/extensions/aws-agents-pay/bin/");
            assert!(
                plugin_path || package_path,
                "{}: payment example at byte {offset} must select the delivered dispatcher, never PATH",
                path.display()
            );
            if route.starts_with("agents-build") {
                assert_eq!(
                    route
                        .split_whitespace()
                        .nth(1)
                        .map(|word| word.trim_end_matches(['`', ')', '.', ',', ':', ';'])),
                    Some("setup-payment-user")
                );
            }
        }
        // Catch executable argv/Command forms too, including the review's
        // original bare argv regression, even though they are not shell lines.
        for bare in ["[\"toolkit\",", "Command::new(\"toolkit\")"] {
            assert!(!text.contains(bare), "{}: bare {bare}", path.display());
        }
    }
    assert!(examples >= 90, "non-vacuous inventory: {examples}");
    let snippet = fs::read_to_string(
        root.join("plugins/aws-agents/skills/agents-build/references/payments.md"),
    )
    .unwrap();
    for required in [
        "Command::new(payment_binary)",
        "!payment_binary.is_absolute()",
        "!payment_binary.is_file()",
        "command.args([\"agents-pay\", \"fetch\", url])",
        "kill_on_drop(true)",
        "from_secs(120)",
        "\"refused\":true",
    ] {
        assert!(
            snippet.contains(required),
            "missing snippet contract: {required}"
        );
    }
}

#[test]
fn payment_plugin_dispatcher_is_in_the_plugin_release_layout() {
    let declaration = Declaration::load(&repo_root().join(DECLARATION_PATH)).unwrap();
    let binary = declaration.binary("aws-agents", "toolkit").unwrap();
    let fixture = temp_dir("payment-release-layout");
    let plugins = fixture.join("plugins");
    fs::create_dir_all(plugins.join("aws-agents")).unwrap();
    let built: Vec<_> = declaration
        .platforms
        .iter()
        .map(|platform| {
            let file = fixture.join(format!("out/{}/toolkit", platform.platform.key()));
            write(&file, "offline stand-in, never executed");
            Built {
                step: BuildStep {
                    binary: binary.clone(),
                    platform: platform.clone(),
                    builder: Builder::Zigbuild,
                },
                file,
            }
        })
        .collect();
    let manifest = install(
        &plugins,
        "aws-agents",
        &built,
        &Provenance {
            source_commit: "0".repeat(40),
            source_dirty: false,
            rustc: "offline fixture".into(),
        },
        false,
    )
    .unwrap();
    assert_eq!(manifest.destination, binary.destination);
    let bin = plugins.join("aws-agents/bin");
    for relative in [
        "toolkit",
        "toolkit.exe",
        "linux-x86_64/toolkit",
        "linux-aarch64/toolkit",
        "macos-x86_64/toolkit",
        "macos-aarch64/toolkit",
        "manifest.json",
    ] {
        assert!(
            bin.join(relative).is_file(),
            "missing release file {relative}"
        );
    }
    let manifest = fs::read_to_string(bin.join("manifest.json")).unwrap();
    assert!(manifest.contains("agents-pay-live"));
    assert!(!manifest.contains("package_root"));
}

#[cfg(unix)]
#[test]
fn flattened_skill_preflight_refuses_without_trying_path() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = temp_dir("flattened-payment-skill");
    let marker = fixture.join("wrong-toolkit-ran");
    let wrong = fixture.join("toolkit");
    write(&wrong, &format!("#!/bin/sh\n : > '{}'\n", marker.display()));
    fs::set_permissions(&wrong, fs::Permissions::from_mode(0o755)).unwrap();
    for page in [
        "plugins/aws-agents/skills/agents-pay/SKILL.md",
        "plugins/aws-agents/skills/agents-pay/references/setup.md",
    ] {
        let text = fs::read_to_string(repo_root().join(page)).unwrap();
        let block = text
            .split("```bash\n")
            .find(|block| block.starts_with("if [ -z"))
            .expect("installation preflight")
            .split("```")
            .next()
            .unwrap();
        for root in [None, Some(&*fixture)] {
            let mut command = std::process::Command::new("/bin/sh");
            command.env_clear().env("PATH", &*fixture);
            if let Some(root) = root {
                command.env("CLAUDE_PLUGIN_ROOT", root);
            }
            let output = command.args(["-c", block]).output().unwrap();
            assert_eq!(output.status.code(), Some(1), "{page}");
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8(output.stderr).unwrap().contains(
                "Payment dispatcher unavailable; install the full aws-agents plugin release"
            ));
            assert!(!marker.exists(), "a flattened install must never try PATH");
        }
    }
}
