//! `toolkit` — read-only utility CLI for the Agent Toolkit fork.
//!
//! Bootstrap slice #4373. The full command contract (commands, flags, exit
//! codes and the JSON shape) is documented in `crates/toolkit-cli/README.md`.

use std::path::PathBuf;
use std::process::ExitCode;

use toolkit_core::{discover_skills, skills_to_json};

const USAGE: &str = "\
Usage:
  toolkit skills list [ROOT] [--json]
  toolkit --version
  toolkit help

Commands:
  skills list   List skill directories (directories containing SKILL.md) under
                ROOT, sorted by name then relative path. ROOT defaults to
                `skills` in the current directory.

Options:
  --json        Emit a JSON array of {name, description, path} objects.
  -h, --help    Print this help.
  -V, --version Print the version.

Exit codes:
  0  success (including help, version and an empty result)
  1  runtime error (missing root, root not a directory, unreadable files)
  2  usage error (unknown command, missing or unknown arguments)";

/// Parsed command line. `Help` and `Version` short-circuit before `run`.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    SkillsList { root: Option<String>, json: bool },
    Help,
    Version,
}

/// Parses arguments without touching the filesystem; usage problems are
/// reported as `Err(USAGE)`.
fn parse_args(args: &[String]) -> Result<Command, &'static str> {
    let Some(first) = args.first().map(String::as_str) else {
        return Err(USAGE);
    };

    match first {
        "-h" | "--help" | "help" => return Ok(Command::Help),
        "-V" | "--version" | "version" => return Ok(Command::Version),
        "skills" => {}
        _ => return Err(USAGE),
    }

    let Some("list") = args.get(1).map(String::as_str) else {
        return Err(USAGE);
    };

    let mut root: Option<String> = None;
    let mut json = false;
    for arg in &args[2..] {
        match arg.as_str() {
            "--json" => json = true,
            "-h" | "--help" => return Ok(Command::Help),
            _ if arg.starts_with('-') => return Err(USAGE),
            _ if root.is_some() => return Err(USAGE),
            _ => root = Some(arg.clone()),
        }
    }
    Ok(Command::SkillsList { root, json })
}

/// Executes a parsed command and returns the process exit code.
fn execute(command: &Command) -> ExitCode {
    match command {
        Command::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Command::Version => {
            println!("toolkit {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::SkillsList { root, json } => {
            let root = PathBuf::from(root.clone().unwrap_or_else(|| "skills".to_string()));
            match discover_skills(&root) {
                Ok(entries) => {
                    if *json {
                        println!("{}", skills_to_json(&entries));
                    } else {
                        for entry in entries {
                            println!("{}\t{}", entry.name, entry.description.replace('\n', " "));
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("error: {error}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&args) {
        Ok(command) => execute(&command),
        Err(usage) => {
            eprintln!("error: invalid arguments\n\n{usage}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parses_skills_list_with_root_and_json() {
        let command =
            parse_args(&args(&["skills", "list", "somewhere", "--json"])).expect("valid arguments");
        assert_eq!(
            command,
            Command::SkillsList {
                root: Some("somewhere".to_string()),
                json: true,
            }
        );
    }

    #[test]
    fn flags_may_precede_the_positional_root() {
        let command =
            parse_args(&args(&["skills", "list", "--json", "somewhere"])).expect("valid arguments");
        assert_eq!(
            command,
            Command::SkillsList {
                root: Some("somewhere".to_string()),
                json: true,
            }
        );
    }

    #[test]
    fn root_defaults_to_none() {
        let command = parse_args(&args(&["skills", "list"])).expect("valid arguments");
        assert_eq!(
            command,
            Command::SkillsList {
                root: None,
                json: false
            }
        );
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert_eq!(parse_args(&args(&["--help"])), Ok(Command::Help));
        assert_eq!(parse_args(&args(&["help"])), Ok(Command::Help));
        assert_eq!(parse_args(&args(&["--version"])), Ok(Command::Version));
        assert_eq!(parse_args(&args(&["version"])), Ok(Command::Version));
    }

    #[test]
    fn usage_errors() {
        assert_eq!(parse_args(&args(&[])), Err(USAGE));
        assert_eq!(parse_args(&args(&["unknown"])), Err(USAGE));
        assert_eq!(parse_args(&args(&["skills"])), Err(USAGE));
        assert_eq!(parse_args(&args(&["skills", "export"])), Err(USAGE));
        assert_eq!(parse_args(&args(&["skills", "list", "--wat"])), Err(USAGE));
        assert_eq!(parse_args(&args(&["skills", "list", "a", "b"])), Err(USAGE));
    }
}
