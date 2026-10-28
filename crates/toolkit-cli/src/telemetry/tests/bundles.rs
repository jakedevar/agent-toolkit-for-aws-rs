//! Generation and check of the vendored telemetry bundles.

use std::fs;
use std::path::{Path, PathBuf};

use super::fx::{Fx, TempDir};
use crate::telemetry::bundles::{sync, RUNTIME_FILES, SKILLS};

const MANIFESTS: [&str; 5] = [
    "plugin.json",
    ".claude-plugin/plugin.json",
    ".cursor-plugin/plugin.json",
    ".codex-plugin/plugin.json",
    ".devin-plugin/plugin.json",
];

fn real_plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-startup-advisor")
}

/// A copy of the real runtime files and interpreter in a fresh plugin.
fn plugin_fixture(label: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new(label);
    let plugin = dir.path().join("plugin");
    let real = real_plugin();
    for name in RUNTIME_FILES {
        let target = plugin.join("scripts/telemetry").join(name);
        fs::create_dir_all(target.parent().expect("parent")).expect("dir");
        fs::copy(real.join("scripts/telemetry").join(name), target).expect("copy");
    }
    let interpreter = plugin.join("skills/shared/dsl/INTERPRETER.md");
    fs::create_dir_all(interpreter.parent().expect("parent")).expect("dir");
    fs::copy(real.join("skills/shared/dsl/INTERPRETER.md"), interpreter).expect("copy");
    for name in MANIFESTS {
        let manifest = plugin.join(name);
        fs::create_dir_all(manifest.parent().expect("parent")).expect("dir");
        fs::write(manifest, "{\"version\":\"2.0.3\"}").expect("manifest");
    }
    assert_eq!(sync(&plugin, true).expect("generate"), Vec::<String>::new());
    (dir, plugin)
}

fn snapshot(plugin: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    let mut stack = vec![plugin.join("skills")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).expect("dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push((
                    path.strip_prefix(plugin)
                        .expect("prefix")
                        .display()
                        .to_string(),
                    fs::read(&path).expect("read"),
                ));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn the_committed_bundles_are_in_sync_with_the_canonical_runtime() {
    // AC5: the bundle copies shipped in this tree are what the generator
    // produces from scripts/telemetry and plugin.json today.
    assert_eq!(
        sync(&real_plugin(), false).expect("check"),
        Vec::<String>::new()
    );
}

#[test]
fn every_bundle_holds_the_runtime_files_and_its_generated_version() {
    let (_guard, plugin) = plugin_fixture("generated");
    for skill in SKILLS {
        let bundle = plugin
            .join("skills")
            .join(skill)
            .join("references/vendored/telemetry");
        for name in RUNTIME_FILES {
            assert_eq!(
                fs::read(bundle.join(name)).expect("bundled"),
                fs::read(plugin.join("scripts/telemetry").join(name)).expect("canonical"),
                "{skill}/{name}"
            );
        }
        assert_eq!(
            fs::read_to_string(bundle.join("version.json")).expect("version"),
            "{\n  \"version\": \"2.0.3\"\n}\n"
        );
    }
    for skill in ["agent-advisor", "azure-to-aws", "heroku-to-aws"] {
        assert!(plugin
            .join("skills")
            .join(skill)
            .join("references/vendored/dsl/INTERPRETER.md")
            .is_file());
    }
}

#[test]
fn a_missing_or_stale_file_is_reported_and_write_restores_it() {
    let (_guard, plugin) = plugin_fixture("stale");
    assert!(sync(&plugin, false).expect("check").is_empty());
    let target =
        plugin.join("skills/gcp-to-aws/references/vendored/telemetry/metric_emission/client.py");
    fs::remove_file(&target).expect("remove");
    let problems = sync(&plugin, false).expect("check");
    assert_eq!(
        problems,
        vec![
            "skills/gcp-to-aws/references/vendored/telemetry/metric_emission/client.py".to_string()
        ]
    );
    assert!(!target.exists(), "--check writes nothing");
    assert!(sync(&plugin, true).expect("write").is_empty());
    assert!(target.is_file());
    fs::write(&target, "tampered").expect("tamper");
    assert_eq!(sync(&plugin, false).expect("check").len(), 1);
    sync(&plugin, true).expect("write");
    assert!(sync(&plugin, false).expect("check").is_empty());
}

