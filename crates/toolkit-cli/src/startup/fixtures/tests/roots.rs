//! Regressions of the round-2 review of b848f1d8 (#4744, findings #4777 #4778
//! #4779). They drive the command line only, so they also build against the
//! parent commit, where the cases marked "parent: PASS" or "parent: refused"
//! behave the wrong way.
//!
//! The trust model under test is one rule, chosen by where a path came from.
//! The three caller-selected roots (`--plugin-root`, `RUN_DIR`, `SEED_DIR`) may
//! themselves be links. Every path derived from one of them refuses a link at
//! every component below that root, however the roots overlap.

use std::fs;
use std::path::Path;

use toolkit_core::Json;

use super::capture::{heroku_estimate_run, GCP_ESTIMATE};
use super::{
    assert_fails, assert_fails_with, assert_passes, copy_dir, golden, plugin_root, set, Copy,
};
use crate::startup::scoring::fixtures::{exec, j, path_text, TempDir};

/// `startup check-expected KIND RUN [SEED] --plugin-root ROOT`.
fn run_with(kind: &str, run: &Path, seed: Option<&Path>, root: &Path) -> (u8, String, String) {
    let mut args = vec!["startup", "check-expected", kind, path_text(run)];
    if let Some(seed) = seed {
        args.push(path_text(seed));
    }
    args.extend(["--plugin-root", path_text(root)]);
    exec(&args)
}

fn assert_link_refused((status, stdout, stderr): (u8, String, String)) {
    assert_eq!(status, 1, "{stdout}{stderr}");
    assert!(
        stdout.contains("symbolic links are not followed"),
        "{stdout}"
    );
    assert!(!stdout.contains("PASS"), "{stdout}");
}

fn assert_pass((status, stdout, stderr): (u8, String, String)) {
    assert_eq!(status, 0, "{stdout}{stderr}");
    assert!(
        stdout.lines().last().is_some_and(|l| l.starts_with("PASS")),
        "{stdout}"
    );
}

// --------------------------- #4777: overlapping caller roots keep the anchor

#[cfg(unix)]
mod overlap {
    use super::*;
    use std::os::unix::fs::symlink;

    /// Gives the plugin root `root` a `fixtures/<set>`: a link to the shipped
    /// `fixtures` directory, or (`real`) a real copy of the one set.
    fn fixtures(root: &Path, set: &str, real: bool) {
        if real {
            copy_dir(
                &plugin_root().join("fixtures").join(set),
                &root.join("fixtures").join(set),
            );
        } else {
            symlink(plugin_root().join("fixtures"), root.join("fixtures")).expect("link");
        }
    }

    #[test]
    fn a_run_directory_equal_to_the_plugin_root_does_not_exempt_fixture_reads() {
        for real in [true, false] {
            let scratch = TempDir::new("check-expected-run-equal");
            let root = scratch.dir("plugin");
            fs::write(root.join("estimation-infra.json"), GCP_ESTIMATE).expect("estimate");
            fixtures(&root, "gcp-live-capture", real);
            let result = run_with("gcp-baseline", &root, None, &root);
            if real {
                // The control: the overlap alone is not an error.
                assert_pass(result);
            } else {
                // parent: PASS (the fixtures link was followed).
                assert_link_refused(result);
            }
        }
    }

    #[test]
    fn a_run_directory_above_the_plugin_root_does_not_exempt_fixture_reads() {
        for real in [true, false] {
            let scratch = TempDir::new("check-expected-run-ancestor");
            let run = scratch.dir("run");
            fs::write(run.join("estimation-infra.json"), GCP_ESTIMATE).expect("estimate");
            let root = run.join("plugin");
            fs::create_dir(&root).expect("plugin root");
            fixtures(&root, "gcp-live-capture", real);
            let result = run_with("gcp-baseline", &run, None, &root);
            if real {
                assert_pass(result);
            } else {
                // parent: PASS.
                assert_link_refused(result);
            }
        }
    }

    #[test]
    fn a_run_directory_apart_from_the_plugin_root_refuses_the_link_too() {
        // The control the reviewer compared with: separate roots.
        let scratch = TempDir::new("check-expected-run-apart");
        let run = scratch.dir("run");
        fs::write(run.join("estimation-infra.json"), GCP_ESTIMATE).expect("estimate");
        let root = scratch.dir("plugin");
        fixtures(&root, "gcp-live-capture", false);
        assert_link_refused(run_with("gcp-baseline", &run, None, &root));
    }

