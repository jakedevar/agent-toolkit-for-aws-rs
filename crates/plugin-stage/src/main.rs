//! `plugin-stage`: build and stage per-plugin `./bin` binaries (#4487).
//!
//! ```text
//! plugin-stage check   [COMMON]                 launcher contract + staged files
//! plugin-stage verify  [COMMON] [--complete]    staged files only (--complete: release gate)
//! plugin-stage plan    [COMMON] [SELECT]        print the builds; refuse what cannot build here
//! plugin-stage stage   [COMMON] [SELECT] [--allow-dirty] [--clean]
//! plugin-stage launcher [COMMON] PLUGIN/NAME    print the canonical launcher JSON
//! plugin-stage verify-package [COMMON] --package-root DIR --complete
//!                                               npm prepack guard: verify one declared
//!                                               package destination (R-PKG-4, #4411)
//!
//! COMMON: --root DIR (workspace; default: nearest ancestor with the declaration)
//!         --plugins-root DIR (default ROOT/plugins)
//!         --declaration FILE (default ROOT/packaging/plugin-binaries.json)
//! SELECT: --platform host|all|KEY (repeatable; default host)
//!         --only PLUGIN/NAME or PACKAGE_ROOT/NAME (repeatable; default every
//!         declared binary)
//! ```
//!
//! Exit status: 0 success, 1 contract problem or build failure, 2 usage.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use plugin_stage::declaration::check_package_root;
use plugin_stage::launcher::{check_launchers, launcher_snippet};
use plugin_stage::manifest::{tracked_package_files, verify_package, verify_staged};
use plugin_stage::stage::{self, Probe};
use plugin_stage::{Declaration, Destination, DECLARATION_PATH};
use toolkit_core::{neutralize_line, quote_untrusted};

const USAGE: &str = "usage: plugin-stage <check|verify|plan|stage|launcher|verify-package> [--root DIR] [--plugins-root DIR] [--declaration FILE] [--platform host|all|KEY]... [--only PLUGIN/NAME]... [--complete] [--allow-dirty] [--clean] [--package-root DIR] [PLUGIN/NAME]";

#[derive(Default)]
struct Args {
    command: String,
    root: Option<PathBuf>,
    plugins_root: Option<PathBuf>,
    declaration: Option<PathBuf>,
    platforms: Vec<String>,
    only: Vec<String>,
    complete: bool,
    allow_dirty: bool,
    clean: bool,
    package_root: Option<PathBuf>,
    positional: Vec<String>,
}

fn parse_args(raw: &[String]) -> Result<Args, String> {
    let mut iter = raw.iter();
    let mut args = Args {
        command: iter.next().ok_or("missing command")?.clone(),
        ..Args::default()
    };
    while let Some(flag) = iter.next() {
        let mut value = |name: &str| {
            iter.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "--root" => args.root = Some(PathBuf::from(value("--root")?)),
            "--plugins-root" => args.plugins_root = Some(PathBuf::from(value("--plugins-root")?)),
            "--declaration" => args.declaration = Some(PathBuf::from(value("--declaration")?)),
            "--platform" => args.platforms.push(value("--platform")?),
            "--only" => args.only.push(value("--only")?),
            "--complete" => args.complete = true,
            "--allow-dirty" => args.allow_dirty = true,
            "--clean" => args.clean = true,
            "--package-root" => {
                args.package_root = Some(PathBuf::from(value("--package-root")?));
            }
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            other => args.positional.push(other.to_string()),
        }
    }
    Ok(args)
}

struct Context {
    root: PathBuf,
    plugins_root: PathBuf,
    declaration: Declaration,
}

fn context(args: &Args) -> Result<Context, String> {
    let root = match &args.root {
        Some(root) => root.clone(),
        None => find_root()?,
    };
    let declaration_path = args
        .declaration
        .clone()
        .unwrap_or_else(|| root.join(DECLARATION_PATH));
    Ok(Context {
        plugins_root: args
            .plugins_root
            .clone()
            .unwrap_or_else(|| root.join("plugins")),
        declaration: Declaration::load(&declaration_path)?,
        root,
    })
}