#[test]
fn a_new_plugin_version_makes_exactly_the_four_generated_version_files_stale() {
    let (_guard, plugin) = plugin_fixture("bump");
    for name in MANIFESTS {
        fs::write(plugin.join(name), "{\"version\":\"2.0.4\"}").expect("bump");
    }
    let problems = sync(&plugin, false).expect("check");
    assert_eq!(problems.len(), 4);
    assert!(problems
        .iter()
        .all(|p| p.ends_with("references/vendored/telemetry/version.json")));
    sync(&plugin, true).expect("write");
    assert!(sync(&plugin, false).expect("check").is_empty());
}

#[test]
fn manifest_version_drift_is_refused_in_both_modes_without_writing_or_echoing_values() {
    for manifest in MANIFESTS {
        for write in [false, true] {
            let (_guard, plugin) = plugin_fixture("drift");
            let before = snapshot(&plugin);
            fs::write(plugin.join(manifest), "{\"version\":\"2.0.4\\u001b[31m\"}").expect("drift");
            let problems = sync(&plugin, write).expect("refusal is a result");
            if manifest == "plugin.json" {
                // The reference version moved: the other four now disagree with it.
                assert_eq!(problems.len(), 4, "{manifest}");
            } else {
                assert_eq!(problems.len(), 1, "{manifest}");
                assert!(problems[0].starts_with(manifest));
            }
            assert!(
                problems
                    .iter()
                    .all(|p| p.contains("version does not match plugin.json")),
                "{problems:?}"
            );
            assert!(
                problems
                    .iter()
                    .all(|p| !p.contains("2.0.4") && !p.contains('\u{1b}')),
                "no file value is echoed"
            );
            assert_eq!(
                snapshot(&plugin),
                before,
                "{manifest} write={write}: nothing written"
            );
        }
    }
}

#[test]
fn orphans_are_reported_but_never_deleted_and_runtime_caches_are_ignored() {
    for write in [false, true] {
        for filename in ["obsolete.py", "obsolete.pyc"] {
            let (_guard, plugin) = plugin_fixture("orphan");
            let bundle = plugin.join("skills/gcp-to-aws/references/vendored/telemetry");
            let cache = bundle.join("consent/__pycache__/record.cpython-312.pyc");
            fs::create_dir_all(cache.parent().expect("parent")).expect("dir");
            fs::write(&cache, b"runtime cache").expect("cache");
            let orphan = bundle.join(filename);
            fs::write(&orphan, "obsolete = True\n").expect("orphan");
            let problems = sync(&plugin, write).expect("sync");
            assert_eq!(
                problems,
                vec![format!("Unexpected bundle file: skills/gcp-to-aws/references/vendored/telemetry/{filename}")]
            );
            assert!(orphan.exists());
            fs::remove_file(&orphan).expect("remove");
            assert!(sync(&plugin, false).expect("check").is_empty());
        }
    }
}

#[test]
fn a_nested_orphan_in_an_unlisted_directory_is_reported() {
    let (_guard, plugin) = plugin_fixture("nested");
    let nested = plugin.join("skills/azure-to-aws/references/vendored/telemetry/extra");
    fs::create_dir_all(&nested).expect("dir");
    fs::write(nested.join("x.txt"), "x").expect("file");
    assert_eq!(
        sync(&plugin, false).expect("check"),
        vec![
            "Unexpected bundle file: skills/azure-to-aws/references/vendored/telemetry/extra/x.txt"
                .to_string()
        ]
    );
}

#[test]
fn the_interpreter_copies_are_checked_with_the_bundles() {
    let (_guard, plugin) = plugin_fixture("dsl");
    let copy = plugin.join("skills/heroku-to-aws/references/vendored/dsl/INTERPRETER.md");
    fs::write(&copy, "drifted").expect("drift");
    assert_eq!(
        sync(&plugin, false).expect("check"),
        vec!["skills/heroku-to-aws/references/vendored/dsl/INTERPRETER.md".to_string()]
    );
    sync(&plugin, true).expect("write");
    assert!(sync(&plugin, false).expect("check").is_empty());
}