    /// A seed directory holding the frozen Heroku discovery inventory.
    fn seed_inventory(dir: &Path) {
        fs::copy(
            golden("heroku-workshop", "seed").join("heroku-resource-inventory.json"),
            dir.join("heroku-resource-inventory.json"),
        )
        .expect("seed inventory");
    }

    #[test]
    fn a_seed_directory_equal_to_the_plugin_root_does_not_exempt_fixture_reads() {
        let run = golden("heroku-workshop", "after-arm64-reprice");
        for real in [true, false] {
            let scratch = TempDir::new("check-expected-seed-equal");
            let root = scratch.dir("plugin");
            seed_inventory(&root);
            fixtures(&root, "heroku-workshop", real);
            let result = run_with("heroku-workshop", &run, Some(&root), &root);
            if real {
                assert_pass(result);
            } else {
                // parent: PASS.
                assert_link_refused(result);
            }
        }
    }

    #[test]
    fn a_seed_directory_above_the_plugin_root_does_not_exempt_fixture_reads() {
        let run = golden("heroku-workshop", "after-arm64-reprice");
        for real in [true, false] {
            let scratch = TempDir::new("check-expected-seed-ancestor");
            let seed = scratch.dir("seed");
            seed_inventory(&seed);
            let root = seed.join("plugin");
            fs::create_dir(&root).expect("plugin root");
            fixtures(&root, "heroku-workshop", real);
            let result = run_with("heroku-workshop", &run, Some(&seed), &root);
            if real {
                assert_pass(result);
            } else {
                // parent: PASS.
                assert_link_refused(result);
            }
        }
    }

    #[test]
    fn a_seed_directory_apart_from_the_plugin_root_refuses_the_link_too() {
        let run = golden("heroku-workshop", "after-arm64-reprice");
        let scratch = TempDir::new("check-expected-seed-apart");
        let seed = scratch.dir("seed");
        seed_inventory(&seed);
        let root = scratch.dir("plugin");
        fixtures(&root, "heroku-workshop", false);
        assert_link_refused(run_with("heroku-workshop", &run, Some(&seed), &root));
    }

    #[test]
    fn a_link_below_an_overlapping_run_directory_is_still_refused() {
        // Reads of the run itself still refuse a link below RUN_DIR when the
        // run and plugin roots coincide.
        let scratch = TempDir::new("check-expected-run-equal-child");
        let root = scratch.dir("plugin");
        fixtures(&root, "gcp-live-capture", true);
        let outside = scratch.dir("outside");
        fs::write(outside.join("estimation-infra.json"), GCP_ESTIMATE).expect("estimate");
        symlink(
            outside.join("estimation-infra.json"),
            root.join("estimation-infra.json"),
        )
        .expect("link");
        assert_link_refused(run_with("gcp-baseline", &root, None, &root));
    }

    // ------------------------------- #4779: the decide copy and its entry root

    #[test]
    fn a_decide_run_directory_may_itself_be_a_link() {
        for kind in ["heroku-decide", "heroku-decide-retained"] {
            let real = Copy::of("heroku-decision-gate", "after-decide-complete");
            assert_pass(run_with(kind, real.path(), None, &plugin_root()));
            let scratch = TempDir::new("check-expected-decide-linked");
            let linked = scratch.join("linked-decide");
            symlink(real.path(), &linked).expect("link run");
            // parent: refused ("cannot read .../linked-decide: symbolic links
            // are not followed").
            assert_pass(run_with(kind, &linked, None, &plugin_root()));
        }
        let real = Copy::of("gcp-decision-gate", "after-decide-complete");
        let scratch = TempDir::new("check-expected-gcp-decide-linked");
        let linked = scratch.join("linked-decide");
        symlink(real.path(), &linked).expect("link run");
        // parent: refused.
        assert_pass(run_with("gcp-decide", &linked, None, &plugin_root()));
    }

