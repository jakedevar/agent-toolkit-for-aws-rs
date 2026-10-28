//! The caller-identity preflight (`_preflight` of `deploy_model.py`).
//!
//! The script asked STS who the caller is and IAM for the account alias, then
//! refused to continue when either looked like production. Those two reads are
//! the only AWS state the plan depends on; they go through [`IdentityClient`],
//! which this build answers from a recorded document ([`super::recorded`]) or
//! not at all ([`NoLiveClient`]). A live client is a follow-up.

use toolkit_aws::recorded::{optional_string_list, optional_text, MemberError};
use toolkit_aws::AwsError;
use toolkit_core::{quote_untrusted, Json, ToolkitError};

use super::model::die;
use super::names::{check_account, check_alias, check_caller_arn, PROD_MARKERS};
use crate::dynamodb::pyfmt;

/// The two AWS reads of the preflight.
pub(crate) trait IdentityClient {
    /// `sts:GetCallerIdentity`.
    fn get_caller_identity(&mut self) -> Result<Json, AwsError>;
    /// `iam:ListAccountAliases`.
    fn list_account_aliases(&mut self) -> Result<Json, AwsError>;
    /// Recorded steps no call used (always 0 for a client that records none).
    fn unconsumed(&self) -> usize {
        0
    }
}

/// The client of a build without live AWS access: every call fails.
pub(crate) struct NoLiveClient;

const NO_LIVE: &str = "this build has no live AWS client (pass --recorded FILE to plan offline \
from a recorded caller identity)";

impl IdentityClient for NoLiveClient {
    fn get_caller_identity(&mut self) -> Result<Json, AwsError> {
        Err(AwsError::connect(NO_LIVE))
    }

    fn list_account_aliases(&mut self) -> Result<Json, AwsError> {
        Err(AwsError::connect(NO_LIVE))
    }
}

/// Who the plan is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) account: String,
    pub(crate) alias: String,
    pub(crate) arn: String,
}

fn invalid_response(operation: &str, error: &MemberError) -> ToolkitError {
    die(format!("{operation} returned an invalid response: {error}"))
}

fn sts_failure(error: &AwsError, profile: &str) -> ToolkitError {
    let shown = quote_untrusted(profile);
    match error {
        AwsError::NoCredentials => die(format!(
            "AWS credentials not found for profile {shown}. Try: aws sso login --profile {shown}"
        )),
        AwsError::Service(service) => die(format!(
            "sts get-caller-identity failed ({}): {}. Credentials may be expired — try: aws sso \
             login --profile {shown}",
            quote_untrusted(service.code().unwrap_or("Unknown")),
            quote_untrusted(service.message())
        )),
        AwsError::Connect { .. } | AwsError::Timeout { .. } => die(format!(
            "cannot reach AWS endpoint: {}",
            quote_untrusted(&error.to_string())
        )),
        other => die(format!(
            "AWS credential check failed: {}: {}",
            other.code(),
            quote_untrusted(&other.to_string())
        )),
    }
}

fn iam_failure(error: &AwsError) -> ToolkitError {
    let reason = match error {
        AwsError::Service(service) => format!(
            "iam list-account-aliases failed ({}): {}",
            quote_untrusted(service.code().unwrap_or("Unknown")),
            quote_untrusted(service.message())
        ),
        other => format!(
            "iam list-account-aliases could not be answered: {}",
            quote_untrusted(&other.to_string())
        ),
    };
    die(format!(
        "{reason}. The production check needs the account alias, so this run is REFUSED."
    ))
}

/// Asks the client who the caller is. Every failure stops the run: a failed STS
/// or IAM call (recorded or not), a request that differs from the recorded one,
/// a call with no recorded step left and a response of the wrong shape. The
/// script let a failed alias lookup through and ran the production check on the
/// ARN alone; that hid a production alias, so it is refused here.
///
/// # Errors
///
/// Exit status 2 for a failed or malformed STS or IAM call, an account id, ARN
/// or alias of the wrong shape, or a recording with steps the run never used.
/// A diagnostic names the member and its expected shape, never its value.
pub(crate) fn resolve_identity(
    client: &mut dyn IdentityClient,
    profile: &str,
) -> Result<Identity, ToolkitError> {
    let caller = client
        .get_caller_identity()
        .map_err(|error| sts_failure(&error, profile))?;
    let arn = optional_text(&caller, "Arn")
        .map_err(|error| invalid_response("sts get-caller-identity", &error))?
        .unwrap_or_default()
        .to_string();
    let account = optional_text(&caller, "Account")
        .map_err(|error| invalid_response("sts get-caller-identity", &error))?
        .unwrap_or_default()
        .to_string();
    check_account("sts get-caller-identity \"Account\"", &account)?;
    check_caller_arn("sts get-caller-identity \"Arn\"", &arn, &account)?;
    let response = client
        .list_account_aliases()
        .map_err(|error| iam_failure(&error))?;
    let alias = optional_string_list(&response, "AccountAliases")
        .map_err(|error| invalid_response("iam list-account-aliases", &error))?
        .first()
        .map(|alias| (*alias).to_string())
        .unwrap_or_default();
    if !alias.is_empty() {
        check_alias("iam list-account-aliases \"AccountAliases\"[0]", &alias)?;
    }
    let unused = client.unconsumed();
    if unused > 0 {
        return Err(die(format!(
            "the recorded document has {unused} step(s) this run never used"
        )));
    }
    Ok(Identity {
        account,
        alias,
        arn,
    })
}

/// Refuses an identity whose ARN or alias mentions a production marker.
///
/// # Errors
///
/// Exit status 2 naming the markers found.
pub(crate) fn refuse_production(identity: &Identity) -> Result<(), ToolkitError> {
    let lower = format!("{} {}", identity.arn, identity.alias).to_lowercase();
    let matched: Vec<String> = PROD_MARKERS
        .iter()
        .filter(|marker| lower.contains(*marker))
        .map(|marker| (*marker).to_string())
        .collect();
    if matched.is_empty() {
        return Ok(());
    }
    let alias = if identity.alias.is_empty() {
        "<none>".to_string()
    } else {
        quote_untrusted(&identity.alias)
    };
    Err(die(format!(
        "caller identity appears to be production ({}). ARN: {}. Alias: {alias}. REFUSING to \
         deploy benchmark resources against a prod account.",
        pyfmt::repr_strings(&matched),
        quote_untrusted(&identity.arn)
    )))
}