#[test]
fn an_unreadable_input_is_a_runtime_error_that_names_the_path_not_the_content() {
    let (_guard, plugin) = plugin_fixture("inputs");
    fs::remove_file(plugin.join("scripts/telemetry/cli.py")).expect("remove");
    let error = sync(&plugin, false).expect_err("a canonical file is missing");
    assert_eq!(error.code(), "telemetry_bundles_io");
    assert!(error.message().contains("cli.py"));
    let (_guard, plugin) = plugin_fixture("manifest");
    fs::write(plugin.join("plugin.json"), "{\"no\": \"version\"}").expect("manifest");
    assert_eq!(
        sync(&plugin, false).expect_err("no version").code(),
        "telemetry_bundles_io"
    );
    fs::write(plugin.join("plugin.json"), "not json").expect("manifest");
    assert_eq!(
        sync(&plugin, false).expect_err("not json").code(),
        "telemetry_bundles_io"
    );
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_destination_is_replaced_as_a_link_never_written_through() {
    let (_guard, plugin) = plugin_fixture("symlink");
    let outside = plugin.parent().expect("parent").join("outside.txt");
    fs::write(&outside, "keep me").expect("outside");
    let target = plugin.join("skills/llm-to-bedrock/references/vendored/telemetry/PROTOCOL.md");
    fs::remove_file(&target).expect("remove");
    std::os::unix::fs::symlink(&outside, &target).expect("link");
    assert_eq!(
        sync(&plugin, false).expect("check").len(),
        1,
        "a link is not a current copy"
    );
    sync(&plugin, true).expect("write");
    assert_eq!(fs::read_to_string(&outside).expect("outside"), "keep me");
    assert!(!fs::symlink_metadata(&target)
        .expect("meta")
        .file_type()
        .is_symlink());
    assert!(sync(&plugin, false).expect("check").is_empty());
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_in_the_destination_path_is_refused_on_write() {
    let (_guard, plugin) = plugin_fixture("symlink-dir");
    let skill = plugin.join("skills/gcp-to-aws/references/vendored");
    let elsewhere = plugin.parent().expect("parent").join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("dir");
    fs::remove_dir_all(skill.join("telemetry")).expect("remove");
    std::os::unix::fs::symlink(&elsewhere, skill.join("telemetry")).expect("link");
    let error = sync(&plugin, true).expect_err("refused");
    assert!(
        error.message().contains("symbolic link"),
        "{}",
        error.message()
    );
    assert!(
        fs::read_dir(&elsewhere).expect("dir").next().is_none(),
        "nothing written through the link"
    );
}

#[test]
fn the_command_prints_ok_or_the_drift_and_exits_accordingly() {
    let (_guard, plugin) = plugin_fixture("command");
    let fx = Fx::new("bundles-command");
    let root = plugin.display().to_string();
    let (code, out, err) = fx.run(&["bundles", "--check", "--plugin-root", &root]);
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (0, "Telemetry bundles and DSL copies: OK\n", "")
    );
    let (code, out, _) = fx.run(&["bundles", &format!("--plugin-root={root}")]);
    assert_eq!(
        (code, out.as_str()),
        (0, "Telemetry bundles and DSL copies: OK\n"),
        "check is the default"
    );

    let target = plugin.join("skills/gcp-to-aws/references/vendored/telemetry/cli.py");
    fs::remove_file(&target).expect("remove");
    let (code, out, err) = fx.run(&["bundles", "--check", "--plugin-root", &root]);
    assert_eq!(code, 1);
    assert!(out.starts_with("Telemetry manifests or bundles are out of sync:\nskills/gcp-to-aws/references/vendored/telemetry/cli.py\n"));
    assert!(out.contains("toolkit telemetry bundles --write"));
    assert!(out.contains("--write does not delete them."));
    assert!(
        err.starts_with("") && !target.exists(),
        "--check writes nothing"
    );
    let (code, out, _) = fx.run(&["bundles", "--write", "--plugin-root", &root]);
    assert_eq!(
        (code, out.as_str()),
        (0, "Telemetry bundles and DSL copies: OK\n")
    );
    assert!(target.is_file());
}

#[test]
fn the_command_refuses_conflicting_or_unknown_options() {
    let fx = Fx::new("bundles-usage");
    for words in [
        &["bundles", "--write", "--check"][..],
        &["bundles", "--bogus"],
        &["bundles", "--plugin-root"],
        &["bundles", "--plugin-root", "--write"],
        &["bundles", "--plugin-root="],
        &["bundles", "positional"],
    ] {
        assert_eq!(fx.run(words).0, 2, "{words:?}");
    }
}

#[test]
fn drift_lines_neutralise_control_characters_in_file_names() {
    #[cfg(unix)]
    {
        let (_guard, plugin) = plugin_fixture("names");
        let bundle = plugin.join("skills/gcp-to-aws/references/vendored/telemetry");
        fs::write(bundle.join("evil\u{1b}[31m.py"), "x").expect("orphan");
        let fx = Fx::new("bundles-names");
        let (code, out, _) = fx.run(&[
            "bundles",
            "--check",
            "--plugin-root",
            &plugin.display().to_string(),
        ]);
        assert_eq!(code, 1);
        assert!(!out.contains('\u{1b}'), "{out:?}");
    }
}
