//! `toolkit elasticache find-tunnel-host` (port of `find_tunnel_host.py`):
//! finds a running, SSM-managed EC2 instance in a VPC to use as a tunnel target.
//!
//! Host selection is the script's: running instances of the VPC, SSM-managed
//! ones first, then by instance type (a stable sort, so ties keep the order of
//! the responses), and the first SSM-managed instance is the recommendation.

use toolkit_aws::recorded::{optional_object, optional_object_list, optional_text};
use toolkit_aws::AwsError;
use toolkit_core::{Json, ToolkitError};

use super::clean::clean;
use super::seams::{paged, paged_partial, Operation, SeamError, TunnelAws};
use crate::elasticache::text::pad_right;
use crate::Output;

/// One running instance of the VPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Instance {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub az: String,
    pub private_ip: String,
    pub ssm: bool,
}

/// The arguments of `find-tunnel-host`, already validated.
pub(super) struct DiscoverArgs<'a> {
    pub vpc_id: &'a str,
    pub region: &'a str,
}

fn required<'j>(object: &'j Json, key: &str, what: &str) -> Result<&'j str, String> {
    optional_text(object, key)
        .map_err(String::from)?
        .ok_or_else(|| format!("{what} is missing from the response"))
}

/// The instances of one `DescribeInstances` page.
pub(super) fn parse_instances(response: &Json) -> Result<Vec<Instance>, String> {
    let mut found = Vec::new();
    for reservation in optional_object_list(response, "Reservations").map_err(String::from)? {
        for instance in optional_object_list(reservation, "Instances").map_err(String::from)? {
            let mut name = String::new();
            for tag in optional_object_list(instance, "Tags").map_err(String::from)? {
                if optional_text(tag, "Key").map_err(String::from)? == Some("Name") {
                    name = optional_text(tag, "Value")
                        .map_err(String::from)?
                        .unwrap_or_default()
                        .to_string();
                    break;
                }
            }
            let placement = optional_object(instance, "Placement")
                .map_err(String::from)?
                .ok_or_else(|| "Placement is missing from the response".to_string())?;
            found.push(Instance {
                id: required(instance, "InstanceId", "InstanceId")?.to_string(),
                name,
                kind: required(instance, "InstanceType", "InstanceType")?.to_string(),
                az: required(placement, "AvailabilityZone", "AvailabilityZone")?.to_string(),
                private_ip: optional_text(instance, "PrivateIpAddress")
                    .map_err(String::from)?
                    .unwrap_or_default()
                    .to_string(),
                ssm: false,
            });
        }
    }
    Ok(found)
}

/// The instance ids of one `DescribeInstanceInformation` page.
pub(super) fn parse_managed_ids(response: &Json) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    for info in optional_object_list(response, "InstanceInformationList").map_err(String::from)? {
        ids.push(required(info, "InstanceId", "InstanceId")?.to_string());
    }
    Ok(ids)
}

/// Whether an AWS error is one of the access-denied codes of the script.
pub(super) fn access_denied(error: &AwsError) -> bool {
    match error {
        AwsError::Service(service) => matches!(
            service.code(),
            Some("AccessDeniedException" | "AccessDenied" | "UnauthorizedAccess")
        ),
        _ => false,
    }
}

fn strings(values: &[&str]) -> Json {
    Json::Array(values.iter().map(|value| Json::string(*value)).collect())
}

fn filter(name_key: &str, name: &str, values: Json) -> Json {
    Json::Object(vec![
        (name_key.to_string(), Json::string(name)),
        ("Values".to_string(), values),
    ])
}

/// The `DescribeInstances` request of the script.
pub(super) fn instances_request(vpc_id: &str) -> Json {
    Json::object(vec![(
        "Filters",
        Json::Array(vec![
            filter("Name", "vpc-id", strings(&[vpc_id])),
            filter("Name", "instance-state-name", strings(&["running"])),
        ]),
    )])
}

/// The `DescribeInstanceInformation` request for `ids`.
pub(super) fn managed_request(ids: &[&str]) -> Json {
    Json::object(vec![(
        "Filters",
        Json::Array(vec![filter("Key", "InstanceIds", strings(ids))]),
    )])
}

/// `find_tunnel_hosts`: the instances, SSM-managed first. An empty list means
/// the VPC has no running instance (the guidance has been printed).
pub(super) fn find_tunnel_hosts(
    args: &DiscoverArgs<'_>,
    aws: &mut dyn TunnelAws,
    lines: &mut Vec<String>,
) -> Result<Vec<Instance>, SeamError> {
    lines.push(format!(
        "Scanning VPC {} in {} for SSM-managed instances ...",
        clean(args.vpc_id),
        clean(args.region)
    ));
    lines.push(String::new());

    let mut instances = paged(
        aws,
        Operation::DescribeInstances,
        &instances_request(args.vpc_id),
        parse_instances,
    )?;

    if instances.is_empty() {
        lines.push("No running EC2 instances found in this VPC.".to_string());
        lines.push(String::new());
        lines.push("Options:".to_string());
        lines.push("  1. Create a minimal jump host: t4g.nano (~$3/month)".to_string());
        lines.push(JUMP_HOST_LAUNCH.to_string());
        lines.push(
            "  2. Deploy your app to Lambda/ECS/EKS in the VPC (no tunnel needed)".to_string(),
        );
        return Ok(Vec::new());
    }

    let ids: Vec<&str> = instances
        .iter()
        .map(|instance| instance.id.as_str())
        .collect();
    // The ids of the pages read before a service error are kept, as the script
    // adds them to `ssm_managed` page by page; a replay or seam failure is not
    // a service error and ends the run.
    let (managed, failure) = paged_partial(
        aws,
        Operation::DescribeInstanceInformation,
        &managed_request(&ids),
        parse_managed_ids,
    )?;
    if let Some(error) = failure {
        if access_denied(&error) {
            lines.push(
                "ERROR: Access denied querying SSM. Ensure your IAM role/user has \
                 ssm:DescribeInstanceInformation permission."
                    .to_string(),
            );
        } else {
            lines.push(format!(
                "Warning: could not query SSM: {}",
                clean(&error.to_string())
            ));
        }
        lines.push("Listing all instances -- SSM status unknown.".to_string());
        lines.push(String::new());
    }
    for instance in &mut instances {
        instance.ssm = managed.contains(&instance.id);
    }
    instances.sort_by(|left, right| {
        (!left.ssm, left.kind.as_str()).cmp(&(!right.ssm, right.kind.as_str()))
    });
    Ok(instances)
}

