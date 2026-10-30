#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::fs;
use std::path::Path;
use toolkit_cli::COMMANDS;

const SCRIPTS: [&str; 6] = [
    "model-selection/scripts/get_deployable_models.py",
    "model-selection/scripts/filter_deployable_models.py",
    "model-selection/scripts/search_deployable_models.py",
    "model-selection/scripts/get_model_names.py",
    "finetuning-technique/scripts/get_recipes.py",
    "endpoint-diagnostics/scripts/collect_diagnostics.py",
];
const DOCS: [&str; 4] = [
    "model-selection/overview.md",
    "model-selection/references/select-for-deployment.md",
    "finetuning-technique/overview.md",
    "endpoint-diagnostics/overview.md",
];

#[test]
fn retired_scripts_and_skill_routes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for tree in [
        "skills/core-skills/aws-ai-ml/references",
        "plugins/aws-core/skills/aws-ai-ml/references",
    ] {
        for script in SCRIPTS {
            assert!(
                !root.join(tree).join(script).exists(),
                "script still exists: {tree}/{script}"
            );
        }
        for doc in DOCS {
            let content = fs::read_to_string(root.join(tree).join(doc)).unwrap();
            for script in SCRIPTS {
                assert!(
                    !content.contains(script.rsplit('/').next().unwrap()),
                    "deleted script named in {tree}/{doc}"
                );
            }
            let joined = content.replace("\\\n", " ");
            for line in joined
                .lines()
                .filter(|line| line.contains("toolkit ai-models "))
            {
                for tail in line.split("toolkit ai-models ").skip(1) {
                    let route = tail
                        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                        .next()
                        .unwrap();
                    let command = COMMANDS
                        .iter()
                        .find(|spec| spec.path == ["ai-models", route])
                        .unwrap_or_else(|| panic!("unregistered route {route} in {doc}"));
                    for word in tail
                        .split_whitespace()
                        .filter(|word| word.starts_with("--"))
                    {
                        let flag =
                            word.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
                        assert!(
                            command.flags.contains(&flag) || command.value_flags.contains(&flag),
                            "unregistered flag {flag} for {route}"
                        );
                    }
                }
            }
        }
    }
}
