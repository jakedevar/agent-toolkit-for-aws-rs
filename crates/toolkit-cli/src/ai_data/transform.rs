//! `SageMaker` dataset transformation job tools (`transformation_tools.py`).
//!
//! The Python module submitted a user's transformation script as a `SageMaker`
//! Processing job and described a job by name. Every service call here goes
//! through [`ProcessingService`]; this build wires [`NoSageMaker`], so
//! `submit` and `describe` report that the SDK is unavailable, while `plan`
//! computes the exact job specification offline. The script that runs inside
//! the managed container is a remote payload and stays outside this crate
//! (see `docs/migration/ai-data.md`).

use std::path::{Path, PathBuf};

use toolkit_core::{open_regular_file, Json};

/// Where the dataset is mounted inside the processing container.
pub(crate) const INPUT_LOCAL_PATH: &str = "/opt/ml/processing/input";
/// Where the container writes its output.
pub(crate) const OUTPUT_LOCAL_PATH: &str = "/opt/ml/processing/output";
/// Default instance type.
pub(crate) const DEFAULT_INSTANCE_TYPE: &str = "ml.m5.xlarge";
/// Default job name prefix.
pub(crate) const DEFAULT_BASE_JOB_NAME: &str = "dataset-transformation";
/// Version of the scikit-learn processing image used when none is given.
pub(crate) const SKLEARN_IMAGE_VERSION: &str = "1.2-1";

/// The arguments of `execute_transformation_job`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransformRequest {
    pub(crate) script: PathBuf,
    pub(crate) source_s3: String,
    pub(crate) output_s3: String,
    pub(crate) instance_type: String,
    pub(crate) region: Option<String>,
    pub(crate) execution_role: Option<String>,
    pub(crate) base_job_name: String,
    pub(crate) image_uri: Option<String>,
}

/// The Processing job a request turns into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessingPlan {
    /// Script file name inside `source_dir` (the job's `code`).
    pub(crate) code: String,
    /// Directory uploaded as the job's source (the script's directory).
    pub(crate) source_dir: String,
    pub(crate) arguments: Vec<String>,
    pub(crate) source_s3: String,
    pub(crate) output_s3: String,
    pub(crate) instance_type: String,
    pub(crate) base_job_name: String,
    pub(crate) region: Option<String>,
    pub(crate) execution_role: Option<String>,
    pub(crate) image_uri: Option<String>,
}

/// A string, or `null` for a value the service resolves.
fn optional_json(value: Option<&str>) -> Json {
    value.map_or(Json::Null, Json::string)
}

impl ProcessingPlan {
    /// The specification as JSON (`null` for values the service resolves).
    pub(crate) fn to_json(&self) -> Json {
        Json::object(vec![
            ("code", Json::string(self.code.as_str())),
            ("source_dir", Json::string(self.source_dir.as_str())),
            ("command", Json::Array(vec![Json::string("python3")])),
            ("instance_count", Json::Int(1)),
            ("instance_type", Json::string(self.instance_type.as_str())),
            ("base_job_name", Json::string(self.base_job_name.as_str())),
            ("role", optional_json(self.execution_role.as_deref())),
            ("region", optional_json(self.region.as_deref())),
            ("image_uri", optional_json(self.image_uri.as_deref())),
            (
                "arguments",
                Json::Array(
                    self.arguments
                        .iter()
                        .map(|a| Json::string(a.as_str()))
                        .collect(),
                ),
            ),
            (
                "inputs",
                Json::Array(vec![Json::object(vec![
                    ("input_name", Json::string("dataset")),
                    ("s3_uri", Json::string(self.source_s3.as_str())),
                    ("local_path", Json::string(INPUT_LOCAL_PATH)),
                    ("s3_data_type", Json::string("S3Prefix")),
                    ("s3_input_mode", Json::string("File")),
                ])]),
            ),
            (
                "outputs",
                Json::Array(vec![Json::object(vec![
                    ("output_name", Json::string("transformed")),
                    ("s3_uri", Json::string(self.output_s3.as_str())),
                    ("local_path", Json::string(OUTPUT_LOCAL_PATH)),
                    ("s3_upload_mode", Json::string("EndOfJob")),
                ])]),
            ),
            ("wait", Json::Bool(false)),
        ])
    }
}

/// Details of a Processing job (`describe_transformation_job`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct JobDetails {
    pub(crate) status: Option<String>,
    pub(crate) failure_reason: Option<String>,
    /// `str()` of the creation time; empty when the service reports none.
    pub(crate) creation_time: String,
    /// `str()` of the end time; empty when the service reports none.
    pub(crate) processing_end_time: String,
    pub(crate) inputs: Vec<Json>,
    pub(crate) outputs: Vec<Json>,
}

/// The `SageMaker` calls the tools need.
pub(crate) trait ProcessingService {
    /// The caller's execution role ARN (`get_execution_role`).
    fn default_role(&self) -> Result<String, String>;
    /// The region of the default session (`boto_region_name`).
    fn default_region(&self) -> Result<String, String>;
    /// The scikit-learn processing image for a region and instance type.
    fn sklearn_image(
        &self,
        region: &str,
        version: &str,
        instance_type: &str,
    ) -> Result<String, String>;
    /// Starts the job without waiting and returns its name.
    fn start(&self, plan: &ProcessingPlan) -> Result<String, String>;
    /// Reads one job.
    fn describe(&self, job_name: &str, region: Option<&str>) -> Result<JobDetails, String>;
}

