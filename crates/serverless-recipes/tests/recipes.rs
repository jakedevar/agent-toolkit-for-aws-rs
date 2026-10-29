//! Contract tests for the deployment recipes of the Rust serverless examples.
//!
//! They read the repository tree, never AWS or Docker: they pin what the
//! recipes promise (no Python left, Rust artifacts built and shipped, a
//! digest-pinned unprivileged image, a shipped lock file that matches the
//! manifest, an identical plugin copy).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const POWERTOOLS: &str = "skills/core-skills/aws-serverless/assets/powertools-handler";
const AWS_SERVERLESS: &str = "skills/core-skills/aws-serverless";
const AWS_SERVERLESS_PLUGIN: &str = "plugins/aws-core/skills/aws-serverless";
const PROCESSING: &str =
    "skills/specialized-skills/serverless-skills/processing-s3-uploads-with-step-functions";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every file under `relative`, as (path relative to it, bytes).
fn tree(relative: &str) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let name = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(name, fs::read(&path).unwrap());
            }
        }
    }
    let base = root().join(relative);
    let mut out = BTreeMap::new();
    walk(&base, &base, &mut out);
    out
}

#[test]
fn no_python_remains_for_the_serverless_examples() {
    for dir in [AWS_SERVERLESS, AWS_SERVERLESS_PLUGIN, PROCESSING] {
        for name in tree(dir).keys() {
            let file = name.rsplit('/').next().unwrap();
            assert!(
                !Path::new(file)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("py")),
                "{dir}/{name} is Python"
            );
            assert!(
                !file.starts_with("requirements") && file != "Pipfile" && file != "pyproject.toml",
                "{dir}/{name} is a Python requirements file"
            );
        }
    }
    let dockerfile = read(&format!("{PROCESSING}/scripts/Dockerfile")).to_lowercase();
    for word in ["python", "pip ", "pip3", "requirements"] {
        assert!(
            !dockerfile.contains(word),
            "the Dockerfile still mentions {word:?}"
        );
    }
}

#[test]
fn the_plugin_copy_of_the_aws_serverless_skill_is_byte_identical() {
    let canonical = tree(AWS_SERVERLESS);
    let bundled = tree(AWS_SERVERLESS_PLUGIN);
    assert_eq!(
        canonical.keys().collect::<Vec<_>>(),
        bundled.keys().collect::<Vec<_>>(),
        "file lists differ; run `toolkit sync plugin-skills`"
    );
    for (name, bytes) in &canonical {
        assert!(bundled[name] == *bytes, "{name} differs in the plugin copy");
    }
}

#[test]
fn the_aws_serverless_skill_links_the_files_of_the_rust_template() {
    let skill = read(&format!("{AWS_SERVERLESS}/SKILL.md"));
    assert!(skill.contains("[assets/powertools-handler/](assets/powertools-handler/)"));
    assert!(!skill.contains("powertools-handler.py"));
    let template = tree(POWERTOOLS);
    // Every file the skill lists for retrieval exists, and every source file
    // of the template is listed (the MCP retrieval fetches them one by one).
    let listed: Vec<&str> = skill
        .lines()
        .find(|l| l.contains("Rust Powertools-style handler template"))
        .expect("the file table row")
        .split('`')
        .skip(1)
        .step_by(2)
        .collect();
    for file in &listed {
        assert!(template.contains_key(*file), "{file} is listed but missing");
    }
    for file in template.keys() {
        assert!(
            listed.contains(&file.as_str()),
            "{file} is not listed in SKILL.md"
        );
    }
    for reference in ["lambda.md", "event-sources.md", "deployment.md"] {
        let text = read(&format!("{AWS_SERVERLESS}/references/{reference}"));
        assert!(!text.contains("powertools-handler.py"), "{reference}");
    }
    let deployment = read(&format!("{AWS_SERVERLESS}/references/deployment.md"));
    for needle in [
        "rust-cargolambda",
        "provided.al2023",
        "cargo lambda build",
        "--locked",
    ] {
        assert!(deployment.contains(needle), "deployment.md lacks {needle}");
    }
    assert!(deployment.contains("(#rust-functions-cargo-lambda)"));
    assert!(deployment.contains("## Rust functions (cargo-lambda)"));
}

