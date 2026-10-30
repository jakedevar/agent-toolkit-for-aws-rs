//! `toolkit elasticache test-connection` (port of `test_connection.py`): a
//! `PING` and an `INFO server` against an ElastiCache endpoint, with RBAC, a
//! legacy AUTH token or an IAM auth token, directly or through an SSM tunnel.
//!
//! Credentials: the password (or IAM token) is a [`toolkit_aws::Secret`] from
//! the command line to the client seam. It is never printed, never part of a
//! diagnostic and absent from the `Debug` output of the connect plan; only the
//! kind of auth (`user=NAME`, `password-auth`, `no-auth`) is reported.

use std::net::IpAddr;

use toolkit_aws::Secret;
use toolkit_core::ToolkitError;

use super::clean::{clean, SecretGate};
use super::seams::{
    CacheEnvironment, CacheErrorKind, ConnectPlan, IamTokenRequest, Resolution, SeamError,
};
use crate::aurora::args::usage;
use crate::Output;

/// The arguments of `test-connection`, already validated.
pub(super) struct ConnectArgs<'a> {
    pub host: &'a str,
    pub port: u16,
    pub timeout_secs: f64,
    pub no_tls: bool,
    pub tunnel_mode: bool,
    pub server_name: Option<&'a str>,
    pub username: Option<&'a str>,
    pub password: Option<&'a str>,
    pub iam_auth: bool,
    pub iam_user: Option<&'a str>,
    pub cache_name: Option<&'a str>,
    pub serverless: bool,
    pub region: &'a str,
}

/// Python `ipaddress` `is_private` (loopback, link-local and the reserved
/// ranges included).
pub(super) fn is_private(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || (a == 198 && (b == 18 || b == 19))
                || a >= 240
                || a == 0
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_private(IpAddr::V4(mapped));
            }
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
        }
    }
}

fn check_dns(
    host: &str,
    env: &mut dyn CacheEnvironment,
    out: &mut Output<'_>,
) -> Result<(), SeamError> {
    if ["127.0.0.1", "localhost", "::1"].contains(&host) {
        return Ok(());
    }
    let shown = clean(host);
    match env.resolve(host)? {
        Resolution::Address(address) if is_private(address) => {
            out.diagnostic(&format!(
                "  WARNING: {shown} resolves to private IP {address}."
            ));
            out.diagnostic("  If you are not in the VPC, this connection will time out.");
            out.diagnostic(
                "  Use --tunnel-mode with `toolkit elasticache start-tunnel` for local development.\n",
            );
        }
        Resolution::Address(_) => {}
        Resolution::NotFound => {
            out.diagnostic(&format!(
                "  WARNING: DNS lookup failed for {shown}. The endpoint may be wrong,"
            ));
            out.diagnostic("  or you may not have DNS access to the VPC.\n");
        }
    }
    Ok(())
}

fn present(text: Option<&str>) -> Option<&str> {
    text.filter(|text| !text.is_empty())
}

/// Runs `test-connection`. A failed connection is exit status 1 with the
/// script's diagnostics; a refusing seam is a typed error.
pub(super) fn run(
    args: &ConnectArgs<'_>,
    env: &mut dyn CacheEnvironment,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let use_tls = !args.no_tls;
    if args.tunnel_mode && args.no_tls {
        return Err(usage(
            "--tunnel-mode requires TLS (cannot combine with --no-tls)",
        ));
    }
    let mut username = present(args.username).map(str::to_string);
    let mut password = present(args.password).map(Secret::new);

    if args.iam_auth {
        let (Some(iam_user), Some(cache_name)) = (present(args.iam_user), present(args.cache_name))
        else {
            return Err(usage("--iam-auth requires --iam-user and --cache-name"));
        };
        out.line(&format!(
            "Generating IAM auth token for user={}, cache={} ...",
            clean(iam_user),
            clean(cache_name)
        ))?;
        let token = env.iam_token(&IamTokenRequest {
            cache_name: cache_name.to_lowercase(),
            iam_user: iam_user.to_string(),
            region: args.region.to_string(),
            serverless: args.serverless,
        })?;
        password = Some(token);
        username = Some(iam_user.to_string());
        out.line("  IAM token generated (valid for 15 minutes)")?;
    }

    if connection_succeeds(args, use_tls, username, password, env, out)? {
        Ok(())
    } else {
        out.set_exit_status(1);
        Ok(())
    }
}