fn find_root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir()
        .map_err(|err| format!("cannot read the current directory: {err}"))?;
    cwd.ancestors()
        .find(|dir| dir.join(DECLARATION_PATH).is_file())
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "no ancestor of {} contains {DECLARATION_PATH}; pass --root",
                cwd.display()
            )
        })
}

fn report(problems: &[String], ok_message: &str) -> ExitCode {
    if problems.is_empty() {
        println!("{ok_message}");
        ExitCode::SUCCESS
    } else {
        for problem in problems {
            eprintln!("plugin-stage: {}", neutralize_line(problem));
        }
        eprintln!("plugin-stage: {} problem(s)", problems.len());
        ExitCode::from(1)
    }
}

fn run_check(ctx: &Context, complete: bool) -> ExitCode {
    let launchers = check_launchers(&ctx.root, &ctx.plugins_root, &ctx.declaration);
    let mut problems = launchers.problems.clone();
    problems.extend(verify_staged(
        &ctx.root,
        &ctx.plugins_root,
        &ctx.declaration,
        complete,
    ));
    problems.extend(tracked_package_files(&ctx.root, &ctx.declaration));
    for reference in &launchers.references {
        println!(
            "launcher {}/{}: {}",
            reference.plugin, reference.name, reference.location
        );
    }
    for binary in launchers.unreferenced(&ctx.declaration) {
        // A zero-reference scan is never evidence of a safe guard (#5022):
        // it only means no registration runs this binary yet.
        let note = if matches!(binary.role, plugin_stage::Role::GuardHook { .. }) {
            "; this guard is NOT enforced by any registration, and the contract only proves that no launcher runs it in a non-canonical form"
        } else {
            ""
        };
        println!(
            "declared {}/{} ({}) has no launcher yet (consumer {}){note}",
            binary.destination.key(),
            binary.name,
            binary.role.as_str(),
            binary.consumer
        );
    }
    report(
        &problems,
        &format!(
            "plugin-stage: launcher contract holds ({} launcher files, {} references)",
            launchers.files_scanned,
            launchers.references.len()
        ),
    )
}

fn run_plan(ctx: &Context, args: &Args, build_now: bool) -> ExitCode {
    let probe = match Probe::detect(&ctx.root) {
        Ok(probe) => probe,
        Err(err) => return report(&[err], ""),
    };
    let platforms =
        match stage::select_platforms(&args.platforms, &ctx.declaration, &probe.host_triple) {
            Ok(platforms) => platforms,
            Err(err) => return report(&[err], ""),
        };
    let steps = match stage::plan(&ctx.declaration, &platforms, &args.only, &probe) {
        Ok(steps) => steps,
        Err(problems) => return report(&problems, ""),
    };
    for step in &steps {
        println!("{}", step.display());
    }
    if !build_now {
        return report(
            &[],
            &format!("plugin-stage: {} build step(s) can run here", steps.len()),
        );
    }
    let provenance = match stage::source_provenance(&ctx.root, &probe.rustc_version) {
        Ok(provenance) => provenance,
        Err(err) => return report(&[err], ""),
    };
    if provenance.source_dirty && !args.allow_dirty {
        return report(
            &["the checkout has uncommitted changes or untracked files that are not ignored (Cargo may build them, for example an untracked build.rs); commit or remove them, or pass --allow-dirty (the manifest records source_dirty: true, it never merges with a clean stage and the release gate refuses it)".to_string()],
            "",
        );
    }
    let mut by_destination: BTreeMap<&Destination, Vec<&stage::BuildStep>> = BTreeMap::new();
    for step in &steps {
        by_destination
            .entry(&step.binary.destination)
            .or_default()
            .push(step);
    }
    for destination in by_destination.keys() {
        if let Destination::PackageRoot(root) = destination {
            let problems = check_package_root(&ctx.root, root);
            if !problems.is_empty() {
                return report(&problems, "");
            }
        }
        let dest_dir = destination.dir(&ctx.root, &ctx.plugins_root);
        if let Err(err) = stage::check_merge_at(&dest_dir, &provenance, args.clean) {
            return report(&[err], "");
        }
    }
    for (destination, destination_steps) in by_destination {
        let mut outputs = Vec::new();
        for step in destination_steps {
            match stage::build(&ctx.root, step, &probe.target_dir) {
                Ok(item) => outputs.push(item),
                Err(err) => return report(&[err], ""),
            }
        }
        let dest_dir = destination.dir(&ctx.root, &ctx.plugins_root);
        match stage::install_at(&dest_dir, destination, &outputs, &provenance, args.clean) {
            Ok(manifest) => println!(
                "plugin-stage: staged {} file(s) for {} from {}{}",
                manifest.binaries.len(),
                destination.label(),
                manifest.source_commit,
                if manifest.source_dirty {
                    " (dirty)"
                } else {
                    ""
                }
            ),
            Err(err) => return report(&[err], ""),
        }
    }
    let problems = verify_staged(&ctx.root, &ctx.plugins_root, &ctx.declaration, false);
    report(
        &problems,
        "plugin-stage: staged files verified against their manifests",
    )
}