#[test]
fn the_processing_skill_builds_and_ships_the_rust_artifacts() {
    let skill = read(&format!("{PROCESSING}/SKILL.md"));
    for needle in [
        "cargo lambda build --release --locked",
        "--bin lambda-processor",
        "--runtime provided.al2023",
        "--handler bootstrap",
        "bootstrap.zip",
        "scripts/src/bin/lambda_processor.rs",
        "scripts/src/bin/fargate_processor.rs",
        "cargo-lambda",
    ] {
        assert!(skill.contains(needle), "SKILL.md lacks {needle:?}");
    }
    let lower = skill.to_lowercase();
    for stale in [
        "python3",
        "python 3",
        "lambda_function",
        "fargate_processor.py",
        "lambda_handler",
        "zipfile",
    ] {
        assert!(!lower.contains(stale), "SKILL.md still mentions {stale:?}");
    }
    // The commands name the binaries the manifest defines.
    let manifest = read(&format!("{PROCESSING}/scripts/Cargo.toml"));
    assert!(manifest.contains("name = \"lambda-processor\""));
    assert!(manifest.contains("name = \"fargate-processor\""));
    assert!(root()
        .join(PROCESSING)
        .join("scripts/src/bin/lambda_processor.rs")
        .is_file());
    assert!(root()
        .join(PROCESSING)
        .join("scripts/src/bin/fargate_processor.rs")
        .is_file());
}

#[test]
fn the_dockerfile_pins_its_bases_builds_locked_and_drops_privileges() {
    let dockerfile = read(&format!("{PROCESSING}/scripts/Dockerfile"));
    let instructions: Vec<&str> = dockerfile
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .collect();
    let froms: Vec<&&str> = instructions
        .iter()
        .filter(|l| l.starts_with("FROM "))
        .collect();
    assert_eq!(froms.len(), 2, "one build stage and one runtime stage");
    for from in froms {
        let digest = from
            .split("@sha256:")
            .nth(1)
            .unwrap_or_else(|| panic!("unpinned: {from}"));
        let digest = digest.split_whitespace().next().unwrap();
        assert_eq!(digest.len(), 64, "{from}");
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()), "{from}");
    }
    assert!(instructions.iter().any(|l| l.contains("cargo build")
        && l.contains("--locked")
        && l.contains("--release")
        && l.contains("--bin fargate-processor")));
    assert!(instructions
        .iter()
        .any(|l| l.starts_with("COPY Cargo.toml Cargo.lock")));
    let user = instructions
        .iter()
        .rev()
        .find(|l| l.starts_with("USER "))
        .expect("a USER instruction");
    let uid = user.trim_start_matches("USER ").split(':').next().unwrap();
    assert!(uid.parse::<u32>().is_ok_and(|n| n != 0), "runs as {user:?}");
    assert!(instructions
        .last()
        .is_some_and(|l| l.starts_with("ENTRYPOINT [\"/app/fargate-processor\"]")));
}

/// `name = "=1.2.3"` and `name = { version = "=1.2.3", ... }` pins.
fn pinned_dependencies(manifest: &str) -> Vec<(String, String)> {
    manifest
        .lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(" = ")?;
            let version = rest.split("\"=").nth(1)?.split('"').next()?;
            Some((name.trim().to_string(), version.to_string()))
        })
        .collect()
}

