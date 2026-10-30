//! Payment documentation selects delivered release artifacts. Offline only:
//! source text and stand-in binaries, never a live build or payment.
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{repo_root, temp_dir, write};
use plugin_stage::stage::{install, BuildStep, Builder, Built, Provenance};
use plugin_stage::{Declaration, Destination, Role, DECLARATION_PATH};

const OPENCLAW: &str = "plugins/aws-agents/skills/agents-pay/packages/openclaw";

#[cfg(unix)]
#[test]
fn exact_token_skill_preflight_accepts_a_delivered_root_without_environment() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = temp_dir("delivered-payment-root with spaces");
    let binary = fixture.join("libexec/toolkit");
    write(
        &binary,
        "#!/bin/sh\n[ \"$1\" = help ] || exit 92\nprintf 'delivered-help\\n'\n",
    );
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let text =
        fs::read_to_string(repo_root().join("plugins/aws-agents/skills/agents-pay/SKILL.md"))
            .unwrap();
    let block = text
        .split("```bash\n")
        .find(|block| block.starts_with("if ["))
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let block = block
        .split("npm install")
        .next()
        .unwrap()
        .replace("${CLAUDE_PLUGIN_ROOT}", fixture.to_str().unwrap());
    let output = std::process::Command::new("/bin/sh")
        .env_clear()
        .args(["-c", &block])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("delivered-help"));
}

fn markdown(dir: &Path, paths: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("skills").map(Result::unwrap) {
        let path = entry.path();
        if path.is_dir() {
            if !matches!(
                path.file_name().and_then(|s| s.to_str()),
                Some("test" | "tests" | "fixtures" | "node_modules" | "bin" | "libexec")
            ) {
                markdown(&path, paths);
            }
        } else if path.extension().is_some_and(|ext| {
            matches!(ext.to_str(), Some("md" | "rs" | "ts" | "js" | "mjs" | "sh"))
        }) {
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
        let (problems, count) = invocation_problems(&text, &path);
        assert!(problems.is_empty(), "{}: {problems:#?}", path.display());
        examples += count;
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
    let bin = plugins.join("aws-agents/libexec");
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
        for root in [None, Some(Path::new("")), Some(&*fixture)] {
            let mut command = std::process::Command::new("/bin/sh");
            command.env_clear().env("PATH", &*fixture);
            if let Some(root) = root {
                command.env("CLAUDE_PLUGIN_ROOT", root);
            }
            let output = command.args(["-c", block]).output().unwrap();
            assert!(!output.status.success(), "{page}");
            assert!(output.stdout.is_empty());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("Payment dispatcher unavailable")
                    || String::from_utf8_lossy(&output.stderr).contains("Set CLAUDE_PLUGIN_ROOT")
            );
            assert!(!marker.exists(), "a flattened install must never try PATH");
        }
    }
}

// Inspect executable Markdown spans/fences and runtime source, not prose.
// This is deliberately a documentation contract, not a general shell parser.
fn snippets(text: &str, markdown: bool) -> Vec<String> {
    if !markdown {
        return vec![text.to_string()];
    }
    let mut result = Vec::new();
    let mut fence = false;
    let mut block = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if fence {
                result.push(std::mem::take(&mut block));
            }
            fence = !fence;
        } else if fence {
            block.push_str(line);
            block.push('\n');
        } else {
            let mut parts = line.split('`');
            parts.next();
            while let Some(code) = parts.next() {
                result.push(code.to_string());
                parts.next();
            }
        }
    }
    if fence {
        result.push(block);
    }
    result
}

// Retain quote type: a single-quoted plugin-root path cannot expand in Bash.
fn tokens(text: &str) -> Vec<(String, Option<char>)> {
    let normalized = text.replace("\\\r\n", "").replace("\\\n", "");
    let mut chars = normalized.chars().peekable();
    let mut result = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() || "()[],;=|&".contains(c) {
            continue;
        }
        let quote = matches!(c, '\'' | '"').then_some(c);
        let mut word = String::new();
        if let Some(q) = quote {
            for c in chars.by_ref() {
                if c == q {
                    break;
                }
                word.push(c);
            }
        } else {
            word.push(c);
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || "()[],;=|&\"'".contains(c) {
                    break;
                }
                word.push(c);
                chars.next();
            }
        }
        result.push((word, quote));
    }
    result
}

