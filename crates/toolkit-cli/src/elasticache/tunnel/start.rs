//! `toolkit elasticache start-tunnel` (port of `start_tunnel.py`): pre-flight
//! checks, then an SSM port-forwarding session supervised until it ends or the
//! user interrupts.
//!
//! The lifecycle runs over the [`TunnelHost`] and [`TunnelProcess`] seams, so a
//! test (or a recording) scripts the process: how long it takes to settle, what
//! it prints, when the output goes idle, when the user interrupts and how it
//! exits. Times are the script's (2 s settle, 15 s first probe, 30 s health
//! interval, 3 s health probe, 5 s graceful stop) and are passed to the seam,
//! never slept here.

use std::time::Duration;

use toolkit_aws::recorded::{optional_object_list, optional_text};
use toolkit_core::{Json, ToolkitError};

use super::clean::clean;
use super::seams::{Operation, ProcessEvent, SeamError, TunnelAws, TunnelHost, TunnelProcess};
use crate::elasticache::text::dumps_inline;
use crate::elasticache::text::shell_quote;
use crate::Output;

const SETTLE: Duration = Duration::from_secs(2);
const FIRST_PROBE: Duration = Duration::from_secs(15);
const HEALTH_INTERVAL: Duration = Duration::from_secs(30);
const HEALTH_PROBE: Duration = Duration::from_secs(3);
const STOP_GRACE: Duration = Duration::from_secs(5);

const AWS_CLI_URL: &str =
    "https://docs.aws.amazon.com/cli/latest/userguide/getting-started-install.html";
const SSM_PLUGIN_URL: &str = "https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-working-with-install-plugin.html";

/// The arguments of `start-tunnel`. Ports are the raw integers of the command
/// line: the range check is part of the pre-flight output.
pub(super) struct StartArgs<'a> {
    pub instance_id: &'a str,
    pub cache_host: &'a str,
    pub cache_port: i64,
    pub local_port: i64,
    pub region: &'a str,
    pub profile: Option<&'a str>,
}