fn run_launcher(ctx: &Context, args: &Args) -> ExitCode {
    let [selector] = args.positional.as_slice() else {
        return usage("launcher needs exactly one PLUGIN/NAME");
    };
    match ctx.declaration.select(selector).map(launcher_snippet) {
        Some(Ok(snippet)) => {
            println!("{snippet}");
            ExitCode::SUCCESS
        }
        Some(Err(problem)) => report(&[problem], ""),
        None => report(
            &[format!(
                "{}: no such PLUGIN/NAME in {DECLARATION_PATH}",
                quote_untrusted(selector)
            )],
            "",
        ),
    }
}

fn usage(message: &str) -> ExitCode {
    eprintln!("plugin-stage: {message}\n{USAGE}");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&raw) {
        Ok(args) => args,
        Err(err) => return usage(&err),
    };
    if !matches!(
        args.command.as_str(),
        "check" | "verify" | "plan" | "stage" | "launcher" | "verify-package"
    ) {
        return usage(&format!("unknown command {:?}", args.command));
    }
    if args.command != "launcher" && !args.positional.is_empty() {
        return usage(&format!("unexpected argument {:?}", args.positional[0]));
    }
    if args.command == "verify-package" {
        if args.package_root.is_none() {
            return usage("verify-package needs --package-root DIR");
        }
        if !args.complete {
            return usage("verify-package is the pack guard and needs --complete");
        }
    } else if args.package_root.is_some() {
        return usage("--package-root is only valid for verify-package");
    }
    let ctx = match context(&args) {
        Ok(ctx) => ctx,
        Err(err) => return report(&[err], ""),
    };
    match args.command.as_str() {
        "check" => run_check(&ctx, args.complete),
        "verify" => {
            let problems = verify_staged(
                &ctx.root,
                &ctx.plugins_root,
                &ctx.declaration,
                args.complete,
            );
            report(
                &problems,
                "plugin-stage: staged files verified against their manifests",
            )
        }
        "plan" => run_plan(&ctx, &args, false),
        "stage" => run_plan(&ctx, &args, true),
        "verify-package" => {
            let dir = args.package_root.as_deref().unwrap_or(Path::new("."));
            let problems = verify_package(&ctx.root, &ctx.declaration, dir, args.complete);
            report(
                &problems,
                "plugin-stage: package destination verified against its manifest (complete)",
            )
        }
        _ => run_launcher(&ctx, &args),
    }
}