    #[test]
    fn a_link_below_a_decide_run_directory_is_refused_by_the_copy() {
        let scratch = TempDir::new("check-expected-decide-child");
        let outside = scratch.dir("outside");
        fs::write(outside.join("notes.md"), "outside").expect("outside file");
        // An unread child, so only the scratch copy can refuse it: a linked
        // directory, and a linked file one level down.
        let run = Copy::of("heroku-decision-gate", "after-decide-complete");
        symlink(&outside, run.path().join("unread-dir")).expect("dir link");
        let report = assert_fails("heroku-decide", run.path());
        assert!(report.contains("unread-dir"), "{report}");
        assert!(
            report.contains("symbolic links are not followed"),
            "{report}"
        );

        let run = Copy::of("heroku-decision-gate", "after-decide-complete");
        fs::create_dir(run.path().join("nested")).expect("nested");
        symlink(outside.join("notes.md"), run.path().join("nested/notes.md")).expect("file link");
        let linked = scratch.join("linked-run");
        symlink(run.path(), &linked).expect("root link");
        // A linked root does not license a link below it.
        assert_link_refused(run_with("heroku-decide", &linked, None, &plugin_root()));
    }

    #[test]
    fn a_decide_copy_still_refuses_an_oversized_child_below_a_linked_root() {
        let run = Copy::of("heroku-decision-gate", "after-decide-complete");
        fs::create_dir(run.path().join("unread")).expect("unread dir");
        let file = fs::File::create(run.path().join("unread/big.bin")).expect("big file");
        file.set_len(64 * 1024 * 1024 + 1).expect("sparse length");
        drop(file);
        let scratch = TempDir::new("check-expected-decide-big");
        let linked = scratch.join("linked-run");
        symlink(run.path(), &linked).expect("root link");
        let (status, stdout, stderr) = run_with("heroku-decide", &linked, None, &plugin_root());
        assert_eq!(status, 1, "{stdout}{stderr}");
        assert!(stdout.contains("big.bin"), "{stdout}");
        assert!(stdout.contains("larger than 67108864 bytes"), "{stdout}");
    }
}

// ------------------------------ #4778: malformed service data is never a PASS