fn connection_succeeds(
    args: &ConnectArgs<'_>,
    use_tls: bool,
    username: Option<String>,
    password: Option<Secret>,
    env: &mut dyn CacheEnvironment,
    out: &mut Output<'_>,
) -> Result<bool, ToolkitError> {
    check_dns(args.host, env, out)?;
    // Client diagnostics use a typed kind with fixed text. Server metadata is
    // withheld whenever a supplied password or active password/IAM token
    // exists. Without credentials it passes a short
    // allowlist (#5004, #5005, #5007).
    let gate = SecretGate::new(&[
        args.password.unwrap_or_default(),
        password.as_ref().map_or("", Secret::expose),
    ]);
    let host = clean(args.host);
    let mut tls_label = if use_tls { "TLS" } else { "plaintext" }.to_string();
    if args.tunnel_mode {
        tls_label.push_str(" tunnel-mode");
    }
    let auth_label = match (&username, &password) {
        (Some(user), _) => format!("user={}", clean(user)),
        (None, Some(_)) => "password-auth".to_string(),
        (None, None) => "no-auth".to_string(),
    };
    out.line(&format!(
        "Connecting to {host}:{} ({tls_label}, {auth_label}) via {} ...",
        args.port,
        env.client_name()
    ))?;
    if use_tls && args.tunnel_mode {
        out.line(&format!(
            "  Tunnel mode: hostname verification disabled, target={host}:{}",
            args.port
        ))?;
        out.diagnostic("  WARNING: tunnel mode is for LOCAL DEVELOPMENT ONLY");
    }

    // Serverless is the flag or the host name; the flag is for a tunnel, where
    // the name is `127.0.0.1`.
    let serverless = args.serverless || args.host.contains(".serverless.");
    let plan = ConnectPlan {
        host: args.host.to_string(),
        port: args.port,
        timeout_secs: args.timeout_secs,
        tls: use_tls,
        check_hostname: !(use_tls && args.tunnel_mode),
        server_name: args
            .server_name
            .filter(|_| use_tls && args.tunnel_mode)
            .map(str::to_string),
        // A cluster client discovers nodes by their VPC addresses, which a
        // tunnel cannot reach.
        cluster_client: serverless && !args.tunnel_mode,
        username,
        password,
    };
    let mut connection = match env.connect(&plan)? {
        Ok(connection) => connection,
        Err(kind) => {
            out.diagnostic(&format!("  Client init FAILED: {}", kind.text()));
            return Ok(false);
        }
    };
    if serverless && args.tunnel_mode {
        out.line("  Note: using standalone client through tunnel (cluster discovery")?;
        out.line("  would return unreachable VPC IPs). PING validates connectivity.")?;
    }

    match connection.ping() {
        Ok(elapsed_ms) => out.line(&format!("  PING: OK ({elapsed_ms:.1} ms)"))?,
        Err(CacheErrorKind::AuthenticationFailed) => {
            out.diagnostic(&format!(
                "  PING FAILED: Authentication error -- {}",
                CacheErrorKind::AuthenticationFailed.text()
            ));
            out.diagnostic(
                "  Hint: Check username/password or ensure the RBAC user exists and is active.",
            );
            return Ok(false);
        }
        Err(CacheErrorKind::Other) => {
            out.diagnostic(&format!("  PING FAILED: {}", CacheErrorKind::Other.text()));
            return Ok(false);
        }
        Err(kind) => {
            out.diagnostic(&format!(
                "  PING FAILED: Connection error -- {}",
                kind.text()
            ));
            // The hint follows the typed kind, never any text.
            out.diagnostic(if kind != CacheErrorKind::TlsHandshakeFailed {
                "  Hint: Check security group rules, VPC placement, and whether you need a tunnel or jump host."
            } else if use_tls {
                "  Hint: TLS handshake failed. Verify the endpoint supports in-transit encryption."
            } else {
                "  Hint: This endpoint may require TLS. Retry without --no-tls (TLS is default)."
            });
            return Ok(false);
        }
    }

    match connection.info_server() {
        Some(info) => out.line(&format!(
            "  Server: engine version {}, mode={}",
            gate.version(&info.version),
            gate.mode(&info.mode)
        ))?,
        // INFO may be blocked by an ACL; that is fine.
        None => out.diagnostic("  Server info: not available (ACL may restrict INFO command)"),
    }

    if use_tls {
        out.line("  TLS: enabled (in-transit encryption active)")?;
    } else {
        out.diagnostic(
            "  TLS: disabled -- consider enabling in-transit encryption for production use",
        );
    }
    connection.close();
    out.line("  Result: CONNECTION SUCCESSFUL")?;
    Ok(true)
}