/// The service of a build without a `SageMaker` SDK client.
pub(crate) struct NoSageMaker;

const UNAVAILABLE: &str = "SageMaker SDK access is not available in this build";

impl ProcessingService for NoSageMaker {
    fn default_role(&self) -> Result<String, String> {
        Err(UNAVAILABLE.to_string())
    }

    fn default_region(&self) -> Result<String, String> {
        Err(UNAVAILABLE.to_string())
    }

    fn sklearn_image(&self, _: &str, _: &str, _: &str) -> Result<String, String> {
        Err(UNAVAILABLE.to_string())
    }

    fn start(&self, _: &ProcessingPlan) -> Result<String, String> {
        Err(UNAVAILABLE.to_string())
    }

    fn describe(&self, _: &str, _: Option<&str>) -> Result<JobDetails, String> {
        Err(UNAVAILABLE.to_string())
    }
}

/// `os.path.basename(uri.rstrip("/"))`: the last `/`-separated word.
fn input_file_name(source_s3: &str) -> &str {
    let trimmed = source_s3.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// `os.path.join(directory, name)` for a relative `name` without `/`.
fn join(directory: &str, name: &str) -> String {
    format!("{directory}/{name}")
}

fn check_s3_uri(label: &str, uri: &str) -> Result<(), String> {
    let bucket = uri
        .strip_prefix("s3://")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("");
    if bucket.is_empty() {
        return Err(format!(
            "Invalid {label}: must be an s3:// URI naming a bucket (got: {})",
            crate::safe_text::shown_value(uri)
        ));
    }
    Ok(())
}

fn script_location(script: &Path) -> Result<(String, String), String> {
    // The script itself must be a regular file and not a symbolic link; the
    // directory is taken as written (made absolute without collapsing `..`),
    // so it names the directory the operating system resolves.
    open_regular_file(script).map_err(|error| error.to_string())?;
    let name = script
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("script path is not valid UTF-8: {}", script.display()))?;
    let absolute = std::path::absolute(script)
        .map_err(|error| format!("cannot resolve {}: {error}", script.display()))?;
    let directory = absolute
        .parent()
        .and_then(|parent| parent.to_str())
        .ok_or_else(|| format!("script path is not valid UTF-8: {}", script.display()))?;
    Ok((name.to_string(), directory.to_string()))
}

/// Builds the job specification for a request without contacting anything.
pub(crate) fn build_plan(request: &TransformRequest) -> Result<ProcessingPlan, String> {
    check_s3_uri("source dataset", &request.source_s3)?;
    check_s3_uri("output location", &request.output_s3)?;
    let (code, source_dir) = script_location(&request.script)?;
    let file_name = input_file_name(&request.source_s3);
    Ok(ProcessingPlan {
        code,
        source_dir,
        arguments: vec![
            "--input".to_string(),
            join(INPUT_LOCAL_PATH, file_name),
            "--output".to_string(),
            join(OUTPUT_LOCAL_PATH, file_name),
        ],
        source_s3: request.source_s3.clone(),
        output_s3: request.output_s3.clone(),
        instance_type: request.instance_type.clone(),
        base_job_name: request.base_job_name.clone(),
        region: request.region.clone().filter(|region| !region.is_empty()),
        execution_role: request
            .execution_role
            .clone()
            .filter(|role| !role.is_empty()),
        image_uri: request.image_uri.clone().filter(|image| !image.is_empty()),
    })
}

/// Submits the job (`execute_transformation_job`) and returns the confirmation
/// line. A missing role, region or image is resolved through the service in
/// the order of the Python function.
pub(crate) fn submit(
    request: &TransformRequest,
    service: &dyn ProcessingService,
) -> Result<String, String> {
    let mut plan = build_plan(request)?;
    if plan.execution_role.is_none() {
        plan.execution_role = Some(service.default_role()?);
    }
    if plan.region.is_none() {
        plan.region = Some(service.default_region()?);
    }
    let region = plan.region.clone().unwrap_or_default();
    if plan.image_uri.is_none() {
        plan.image_uri =
            Some(service.sklearn_image(&region, SKLEARN_IMAGE_VERSION, &plan.instance_type)?);
    }
    let name = service.start(&plan)?;
    Ok(format!(
        "Processing job '{name}' submitted. Output will be at: {}",
        plan.output_s3
    ))
}

/// Describes a job (`describe_transformation_job`) as the script's dict.
pub(crate) fn describe(
    job_name: &str,
    region: Option<&str>,
    service: &dyn ProcessingService,
) -> Result<Json, String> {
    if job_name.is_empty() {
        return Err("job name must not be empty".to_string());
    }
    let details = service.describe(job_name, region.filter(|value| !value.is_empty()))?;
    Ok(Json::object(vec![
        ("job_name", Json::string(job_name)),
        ("status", optional_json(details.status.as_deref())),
        (
            "failure_reason",
            optional_json(details.failure_reason.as_deref()),
        ),
        ("creation_time", Json::string(details.creation_time)),
        (
            "processing_end_time",
            Json::string(details.processing_end_time),
        ),
        ("inputs", Json::Array(details.inputs)),
        ("outputs", Json::Array(details.outputs)),
    ]))
}