/// Every malformed `services` value or element, and every malformed root.
const MALFORMED_SERVICES: &[(&str, &str, &str)] = &[
    (
        "null collection",
        "null",
        "services is NoneType, not a list",
    ),
    (
        "object collection",
        r#"{"a": 1}"#,
        "services is dict, not a list",
    ),
    ("text collection", r#""eb""#, "services is str, not a list"),
    ("number collection", "3", "services is int, not a list"),
    ("false collection", "false", "services is bool, not a list"),
    (
        "null element",
        "[null]",
        "services[0] is NoneType, not an object",
    ),
    ("number element", "[7]", "services[0] is int, not an object"),
    (
        "text element",
        r#"["eb"]"#,
        "services[0] is str, not an object",
    ),
    ("list element", "[[]]", "services[0] is list, not an object"),
];

#[test]
fn a_malformed_services_collection_fails_heroku_estimate_by_name() {
    // parent: PASS for the null collection and the null element (the
    // reviewer's two reproductions); the others failed only on an unrelated
    // coverage line or not at all.
    for (label, value, needle) in MALFORMED_SERVICES {
        let run = heroku_estimate_run();
        run.edit("aws-design.json", |doc| set(doc, &["services"], j(value)));
        let report = assert_fails("heroku-estimate", run.path());
        assert!(report.contains(needle), "{label}: {report}");
    }
    // The control: the untouched synthesised run passes.
    assert_passes("heroku-estimate", heroku_estimate_run().path());
}

#[test]
fn a_design_root_that_is_not_an_object_fails_heroku_estimate() {
    for (value, needle) in [
        ("null", "aws-design.json is NoneType, not an object"),
        ("[]", "aws-design.json is list, not an object"),
        (r#""x""#, "aws-design.json is str, not an object"),
        ("0", "aws-design.json is int, not an object"),
    ] {
        let run = heroku_estimate_run();
        run.write("aws-design.json", value);
        // parent: PASS (a non-object root read as no services at all).
        assert_fails_with("heroku-estimate", run.path(), needle);
    }
}

#[test]
fn a_service_without_a_usable_id_fails_heroku_estimate() {
    // `"" in doc` is always true, so a blank id once certified coverage.
    for (label, edit, needle) in [
        (
            "missing id",
            (|svc: &mut Json| remove_member(svc, "service_id")) as fn(&mut Json),
            "service_id must be non-empty text, not NoneType",
        ),
        (
            "null id",
            |svc| set(svc, &["service_id"], Json::Null),
            "service_id must be non-empty text, not NoneType",
        ),
        (
            "empty id",
            |svc| set(svc, &["service_id"], j(r#""""#)),
            "service_id must be non-empty text, not ''",
        ),
        (
            "blank id",
            |svc| set(svc, &["service_id"], j(r#""   ""#)),
            "service_id must be non-empty text, not '   '",
        ),
        (
            "number id",
            |svc| set(svc, &["service_id"], j("7")),
            "service_id must be non-empty text, not int",
        ),
    ] {
        let run = heroku_estimate_run();
        run.edit("aws-design.json", |doc| edit(first_service(doc)));
        // parent: PASS for the missing, null and empty ids.
        let report = assert_fails("heroku-estimate", run.path());
        assert!(report.contains(needle), "{label}: {report}");
    }
}

#[test]
fn a_service_name_that_is_not_text_fails_and_an_empty_one_covers_nothing() {
    let run = heroku_estimate_run();
    run.edit("aws-design.json", |doc| {
        set(first_service(doc), &["aws_service"], j("7"));
    });
    assert_fails_with(
        "heroku-estimate",
        run.path(),
        "aws_service is int, not text",
    );

    // A service priced nowhere, warned nowhere, with an empty name: `"" in
    // doc` must not cover it. parent: PASS.
    let run = heroku_estimate_run();
    run.edit("aws-design.json", |doc| {
        let Json::Object(design) = doc else {
            panic!("design object")
        };
        let Some((_, Json::Array(services))) = design.iter_mut().find(|(k, _)| k == "services")
        else {
            panic!("services list")
        };
        services.push(j(r#"{"service_id": "ghost:unpriced", "aws_service": ""}"#));
    });
    assert_fails_with(
        "heroku-estimate",
        run.path(),
        "design service ghost:unpriced neither in the cost breakdown nor warned as unpriced",
    );
}

/// Every other program that reads a design goes through the same reader.
#[test]
fn a_malformed_services_collection_fails_every_design_reader() {
    let cases: [(&str, &str, &str, &str); 5] = [
        (
            "heroku-workshop",
            "heroku-workshop",
            "after-arm64-reprice",
            "aws-design.json",
        ),
        (
            "gcp-workshop",
            "gcp-workshop",
            "after-graviton-reprice",
            "aws-design.json",
        ),
        (
            "heroku-nonweb-scaling",
            "heroku-nonweb-scaling",
            "after-design",
            "aws-design.json",
        ),
        (
            "azure-design",
            "azure-iac-terraform",
            "after-design",
            "aws-design.json",
        ),
        (
            "azure-estimate",
            "azure-iac-terraform",
            "after-estimate",
            "aws-design.json",
        ),
    ];
    for (kind, set_name, tree, file) in cases {
        for (label, value, needle) in [MALFORMED_SERVICES[0], MALFORMED_SERVICES[5]] {
            let run = Copy::of(set_name, tree);
            if !run.path().join(file).exists() {
                // The estimate tree reads its design from the after-design
                // golden; give it a run copy to damage.
                fs::copy(
                    golden(set_name, "after-design").join(file),
                    run.path().join(file),
                )
                .expect("design copy");
            }
            run.edit(file, |doc| set(doc, &["services"], j(value)));
            let report = check_fails(kind, run.path());
            assert!(report.contains(needle), "{kind} {label}: {report}");
        }
    }
}

/// `assert_fails` for a seeded workshop program: its own seed fixture.
fn check_fails(kind: &str, run: &Path) -> String {
    let (status, stdout, stderr) = super::check(kind, run, &[]);
    assert_eq!(status, 1, "{kind} must fail:\n{stdout}\n{stderr}");
    assert!(!stdout.contains("PASS"), "{kind}: {stdout}");
    stdout
}

fn first_service(doc: &mut Json) -> &mut Json {
    let Json::Object(members) = doc else {
        panic!("design object")
    };
    let Some((_, Json::Array(services))) = members.iter_mut().find(|(k, _)| k == "services") else {
        panic!("services list")
    };
    &mut services[0]
}

fn remove_member(doc: &mut Json, key: &str) {
    let Json::Object(members) = doc else {
        panic!("object")
    };
    members.retain(|(k, _)| k != key);
}