const JUMP_HOST_LAUNCH: &str = "     Launch one with: aws ec2 run-instances (attach the AmazonSSMManagedInstanceCore IAM policy), then run toolkit elasticache start-tunnel to connect";

/// Runs `find-tunnel-host` over `aws`.
pub(super) fn run(
    args: &DiscoverArgs<'_>,
    aws: &mut dyn TunnelAws,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let mut lines = Vec::new();
    let found = find_tunnel_hosts(args, aws, &mut lines);
    for line in &lines {
        out.line(line)?;
    }
    let results = match found {
        Ok(results) => results,
        Err(SeamError::Aws(error)) => {
            if access_denied(&error) {
                out.line(
                    "ERROR: Access denied. Ensure your IAM role/user has ec2:DescribeInstances \
                     and ssm:DescribeInstanceInformation permissions.",
                )?;
            } else {
                out.line(&format!("ERROR: {}", clean(&error.to_string())))?;
            }
            out.set_exit_status(1);
            return Ok(());
        }
        Err(other) => return Err(other.into()),
    };
    if results.is_empty() {
        out.set_exit_status(1);
        return Ok(());
    }
    render(&results, args.region, out)
}

fn render(results: &[Instance], region: &str, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let ready: Vec<&Instance> = results.iter().filter(|r| r.ssm).collect();
    let others: Vec<&Instance> = results.iter().filter(|r| !r.ssm).collect();
    let region = clean(region);
    let cell = |text: &str, width: usize| pad_right(&clean(text), width);

    if let Some(best) = ready.first() {
        out.line(&format!(
            "Found {} SSM-managed instance(s) (ready for tunnel, $0 extra cost):",
            ready.len()
        ))?;
        out.line("")?;
        out.line(&format!(
            "  {:<22} {:<25} {:<14} {:<16} {}",
            "Instance ID", "Name", "Type", "AZ", "Private IP"
        ))?;
        out.line(&format!(
            "  {} {} {} {} {}",
            "-".repeat(22),
            "-".repeat(25),
            "-".repeat(14),
            "-".repeat(16),
            "-".repeat(15)
        ))?;
        for r in &ready {
            out.line(&format!(
                "  {} {} {} {} {}",
                cell(&r.id, 22),
                cell(&r.name, 25),
                cell(&r.kind, 14),
                cell(&r.az, 16),
                clean(&r.private_ip)
            ))?;
        }
        let label = if best.name.is_empty() {
            &best.kind
        } else {
            &best.name
        };
        let id = clean(&best.id);
        out.line("")?;
        out.line(&format!("Recommended: {id} ({})", clean(label)))?;
        out.line("")?;
        out.line("Next step: start a tunnel with:")?;
        out.line("  toolkit elasticache start-tunnel \\")?;
        out.line(&format!("    --instance-id {id} \\"))?;
        out.line("    --cache-host <your-cache-endpoint> \\")?;
        out.line(&format!("    --region {region}"))?;
        out.line("")?;
        out.line("  For ElastiCache Serverless, you must also tunnel port 6380:")?;
        out.line("  toolkit elasticache start-tunnel \\")?;
        out.line(&format!("    --instance-id {id} \\"))?;
        out.line("    --cache-host <your-cache-endpoint> \\")?;
        out.line("    --cache-port 6380 \\")?;
        out.line("    --local-port 6380 \\")?;
        out.line(&format!("    --region {region}"))?;
        out.line("")?;
        out.line("  Note: ElastiCache Serverless requires TLS. When connecting through")?;
        out.line(
            "  the tunnel, use --tunnel-mode --server-name <your-cache-endpoint> to pass the",
        )?;
        out.line("  the real cache hostname for reference.")?;
    } else {
        out.line(&format!(
            "Found {} instance(s) but none have SSM agent:",
            others.len()
        ))?;
        out.line("")?;
        out.line(&format!(
            "  {:<22} {:<25} {:<14}",
            "Instance ID", "Name", "Type"
        ))?;
        out.line(&format!(
            "  {} {} {}",
            "-".repeat(22),
            "-".repeat(25),
            "-".repeat(14)
        ))?;
        for r in &others {
            out.line(&format!(
                "  {} {} {}",
                cell(&r.id, 22),
                cell(&r.name, 25),
                cell(&r.kind, 14)
            ))?;
        }
        out.line("")?;
        out.line("Options:")?;
        out.line("  1. Install SSM agent on one of these instances (free):")?;
        out.line("     Attach the AmazonSSMManagedInstanceCore IAM policy to the instance role")?;
        out.line("  2. Create a minimal jump host (~$3/month):")?;
        out.line(JUMP_HOST_LAUNCH)?;
    }
    if !others.is_empty() && !ready.is_empty() {
        out.line("")?;
        out.line(&format!(
            "({} additional instance(s) without SSM agent not shown)",
            others.len()
        ))?;
    }
    Ok(())
}