fn invocation_problems(text: &str, path: &Path) -> (Vec<String>, usize) {
    let mut problems = Vec::new();
    let mut count = 0;
    for code in snippets(text, path.extension().is_some_and(|ext| ext == "md")) {
        let words = tokens(&code);
        for (i, (word, quote)) in words.iter().enumerate() {
            let name = word.rsplit('/').next().unwrap_or(word);
            if !matches!(name, "toolkit" | "toolkit.exe") {
                continue;
            }
            let rest: Vec<_> = words[i + 1..].iter().map(|(w, _)| w.as_str()).collect();
            let route_at = rest.iter().position(|w| !w.starts_with('-'));
            let route = route_at.and_then(|i| rest.get(i)).copied();
            // A named executable string in code is an invocation even when its
            // arguments are indirect (assignments, Command::new, argv, spawn).
            let bare_literal =
                quote.is_some() && matches!(word.as_str(), "toolkit" | "toolkit.exe");
            let command = (matches!(word.as_str(), "toolkit" | "toolkit.exe") && words.len() > 1)
                || (path.extension().is_some_and(|ext| ext == "md")
                    && word.contains("/toolkit")
                    && !rest.is_empty())
                || route.is_some_and(|r| {
                    matches!(
                        r,
                        "agents-pay"
                            | "agents-build"
                            | "help"
                            | "ai-models"
                            | "documentdb"
                            | "startup"
                            | "ai-payloads"
                    )
                })
                || (quote.is_none() && code.trim_start().starts_with(word));
            if !command && !bare_literal {
                continue;
            }
            count += 1;
            let plugin = word == "${CLAUDE_PLUGIN_ROOT}/libexec/toolkit"
                || word == "${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit";
            let standalone_page = path.ends_with("references/openclaw-setup.md")
                || path.ends_with("packages/openclaw/README.md");
            let package = (standalone_page && word == "./bin/toolkit")
                || word == "~/.openclaw/extensions/aws-agents-pay/bin/toolkit";
            if bare_literal || !(plugin || package) || (plugin && *quote != Some('"')) {
                problems.push(format!(
                    "dispatcher must use the delivered explicit path: {word}"
                ));
            }
            let allowed = route == Some("agents-pay")
                || route == Some("help")
                || (route == Some("agents-build")
                    && route_at.and_then(|i| rest.get(i + 1)).copied()
                        == Some("setup-payment-user"));
            if !allowed {
                problems.push(format!(
                    "aws-agents permits payment routes and help only: {word}"
                ));
            }
        }
    }
    (problems, count)
}

#[test]
fn seeded_command_spellings_are_refused_without_flagging_prose() {
    let path = Path::new("references/regression.md");
    for fixture in include_str!("fixtures/payment-invocations.txt").split("\n---\n") {
        let text = format!("```text\n{fixture}\n```\n");
        assert!(
            !invocation_problems(&text, path).0.is_empty(),
            "missed {fixture}"
        );
    }
    for text in [
        "The toolkit binary enforces policy.",
        "The `toolkit` binary enforces policy.",
        "AWS toolkit documentation.",
        "The CDK toolkit stack.",
    ] {
        assert!(
            invocation_problems(text, path).0.is_empty(),
            "prose: {text}"
        );
    }
    for command in [
        "\"${CLAUDE_PLUGIN_ROOT}/libexec/toolkit\" agents-pay fetch URL",
        "\"${CLAUDE_PLUGIN_ROOT}/libexec/toolkit\" agents-build setup-payment-user",
        "\"${CLAUDE_PLUGIN_ROOT}/libexec/toolkit\" help",
        "\"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit\" help",
    ] {
        assert!(invocation_problems(&format!("`{command}`"), path).0.is_empty(), "{command}");
    }
    // The same spellings in runtime source files are covered too.
    for fixture in include_str!("fixtures/payment-invocations.txt").split("\n---\n") {
        assert!(
            !invocation_problems(fixture, Path::new("scripts/regression.sh"))
                .0
                .is_empty(),
            "source: {fixture}"
        );
    }
}