#[test]
fn each_shipped_lock_file_matches_its_manifest_and_is_checksummed() {
    for dir in [POWERTOOLS, &format!("{PROCESSING}/scripts")] {
        let manifest = read(&format!("{dir}/Cargo.toml"));
        let lock = read(&format!("{dir}/Cargo.lock"));
        assert!(
            lock.contains("version = 4") || lock.contains("version = 3"),
            "{dir}"
        );

        // Split into [[package]] blocks.
        let packages: Vec<&str> = lock.split("[[package]]").skip(1).collect();
        let field = |block: &str, name: &str| -> Option<String> {
            block
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{name} = \"")))
                .map(|v| v.trim_end_matches('"').to_string())
        };
        let versions: BTreeMap<String, Vec<String>> =
            packages.iter().fold(BTreeMap::new(), |mut acc, block| {
                acc.entry(field(block, "name").unwrap())
                    .or_default()
                    .push(field(block, "version").unwrap());
                acc
            });

        let pins = pinned_dependencies(&manifest);
        assert!(
            pins.len() >= 5,
            "{dir}: expected exact pins, found {pins:?}"
        );
        for (name, version) in &pins {
            let crate_name = name.replace('_', "-");
            let found = versions
                .iter()
                .filter(|(n, _)| n.replace('_', "-") == crate_name)
                .flat_map(|(_, v)| v.iter());
            assert!(
                found.clone().any(|v| v == version),
                "{dir}: Cargo.lock does not pin {name} {version}"
            );
        }

        // Every registry package carries a checksum; the only package
        // without a source is the example itself.
        for block in packages {
            let name = field(block, "name").unwrap();
            match field(block, "source") {
                Some(source) => {
                    assert!(
                        source.starts_with("registry+https://github.com/rust-lang/crates.io-index"),
                        "{dir}: {name} comes from {source}"
                    );
                    assert!(
                        field(block, "checksum").is_some(),
                        "{dir}: {name} has no checksum"
                    );
                }
                None => assert!(
                    ["powertools-handler", "s3-upload-processor"].contains(&name.as_str()),
                    "{dir}: unexpected local package {name}"
                ),
            }
        }
    }
}

#[test]
fn the_state_machine_still_reaches_the_container_and_the_task_event() {
    let definition: Value = serde_json::from_str(&read(&format!(
        "{PROCESSING}/scripts/statemachine.asl.json"
    )))
    .unwrap();
    let states = &definition["States"];
    assert_eq!(
        states["CheckFileSize"]["Choices"][0]["NumericGreaterThan"],
        6_291_456
    );
    let overrides = &states["RunFargateTask"]["Parameters"]["Overrides"]["ContainerOverrides"][0];
    let container = overrides["Name"].as_str().unwrap();
    let ecs = read(&format!("{PROCESSING}/references/ecs-task-definition.md"));
    assert!(ecs.contains(&format!("\"name\": \"{container}\"")));
    let variable = overrides["Environment"][0]["Name"].as_str().unwrap();
    let fargate = read(&format!(
        "{PROCESSING}/scripts/src/bin/fargate_processor.rs"
    ));
    assert!(
        fargate.contains(&format!("\"{variable}\"")),
        "the Fargate binary must read {variable}"
    );
    // The Lambda branch passes the whole event as the payload.
    assert_eq!(states["InvokeLambda"]["Parameters"]["Payload.$"], "$");
}

#[test]
fn the_migration_record_covers_every_retired_python_file() {
    let record = read("docs/migration/serverless-examples.md");
    for retired in [
        "aws-serverless/assets/powertools-handler.py",
        "scripts/lambda_function.py",
        "scripts/fargate_processor.py",
        "scripts/requirements.txt",
    ] {
        assert!(
            record.contains(retired),
            "the record does not mention {retired}"
        );
    }
    for heading in ["## Behaviour changes", "## Dependencies", "## Verification"] {
        assert!(record.contains(heading), "the record lacks {heading}");
    }
}

#[test]
fn the_examples_are_workspace_members_outside_the_toolkit_binary() {
    let workspace = read("Cargo.toml");
    assert!(workspace.contains(POWERTOOLS));
    assert!(workspace.contains(&format!("{PROCESSING}/scripts")));
    assert!(workspace.contains("exclude = [\"plugins\"]"));
    let cli = read("crates/toolkit-cli/Cargo.toml");
    assert!(!cli.contains("powertools-handler") && !cli.contains("s3-upload-processor"));
    for file in ["registry.rs", "lib.rs"] {
        let source = read(&format!("crates/toolkit-cli/src/{file}"));
        assert!(
            !source.contains("powertools") && !source.contains("s3_upload"),
            "{file}"
        );
    }
}