/// The `aws ssm start-session` command, one argument per element. It never
/// holds a secret: instance, host, ports, region and profile name only.
pub(super) fn tunnel_command(args: &StartArgs<'_>) -> Vec<String> {
    let parameters = Json::object(vec![
        ("host", Json::Array(vec![Json::string(args.cache_host)])),
        (
            "portNumber",
            Json::Array(vec![Json::string(args.cache_port.to_string())]),
        ),
        (
            "localPortNumber",
            Json::Array(vec![Json::string(args.local_port.to_string())]),
        ),
    ]);
    let mut command: Vec<String> = [
        "aws",
        "ssm",
        "start-session",
        "--target",
        args.instance_id,
        "--document-name",
        "AWS-StartPortForwardingSessionToRemoteHost",
        "--parameters",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    command.push(dumps_inline(&parameters));
    command.push("--region".to_string());
    command.push(args.region.to_string());
    if let Some(profile) = args.profile {
        command.push("--profile".to_string());
        command.push(profile.to_string());
    }
    command
}

/// The command as it is echoed: each argument redacted, bounded, escaped and
/// quoted for a POSIX shell.
pub(super) fn echo_command(command: &[String]) -> String {
    command
        .iter()
        .map(|argument| shell_quote(&clean(argument)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `ec2:DescribeInstanceStatus`: whether the instance is `running`.
fn parse_running(response: &Json) -> Result<bool, String> {
    let statuses = optional_object_list(response, "InstanceStatuses").map_err(String::from)?;
    let Some(first) = statuses.first() else {
        return Ok(false);
    };
    let state = toolkit_aws::recorded::optional_object(first, "InstanceState")
        .map_err(String::from)?
        .ok_or_else(|| "InstanceState is missing from the response".to_string())?;
    Ok(optional_text(state, "Name").map_err(String::from)? == Some("running"))
}

/// `ssm:DescribeInstanceInformation`: whether the first entry is `Online`.
fn parse_online(response: &Json) -> Result<bool, String> {
    let infos = optional_object_list(response, "InstanceInformationList").map_err(String::from)?;
    let Some(first) = infos.first() else {
        return Ok(false);
    };
    Ok(optional_text(first, "PingStatus").map_err(String::from)? == Some("Online"))
}

/// One check that asks AWS. A service or recording-shape failure prints the
/// script's warning and counts as "no"; a seam failure ends the run.
fn aws_check(
    aws: &mut dyn TunnelAws,
    operation: Operation,
    request: &Json,
    parse: fn(&Json) -> Result<bool, String>,
    warning: &str,
    out: &mut Output<'_>,
) -> Result<bool, ToolkitError> {
    let outcome = match aws.call(operation, request) {
        Ok(response) => parse(&response)
            .map_err(|reason| SeamError::Aws(toolkit_aws::AwsError::invalid_response(reason))),
        Err(error) => Err(error),
    };
    match outcome {
        Ok(answer) => Ok(answer),
        Err(SeamError::Aws(error)) => {
            out.line(&format!("{warning}: {}", clean(&error.to_string())))?;
            Ok(false)
        }
        Err(other) => Err(other.into()),
    }
}

fn fail(out: &mut Output<'_>, lines: &[&str]) -> Result<(), ToolkitError> {
    for line in lines {
        out.line(line)?;
    }
    out.set_exit_status(1);
    Ok(())
}

/// Runs `start-tunnel`.
pub(super) fn run(
    args: &StartArgs<'_>,
    aws: &mut dyn TunnelAws,
    host: &mut dyn TunnelHost,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    out.line("Pre-flight checks:")?;
    out.line("")?;

    let mut ports = [0_u16; 2];
    for (slot, (label, port)) in ports.iter_mut().zip([
        ("--cache-port", args.cache_port),
        ("--local-port", args.local_port),
    ]) {
        match u16::try_from(port).ok().filter(|port| *port >= 1) {
            Some(valid) => *slot = valid,
            None => {
                return fail(
                    out,
                    &[&format!(
                        "  [FAIL] {label} must be between 1 and 65535 (got {port})"
                    )],
                );
            }
        }
    }
    let [cache_port, local_port] = ports;
    out.line(&format!(
        "  [OK]   Port numbers are valid (local={local_port}, remote={cache_port})"
    ))?;

    if !host.local_port_free(local_port)? {
        return fail(
            out,
            &[
                &format!("  [FAIL] Local port {local_port} is already in use."),
                &format!("         Another process is listening on 127.0.0.1:{local_port}."),
                "         Stop that process first, or use --local-port to pick a different port.",
            ],
        );
    }
    out.line(&format!("  [OK]   Local port {local_port} is available"))?;

    if !host.program_on_path("aws")? {
        return fail(
            out,
            &[
                "  [FAIL] AWS CLI not found on PATH.",
                &format!("         Install: {AWS_CLI_URL}"),
            ],
        );
    }
    out.line("  [OK]   AWS CLI found")?;

    if !host.program_on_path("session-manager-plugin")? {
        return fail(
            out,
            &[
                "  [FAIL] Session Manager plugin not found on PATH.",
                &format!("         Install: {SSM_PLUGIN_URL}"),
            ],
        );
    }
    out.line("  [OK]   Session Manager plugin found")?;

    let instance = clean(args.instance_id);
    let status_request = Json::object(vec![
        (
            "InstanceIds",
            Json::Array(vec![Json::string(args.instance_id)]),
        ),
        ("IncludeAllInstances", Json::Bool(true)),
    ]);
    if !aws_check(
        aws,
        Operation::DescribeInstanceStatus,
        &status_request,
        parse_running,
        "Warning: could not verify instance state",
        out,
    )? {
        return fail(
            out,
            &[
                &format!("  [FAIL] Instance {instance} is not in 'running' state."),
                "         Start the instance or choose a different one (use toolkit elasticache find-tunnel-host).",
            ],
        );
    }
    out.line(&format!("  [OK]   Instance {instance} is running"))?;

    let ssm_request = super::discover::managed_request(&[args.instance_id]);
    if aws_check(
        aws,
        Operation::DescribeInstanceInformation,
        &ssm_request,
        parse_online,
        "Warning: could not verify SSM status",
        out,
    )? {
        out.line(&format!("  [OK]   Instance {instance} SSM agent is online"))?;
    } else {
        for line in [
            format!("  [WARN] Instance {instance} is not reporting to SSM as Online."),
            "         The tunnel may fail. Ensure the instance has:".to_string(),
            "           - SSM agent installed and running".to_string(),
            "           - AmazonSSMManagedInstanceCore IAM policy attached".to_string(),
            "           - Network route to SSM endpoints (NAT gateway or VPC endpoint)".to_string(),
            "         Proceeding anyway ...".to_string(),
            String::new(),
        ] {
            out.line(&line)?;
        }
    }
    out.line("")?;

    let command = tunnel_command(args);
    let cache_host = clean(args.cache_host);
    out.line(&format!(
        "Starting tunnel: 127.0.0.1:{local_port} -> {cache_host}:{cache_port}"
    ))?;
    out.line(&format!("  via SSM instance: {instance}"))?;
    out.line(&format!("  region: {}", clean(args.region)))?;
    out.line("")?;
    out.line("Tunnel command:")?;
    out.line(&format!("  {}", echo_command(&command)))?;
    out.line("")?;

    let mut process = host.spawn(&command)?;
    supervise(args, local_port, process.as_mut(), host, out)
}

/// The launched tunnel: settle, first probe, then the supervising loop.
fn supervise(
    args: &StartArgs<'_>,
    local_port: u16,
    process: &mut dyn TunnelProcess,
    host: &mut dyn TunnelHost,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    match lifecycle(args, local_port, process, host, out) {
        Ok(()) => Ok(()),
        Err(StopRun::Output(error)) => {
            process.kill();
            Err(error)
        }
        Err(StopRun::Seam(SeamError::Aws(error))) => {
            // The script's `except Exception` branch.
            out.line("")?;
            out.line(&format!("Error: {}", clean(&error.to_string())))?;
            process.kill();
            out.set_exit_status(1);
            Ok(())
        }
        Err(StopRun::Seam(other)) => {
            process.kill();
            Err(other.into())
        }
    }
}

enum StopRun {
    Seam(SeamError),
    Output(ToolkitError),
}

impl From<SeamError> for StopRun {
    fn from(error: SeamError) -> Self {
        Self::Seam(error)
    }
}

impl From<ToolkitError> for StopRun {
    fn from(error: ToolkitError) -> Self {
        Self::Output(error)
    }
}

fn lifecycle(
    args: &StartArgs<'_>,
    local_port: u16,
    process: &mut dyn TunnelProcess,
    host: &mut dyn TunnelHost,
    out: &mut Output<'_>,
) -> Result<(), StopRun> {
    if let Some(code) = process.exited_after(SETTLE)? {
        let output = process.drain_output()?;
        out.line(&format!("Tunnel process exited immediately (code {code}):"))?;
        out.line("")?;
        out.line(&clean(&output))?;
        out.set_exit_status(1);
        return Ok(());
    }

    if host.port_accepts(local_port, FIRST_PROBE)? {
        out.line(&format!(
            "Tunnel is UP. Local port 127.0.0.1:{local_port} is accepting connections."
        ))?;
        out.line("")?;
        if args.cache_host.contains(".serverless.") {
            let cache_host = clean(args.cache_host);
            out.line("Note: ElastiCache Serverless requires TLS. Your client must connect")?;
            out.line(&format!("  with TLS enabled and SNI set to {cache_host}"))?;
            out.line("  (the tunnel forwards raw TCP; TLS negotiation happens end-to-end).")?;
            out.line("")?;
        }
        out.line("Test with:")?;
        out.line(&format!(
            "  toolkit elasticache test-connection 127.0.0.1 --port {local_port} \\"
        ))?;
        out.line(&format!(
            "    --tunnel-mode --server-name {} \\",
            shell_quote(&clean(args.cache_host))
        ))?;
        out.line("    --username <user> --password <password>")?;
        out.line("")?;
        out.line("Press Ctrl+C to stop the tunnel.")?;
    } else {
        out.line(&format!(
            "Warning: local port {local_port} not yet responding after 15s."
        ))?;
        out.line("The tunnel may still be starting. Check the output below.")?;
        out.line("")?;
    }

    loop {
        match process.next_event(HEALTH_INTERVAL)? {
            ProcessEvent::Line(line) => {
                out.line(&format!(
                    "  [SSM] {}",
                    clean(line.trim_end_matches(['\r', '\n']))
                ))?;
            }
            ProcessEvent::Idle => {
                if !host.port_accepts(local_port, HEALTH_PROBE)? {
                    out.line("")?;
                    out.line(&format!(
                        "  [WARN] Tunnel port {local_port} is no longer responding."
                    ))?;
                    out.line("         The SSM session may have stalled. Press Ctrl+C to stop.")?;
                }
            }
            ProcessEvent::Eof => break,
            ProcessEvent::Interrupted => return shut_down(process, out),
        }
    }
    let code = process.wait_exit()?;
    if code != 0 {
        out.line("")?;
        out.line(&format!("Tunnel exited with code {code}"))?;
        out.set_exit_status(1);
    }
    Ok(())
}

/// Ctrl+C: ask the process to stop, kill it when it does not within the grace
/// period.
fn shut_down(process: &mut dyn TunnelProcess, out: &mut Output<'_>) -> Result<(), StopRun> {
    out.line("")?;
    out.line("")?;
    out.line("Shutting down tunnel ...")?;
    process.terminate();
    if process.wait(STOP_GRACE)?.is_none() {
        process.kill();
    }
    out.line("Tunnel closed.")?;
    Ok(())
}
