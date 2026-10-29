# Slice `ai-payloads` (Issue #4386): design

Status: design only. No implementation code. Python in scope was read as
migration evidence and was not run.

Revision: **r1**, design repair 1 after pre-review #5119
(`CHANGES_REQUESTED` at `779a8ec0`). Findings #5122–#5129 are resolved in
place, and §11 maps each finding to the sections and test IDs that resolve it.
The PM rulings recorded on #5119 stand:

- no Python-free serverless mode in #4386 (OD-1);
- the Rust reward-Lambda template is follow-up #5121 (P-2);
- the two defective reward templates stay unchanged and are tracked as #5120
  (P-4), and `reward check` must flag both.

- Base: `fa3fa137186b7e0d7e9fc5aafc2a58f0d8de8ad4` (`origin/rolling` when this
  was written). Dependencies #4379 (`aws-core`), #4384 (`ai-models`) and #4385
  (`ai-data`) are closed and on rolling.
- Documentation checked: 2026-10-28, public pages only, no AWS calls.

## 0. Summary

The slice covers 28 canonical Python files and 28 byte-identical mirrors (56
inventory rows, 4,141 canonical lines). Reading them and the primary
documentation gives three facts that settle the design.

1. **23 of the 25 rows marked `external-service-payload` never run inside a
   managed service.** They are notebook or script *templates*. The customer's
   coding agent splits them on `# Cell N:` markers, fills placeholders, and
   writes a Jupyter notebook or a numbered `.py` script into the customer's
   project. The customer's own Python kernel then runs them through the
   SageMaker Python SDK v3. What runs inside SageMaker or Bedrock is AWS-owned
   (managed training recipes, serving containers, the import pipeline) or user
   code these files do not contain (`ENTRY_SCRIPT`, `SCRIPT_PATH`, a PySpark
   app).
2. **The 4 reward-function templates are the only true service payloads.**
   SageMaker training and evaluation invoke them as AWS Lambda functions. Two of
   them are mislabelled `toolkit-helper` in the inventory. Lambda accepts any
   runtime, Rust included, through an OS-only runtime. SageMaker and Nova bind
   the function by Lambda ARN.
3. **No repository tooling executes any file in scope.** A search of
   `.github/`, `mise.toml`, `tools/` and `crates/` finds no reference. This
   resolves inventory-audit U-2: only customers, or the customer's agent,
   execute these files.

What the design does:

- **Rust controllers.** A new `toolkit ai-payloads` command family takes over
  every deterministic step that the skill text now asks the customer's agent to
  do by hand:
  - template cataloguing and linting;
  - placeholder substitution with typed, escaped values;
  - nbformat notebook and `# %%` script emission, with safe file placement;
  - offline checks of the documented reward-Lambda contract.
- **Python kept, with a documented reason.** The template text stays Python
  because the customer's kernel and the SDK v3 are Python (AC3).
- **AC3 accounting.** Python rows removed: **0 of 56**. All 56 rows are kept,
  each with a documented disposition. None of them counts as Python removed.
- **New crate dependencies: none.**

## 1. Inventory

### 1.1 Scope and mirrors

The canonical root is `skills/core-skills/aws-ai-ml/references/`. Every
canonical file has a mirror at the same relative path under
`plugins/aws-core/skills/aws-ai-ml/references/`.

- At the base commit each pair is byte-identical (`cmp`).
- `python-inventory.json` lists each mirror with `canonical_identical_path`
  set to the canonical path.
- Mirrors are maintained by `toolkit sync plugin-skills` (#4377); they are
  never edited by hand.
- Every inventory row is `status: pending`.

Roles used below:

- **TPL**: a template users copy. The agent renders it into the customer's
  project, and the customer's Python kernel runs it on the client side.
- **LAMBDA**: a payload executed inside a managed service. The customer
  customizes it and deploys it as an AWS Lambda function, which SageMaker
  invokes.
- **REF**: a reference pattern. The agent reads it and writes its own code from
  it, which the customer's kernel runs.
- **CLI**: a command the user runs. **No file in scope has this role.**

### 1.2 File table

Line counts are for the canonical file. Callers were found with `git grep` on
the base commit. `overview.md` means the `overview.md` of the same reference
directory, and the number after a colon is the line.

| ID | Canonical path (under `references/`) | Lines | Inventory class | Role | Callers |
| --- | --- | --- | --- | --- | --- |
| T01 | `finetuning/code_templates/sft.py` | 160 | payload | TPL | `finetuning/overview.md`:80, 211; `finetuning/references/code_output_guide.md` |
| T02 | `finetuning/code_templates/dpo.py` | 170 | payload | TPL | `finetuning/overview.md`:81, 212; `continuous_customization.md` (append rounds) |
| T03 | `finetuning/code_templates/rlvr.py` | 184 | payload | TPL | `finetuning/overview.md`:82, 213; `finetuning/references/rlvr_reward_function.md`:162 |
| T04 | `finetuning/code_templates/rlaif_builtin.py` | 171 | payload | TPL | `finetuning/overview.md`:83, 216; `finetuning/references/rlaif_guide.md`:39 |
| T05 | `finetuning/code_templates/rlaif_custom_prompt.py` | 184 | payload | TPL | `finetuning/overview.md`:84, 217; `rlaif_guide.md`:85 |
| T06 | `finetuning/templates/rlvr_reward_function_source_template.py` | 251 | payload | LAMBDA | `finetuning/overview.md`:209; `rlvr_reward_function.md`:98 |
| T07 | `finetuning/templates/nova_rlvr_reward_function_source_template.py` | 358 | payload | LAMBDA | `finetuning/overview.md`:210; `rlvr_reward_function.md`:97 |
| T08 | `model-evaluation/scripts/reward_function_source_template.py` | 246 | **helper (wrong)** | LAMBDA | `model-evaluation/references/create-reward-function.md`:22, 66 |
| T09 | `model-evaluation/scripts/nova_reward_function_source_template.py` | 364 | **helper (wrong)** | LAMBDA | `create-reward-function.md`:21, 67 |
| T10 | `finetuning/scripts/mlflow_reference.py` | 27 | helper | REF | `finetuning/overview.md`:192 ("use as the pattern to query MLflow metrics") |
| T11 | `model-deployment/code_templates/deploy-jumpstart-sagemaker.py` | 177 | payload | TPL | `model-deployment/references/deploy-jumpstart-sagemaker.md`:172, routed from `model-deployment/overview.md`:203 |
| T12 | `model-deployment/code_templates/deploy-nova-bedrock.py` | 73 | payload | TPL | `references/deploy-nova-bedrock.md`:90; `overview.md`:202 |
| T13 | `model-deployment/code_templates/deploy-nova-sagemaker.py` | 74 | payload | TPL | `references/deploy-nova-sagemaker.md`:113; `overview.md`:201 |
| T14 | `model-deployment/code_templates/deploy-oss-bedrock.py` | 130 | payload | TPL | `references/deploy-oss-bedrock.md`:96; `overview.md`:200 |
| T15 | `model-deployment/code_templates/deploy-oss-sagemaker.py` | 84 | payload | TPL | `references/deploy-oss-sagemaker.md`:154; `overview.md`:199 |
| T16 | `model-deployment/code_templates/optimize-benchmark.py` | 114 | payload | TPL | `references/optimization/benchmark-workflow.md`:92; `interpreting-results.md`:46 |
| T17 | `model-deployment/code_templates/optimize-huggingface-to-s3.py` | 48 | payload | TPL | `references/optimization/huggingface-to-s3.md`:15, 21 |
| T18 | `model-deployment/code_templates/optimize-recommendation.py` | 144 | payload | TPL | `optimization/recommendation-workflow.md`:83; `recommendation-deploy.md`:3; `recommendation-options.md`:34, 38, 42 |
| T19 | `model-evaluation/code_templates/custom_scorer_evaluator.py` | 95 | payload | TPL | `model-evaluation/references/custom-scorer-evaluation.md`:181 |
| T20 | `model-evaluation/code_templates/llmaaj_evaluator.py` | 101 | payload | TPL | `model-evaluation/references/llmaaj-evaluation.md`:229 |
| T21 | `processing-jobs/code_templates/script_processing.py` | 99 | payload | TPL | `processing-jobs/overview.md`:32, 91; `processing-jobs/references/configuration.md`:3 |
| T22 | `processing-jobs/code_templates/framework_processing.py` | 101 | payload | TPL | `processing-jobs/overview.md`:33, 92 |
| T23 | `processing-jobs/code_templates/spark_processing.py` | 118 | payload | TPL | `processing-jobs/overview.md`:34, 93 |
| T24 | `processing-jobs/code_templates/dataset_definition_processing.py` | 127 | payload | TPL | `processing-jobs/overview.md`:35, 94 |
| T25 | `training-jobs/code_templates/recipe_training.py` | 163 | payload | TPL | `training-jobs/overview.md`:40, 108; `training-jobs/references/configuration.md`:3 |
| T26 | `training-jobs/code_templates/jumpstart_training.py` | 158 | payload | TPL | `training-jobs/overview.md`:41, 109 |
| T27 | `training-jobs/code_templates/custom_training.py` | 175 | payload | TPL | `training-jobs/overview.md`:42, 110 |
| T28 | `dataset-transformation/code_templates/transformation.py` | 45 | payload | TPL | `dataset-transformation/overview.md`:128, 157, 159, 163; `docs/migration/ai-data.md`:58 |

Totals:

| Group | Files | Lines | Removed by this slice |
| --- | --- | --- | --- |
| 23 TPL | T01–T05, T11–T28 | 2,895 | 0 |
| 4 LAMBDA | T06–T09 | 1,219 | 0 |
| 1 REF | T10 | 27 | 0 |
| Canonical | 28 | 4,141 | 0 |
| Including mirrors | 56 | 8,282 | 0 |

The 4,141 canonical lines are the inventory audit's 3,504 deduplicated payload
lines plus 637 helper lines.

`SKILL.md` names no file directly. It routes to each reference directory
(`SKILL.md`, lines 31–45), and the `overview.md` and reference pages above name
the files.

### 1.3 How the templates are consumed today

Four copies of `code_output_guide.md` exist, under `finetuning/`,
`model-deployment/`, `dataset-transformation/` and `model-evaluation/`. They
differ only in the pinned `sagemaker` version and in one extra section each.
The training-jobs and processing-jobs `configuration.md` pages give the same
rules in short form. The rules:

- Mode choice: the agent asks once, "Jupyter notebook or a Python script?".
- `# Cell N: Label` starts a section. `# Cell 0 [markdown]:` becomes a markdown
  cell.
- `# NOTEBOOK_ONLY` drops the line in script mode. `# NOTEBOOK_ONLY_SECTION` on
  a marker line drops the whole section in script mode.
- `[PLACEHOLDER]` values are replaced with the user's configuration. "Use
  EXACTLY the imports shown in each code template."
- Notebook mode:
  - writes `<project-dir>/notebooks/<project-name>.ipynb`;
  - asks whether to append to or replace an existing notebook, and uses Cell 0
    as the section divider when appending;
  - the agent must hand-write the JSON: 2-space indent, one string per source
    line ending in `\n` except the last, the
    `{"cells", "metadata", "nbformat": 4, "nbformat_minor": 4}` wrapper,
    `execution_count: null`, `outputs: []`;
  - "Do NOT use bash commands".
- Script mode:
  - writes `<project-dir>/scripts/NN_<descriptive_name>.py`, taking the next
    free number;
  - uses `# %%` section markers and a docstring at the top;
  - runs the script with `python3 <script>.py` in the customer's environment.

The hand-written JSON and the placeholder edits are the only deterministic
local logic in this workflow. They are what the Rust controller replaces (§4).

### 1.4 Template facts the controller must preserve

These facts come from reading every template:

- **Cell markers.** Every TPL file has `# Cell 0 [markdown]: ...` and markers
  numbered consecutively from 0 (4–9 per file). One comment line,
  `# Cell 2 is intentionally SDK-free ...` (T11:101), is *not* a marker: no
  `:` follows the number.
- **Install lines.** Install commands differ between groups:
  - T01–T05, T11–T20 have a commented line,
    `# %pip install ... # NOTEBOOK_ONLY`;
  - T21–T27 have a bare IPython magic, `%pip install ... # NOTEBOOK_ONLY`, so
    these files are not even valid Python syntax as written;
  - T28 has none.
- **Plotting section.** `# NOTEBOOK_ONLY_SECTION` occurs on the MLflow and
  matplotlib plotting cell of T01–T05.
- **Placeholders.** Placeholders appear in three contexts:
  - *quoted slots*, 61 distinct names, such as `"[ROLE_ARN]"`,
    `"[PROJECT_DIR]"` and `"[GENERATE A NAME FOR THE EVALUATOR HERE]"`;
  - *bare slots*, which are typed expressions. There are 9 names in 4 files:
    - `[INPUT_TOKENS_MEAN]`, `[INPUT_TOKENS_STDDEV]`, `[OUTPUT_TOKENS_MEAN]`,
      `[OUTPUT_TOKENS_STDDEV]`, `[CONCURRENCY]` and `[REQUEST_COUNT]`, in T16
      and T18 at lines 34–39;
    - `[METRICS_LIST]` (T20:31) and `[TRUE_OR_FALSE]` (T20:38);
    - `[ACCEPT_EULA]` (T15:28), which feeds `model_builder.accept_eula`
      (T15:44).

    §4.4.1 gives each name its own type;
  - *comment occurrences*, such as T16:51 and T18:117.
  `"[^A-Za-z0-9-]"` (T25–T27) is a regex, not a placeholder.
- **Sentinel occurrences.** T17:39 is
  `if _hf_token in ("", "None", "[HF_TOKEN_OR_NONE]"):`. A blind replace-all
  would turn a real token into the sentinel that disables it. T11:50 rejects
  any value starting with `[` as an unsubstituted placeholder.
- **Secrets.** `[HF_TOKEN_OR_NONE]` (T17:23) holds a secret.
  `huggingface-to-s3.md`:11 says "store it securely; never echo it back", and
  the template prefers a Secrets Manager ARN.
- **Empty fields.** Some configuration fields are literals for the user to
  fill, not placeholders. Examples:
  - `BASE_MODEL = ""` in T01–T05;
  - `REGION = ""` in T21–T27;
  - `CUSTOM_PROMPT_PATH = ""` in T05 Cell 3 (T05:50);
  - the parenthesized multi-line `ACCEPT_EULA = (\n    False  # ...\n)` in
    T04:36–38 and T05:38–40.

  97 top-level literal assignments of this kind exist across the 23 TPL files.
  §4.4.3 defines which of them `assign` may set.
- **Manual edit points.** 27 lines in 7 templates carry `NOTE FOR AGENT` or
  `Uncomment` instructions. Examples: remove the `ACCEPT_EULA` lines for
  non-Meta models (T15:25–27, :42), and uncomment `# accept_eula=ACCEPT_EULA`
  (T01–T05). T11's EULA consent is the dict member `"accept_eula": False`
  (T11:22), not a slot. These stay manual steps (§4.4.3).
- **Generated module (T28).** T28 Cell 2 is generated per dataset, and the
  template requires it to be a separate module:
  - T28:16–17: "In notebook mode, it uses %%writefile to save the function to
    transform_fn.py. In script mode, the function is written to disk
    directly."
  - T28:28: Cell 3 imports `from transform_fn import transform_dataset`.
  - The caller writes and tests `<project-dir>/scripts/transform_fn.py` in its
    Step 7, *before* rendering (`dataset-transformation/overview.md`:128–145).
  - `dataset_transformation_code.md`:26 and :41: "Both files must be in the
    same directory (`<project-dir>/scripts/`)".

  §4.4.4 specifies how this is handled.

### 1.5 Python not in this inventory

Twelve aws-ai-ml markdown pages contain 24 fenced `python` blocks, for example
`custom-lambda-scorer.md` (3) and `continuous_customization.md` (5). They are
customer guidance with the same customer-language disposition as the TPL group.
They are not `.py` inventory rows and stay outside this slice. §8 proposes
recording them (P-7).

## 2. Managed-service language contracts (AC1)

Every entry was read from public AWS or vendor documentation on 2026-10-28.
Anchors are the page's own section ids.

| # | Service / surface | What runs there | Accepted languages and entry points (documented) | Source (URL, section) |
| --- | --- | --- | --- | --- |
| C1 | SageMaker Python SDK v3: trainers (`SFTTrainer`, `DPOTrainer`, `RLVRTrainer`, `RLAIFTrainer`), `ModelBuilder`, `BedrockModelBuilder`, AI registry `DataSet` and `Evaluator`, `sagemaker.train.evaluate` | the customer's kernel (client side) | Python only. Model customization is "available only in SageMaker Python SDK V3". | <https://sagemaker.readthedocs.io/en/stable/model_customization/index.html> (page intro) |
| C2 | SageMaker serverless model customization | AWS-managed training recipes and containers; no customer code except reward Lambdas | Interfaces: "APIs available through the SageMaker AI Python SDK", Studio UI, or agent-guided skills. The public `CreateTrainingJob` API has `ServerlessJobConfig` (`BaseModelArn`, `CustomizationTechnique`, `EvaluationType`, `EvaluatorArn`, `JobType`, `Peft`, `SequenceLength`, `AcceptEula`), `MlflowConfig` and `ModelPackageConfig`, so the service is callable from any AWS SDK, Rust included. | <https://docs.aws.amazon.com/sagemaker/latest/dg/customize-model.html> ("Key concepts", "User Interfaces"); <https://docs.aws.amazon.com/sagemaker/latest/APIReference/API_CreateTrainingJob.html> ("Request Syntax", `ServerlessJobConfig`) |
| C3 | SageMaker training container (T25–T27) | the training image | Any executable. SageMaker runs `docker run image train`, or `ContainerEntrypoint`/`ContainerArguments` from `AlgorithmSpecification`. The exec-form `ENTRYPOINT` receives SIGTERM, with 120 s before SIGKILL. Shell and Python are only examples. | <https://docs.aws.amazon.com/sagemaker/latest/dg/your-algorithms-training-algo-dockerfile.html> (`#your-algorithms-training-algo-dockerfile-api-ep-in`, `#your-algorithms-training-algo-dockerfile-api-pass-ep`) |
| C4 | SageMaker Processing container (T21, T22, T24) | the processing image | Any image and entry point: `AppSpecification.ContainerEntrypoint`/`ContainerArguments`. Configuration comes from `/opt/ml/config/processingjobconfig.json` and `resourceconfig.json`; data under `/opt/ml/processing`. `ScriptProcessor` and `FrameworkProcessor` choose `python3` themselves; the service does not require it. | <https://docs.aws.amazon.com/sagemaker/latest/dg/build-your-own-processing-container.html> (`#byoc-config`); <https://docs.aws.amazon.com/sagemaker/latest/dg/processing-container-run-scripts.html> |
| C5 | SageMaker Spark processing (T23) | AWS prebuilt Spark images | A PySpark script (`PySparkProcessor`) or a Spark JAR (`SparkJarProcessor`) | <https://docs.aws.amazon.com/sagemaker/latest/dg/use-spark-processing-container.html> |
| C6 | SageMaker real-time inference (T11, T13, T15, T18 deploy) | AWS serving containers chosen by the SDK; no customer code from these templates | Any language for custom containers: `docker run image serve`, a web server on port 8080 with `POST /invocations` and `GET /ping` (2 s ping timeout, 8 min to healthy) | <https://docs.aws.amazon.com/sagemaker/latest/dg/your-algorithms-inference-code.html> (`#your-algorithms-inference-code-run-image`, `#your-algorithms-inference-code-container-response`, `#your-algorithms-inference-algo-ping-requests`) |
| C7 | Bedrock Custom Model Import (T14) | the AWS import pipeline; no customer code | Hugging Face weights: `.safetensors`, `config.json`, `tokenizer_config.json`, `tokenizer.json`, `tokenizer.model`, in supported architectures. Model files are data, not code. | <https://docs.aws.amazon.com/bedrock/latest/userguide/model-customization-import-model.html> ("Supported architectures", "Import a model source from Amazon S3") |
| C8 | Bedrock `CreateCustomModel` (T12, Nova) | AWS pipeline | A SageMaker model package ARN (`customModelDataSource`) or an S3 artifact URI (`modelSourceConfig`); no code | <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_CreateCustomModel.html> (intro, "Request Body") |
| C9 | AWS Lambda (T06–T09) | the customized reward function | Lambda "is agnostic to your choice of runtime". Managed runtimes: Node.js, Python, Java, .NET, Ruby. "To use other languages in Lambda, such as Go or Rust, use an OS-only runtime" (`provided.al2023`, deprecation 2029-06-30). Rust: "use the Rust runtime client ... deploy it to Lambda using an OS-only runtime". | <https://docs.aws.amazon.com/lambda/latest/dg/lambda-runtimes.html> (intro, `#runtimes-supported`); <https://docs.aws.amazon.com/lambda/latest/dg/lambda-rust.html> |
| C10 | SageMaker custom scorers and RLVR reward Lambdas (T06, T08) | Lambda, invoked by the eval or RFT container | "You must implement your custom scorer as an AWS Lambda function". Input: "The container invokes your Lambda once per sample, passing a list that contains a single sample object. Your Lambda must iterate the list, but currently it contains exactly one item per invocation." The sample is shaped like the dataset format (OpenAI, verl, HF prompt-completion, HF preference, SageMaker Eval; per-format fields in §4.6.1). `id` is "Present for the OpenAI, Hugging Face, and SageMaker AI Evaluation formats; for verl it appears only if set in the dataset entry". Output: Option A, a raw list of `{id, aggregate_reward_score, metrics_list?}`; or Option B, `{"statusCode": 200, "body": "<JSON string of that list>"}`, where `body` "must be a JSON string (not a nested JSON object)". A non-200 status means the sample "is counted in `byoc_failure_count` and its custom metrics are dropped". Each `id` must match its input, and `metrics_list[].type` is `"Reward"` or `"Metric"`. On a payload matching neither envelope, the page's Note says: "SageMaker AI cannot parse the reward response. In RFT workflows this commonly surfaces as `InvalidLambdaResponseException`." The page lists common causes: an object instead of a string `body`, an omitted `id`, a result count different from the input count, and unsupported `metrics_list` field names. The execution role needs `lambda:InvokeFunction`. No runtime is mandated. | <https://docs.aws.amazon.com/sagemaker/latest/dg/model-customize-evaluation-preset-custom-scorers.html> (`#model-customize-evaluation-custom-scorers-lambda-input` and its five per-format subsections, `#model-customize-evaluation-custom-scorers-lambda-output` including its Note, `#model-customize-evaluation-custom-scorers-fields`, `#model-customize-evaluation-custom-scorers-permissions`); <https://docs.aws.amazon.com/sagemaker/latest/dg/customizing-models-rft.html> (`#technique-rlvr`) |
| C11 | Nova reward Lambdas (T07, T09) | Lambda, invoked by Nova RFT and evaluation | "It must be implemented as an Lambda function". Input: an OpenAI-format sample with a container-added string `id`, `messages`, and dataset fields such as `reference_answer` passed through. The documentation shows the input in two shapes:<br>- "Sample payload for the reward lambda" shows one JSON **object**;<br>- the contract handler passes `event` straight to `lambda_grader(samples: list[dict])`, a **list**;<br>- the LLM-judge example normalizes both with `samples = event if isinstance(event, list) else [event]`.<br>The grader returns a list of `{id, aggregate_reward_score, metrics_list?}`, and the example handler returns `{"statusCode": 200, "body": json.dumps(results)}`. Bound by Lambda ARN in the recipe field `reward_lambda_arn`. 15 min timeout; must handle `rollout_worker_replicas * 64` concurrent calls. No runtime is mandated. | <https://docs.aws.amazon.com/nova/latest/nova2-userguide/nova-implementing-reward-functions.html> (`#nova-reward-interface`, `#nova-reward-input-fields`, `#nova-reward-constraints`, `#nova-reward-using-custom`, `#nova-reward-llm-judge-implementation`) |
| C12 | AI registry `Evaluator.create(type=REWARD_FUNCTION, source=...)` | client SDK call | `source` is a Lambda ARN ("Method: Lambda") or a local `.py` path ("Method: BYOC"). The docs do not say what happens to a local file, or which runtime it gets. | <https://sagemaker.readthedocs.io/en/stable/v3-examples/model-customization-examples/serverless/ai_registry_example.html> |
| C13 | SageMaker managed MLflow (T10; T01–T05 Cell 7/8) | a tracking server or MLflow App | The documented client is the MLflow **Python** client plus the `sagemaker-mlflow` plugin, which "authenticates API calls made with MLflow using AWS Signature Version 4" and accepts the server ARN as the tracking URI. MLflow's own REST API is language-neutral: `GET /api/2.0/mlflow/runs/get`; `GET /api/2.0/mlflow/metrics/get-history` with `run_id`, `metric_key`, `max_results`, `page_token`. `DescribeMlflowTrackingServer.TrackingServerUrl` is documented only as "the URL to connect to the MLflow user interface". No AWS page found names the SigV4 signing scope or the REST endpoint for non-Python clients. | <https://docs.aws.amazon.com/sagemaker/latest/dg/mlflow-track-experiments.html> (intro, `#mlflow-track-experiments-install-plugin`); <https://mlflow.org/docs/latest/api_reference/rest-api.html> ("Get Run", "Get Metric History"); <https://docs.aws.amazon.com/sagemaker/latest/APIReference/API_DescribeMlflowTrackingServer.html> (`TrackingServerUrl`) |
| C14 | Jupyter notebook format (render output) | the customer's Jupyter | nbformat 4: top-level `metadata`, `nbformat`, `nbformat_minor`, `cells`. Cells have `cell_type`, `metadata`, `source`; code cells also `execution_count` (integer or null) and `outputs`. Multi-line strings "may be written as a list of strings". Cell `id` (1–64 chars, unique) is required from nbformat 4.5. | <https://nbformat.readthedocs.io/en/latest/format_description.html> ("Top-level structure", "Cell Types", "Multi-line strings", "Cell ids") |

**Not verified.** The Bedrock RFT reward page
(<https://docs.aws.amazon.com/bedrock/latest/userguide/nova-rft.html>) returned
only its title when fetched. No file in scope uses Bedrock RFT jobs:
T12 and T14 deploy and import, and do not customize. Nothing depends on that
page. `CreateCustomModelDeployment` (used by T12 through the SDK) was not
fetched separately either.

**C10 evidence for the `InvalidLambdaResponseException` claim (r1, #5129).**

- The exception is named in exactly one place: the **Note** at the end of the
  "Sample Lambda Output Payload" section
  (`#model-customize-evaluation-custom-scorers-lambda-output`) of
  <https://docs.aws.amazon.com/sagemaker/latest/dg/model-customize-evaluation-preset-custom-scorers.html>.
  It is not on `customizing-models-rft.html`.
- Method, 2026-10-28: the page's raw HTML was fetched with `curl` and searched
  with `grep`. It holds one occurrence (the fetched bytes have SHA-256
  `c98c83cdbdd6d19b97a487d4ad138c5505cfac6e9e5b99ff1e4a58e5cdb5030a`).
- Quoted text: "If your Lambda returns a payload that does not match one of
  these two envelopes, SageMaker AI cannot parse the reward response. In RFT
  workflows this commonly surfaces as `InvalidLambdaResponseException`."
- Pre-review #5119 did not find the sentence. A converted or truncated
  rendering of the page probably dropped the Note block. This is inferred.
- The wording "commonly surfaces" is not a guarantee, so the design treats the
  exception name as documented-but-indicative.
  - `reward check` diagnostics never promise that a service raises it. They
    say "matches neither documented envelope (C10 Option A/B)".
  - The service-side effect is stated only where the page states it: the
    non-200 `byoc_failure_count` behaviour.

### 2.1 Consequences

- **TPL (23 files): no managed-service language contract applies to the files
  themselves.** Their language is fixed by the client stack the product chose:
  the SageMaker Python SDK v3 (C1) running in a Jupyter Python kernel or under
  `python3`. The services they drive are language-neutral (C2–C8), so a
  Python-free client is possible in principle through `CreateTrainingJob` with
  `ServerlessJobConfig` and the other APIs. That would replace the product's
  notebook UX, and it is a product choice, not a migration step (§8 P-1, §9
  OD-1).
- **LAMBDA (4 files): the service accepts any Lambda runtime** (C9–C11).
  Python is the customer's choice in these starting templates. It also matches
  the SDK's "BYOC" local-file path, whose behaviour is undocumented (C12). The
  templates stay Python. A Rust reward-Lambda template would be a product
  addition (§8 P-2).
- **REF (T10): the only documented client is Python** (C13). A Rust replacement
  needs the SigV4 scope and endpoint for SageMaker MLflow REST access, which
  were not found in primary documentation (§8 P-3).

### 2.2 Template defects found while checking the contracts

These are product-content defects. Existing Python is read-only migration
evidence, so this slice does not fix them. They become proposed Issues, and the
reward checker (§4.6) makes the first two detectable offline.

- **D-1.** T09 returns `{"statusCode": 200, "body": results}` with `body` as a
  parsed list. Its own comment reads "Must be a parsed list, NOT json.dumps()".
  C10 Option B requires `body` to be a JSON *string*, and C11's example uses
  `json.dumps`. T08, its open-weights sibling, does use a string. (P-4, filed
  as #5120.)
- **D-2.** T06's `lambda_handler` returns `{"error": ...}` with no `statusCode`
  and no results, both for an empty batch and for the first failing sample. Such
  a payload matches neither C10 envelope. The page says SageMaker "cannot parse
  the reward response", which in RFT "commonly surfaces as
  `InvalidLambdaResponseException`" (§2, C10 evidence note). The page also
  names "returning a different number of results than inputs" as a cause. T07
  instead returns score 0.0 for each failed sample. (P-4, filed as #5120.)
- **D-3.** The install lines are inconsistent, commented in some templates and
  bare in others (§1.4). A commented `# %pip` line never runs in notebook mode.
  (P-5)
- **D-4.** Inventory classes are wrong for T08 and T09, and inaccurate for the
  TPL group (§1.2). (P-6)
- **D-5 (r1, found while resolving #5124).** In notebook mode, T28's
  `from transform_fn import transform_dataset` (T28:28) does not resolve unless
  `<project-dir>/scripts/` is on the kernel's `sys.path`.
  - The notebook lives in `<project-dir>/notebooks/` (§1.3), and the module in
    `<project-dir>/scripts/`. Neither the template nor the guide adds that
    directory to `sys.path`.
  - The guide's own test command does add it, with
    `sys.path.insert(0, '<project-dir>/scripts')` (overview.md:137).
  - Script mode is unaffected: Python puts the running script's directory first
    on `sys.path`, and the script and the module share `scripts/`.
  - That Jupyter starts the kernel in the notebook's directory is Jupyter's
    usual default. It is inferred and was not verified against a primary page
    in this repair.
  - The template text is read-only evidence, so `render` adds no code to fix
    this. (P-8)

## 3. Dispositions (AC2, AC3)

Disposition codes:

- **R-CTRL**: a Rust controller replaces the local, deterministic part of the
  workflow. The template text stays as the customer-language payload (see
  K-PY).
- **K-PY**: kept as Python text, with a documented disposition. *Not* counted
  as Python removed (AC3).
- **RM**: removed. **No file qualifies.** Every file has live callers and a
  product role. Renaming `.py` to a template extension would hide Python
  without removing it, which AGENTS.md forbids ("Do not delete functionality to
  satisfy a file-extension check"); see OD-4.

| Files | Disposition | Rust lands in | Reason |
| --- | --- | --- | --- |
| T01–T05, T11–T28 (23 TPL) and their mirrors | **R-CTRL + K-PY.** The controller replaces cataloguing, placeholder substitution, notebook JSON and script emission. The template text stays Python. | `crates/toolkit-cli`, module `ai_payloads`, slice `ai-payloads`: `toolkit ai-payloads templates` and `toolkit ai-payloads render` | The client is the Python SDK v3 in the customer's kernel (C1). No managed service runs these files (§2.1). The language-neutral APIs (C2) permit a separate Python-free product mode, which is proposed and not done (P-1). |
| T06–T09 (4 LAMBDA) and their mirrors | **R-CTRL (contract checker) + K-PY.** The Rust part generates the documented sample events and checks a captured Lambda response against the documented envelopes, offline. | `toolkit ai-payloads reward sample` and `toolkit ai-payloads reward check` | Lambda accepts any runtime (C9). The templates are customer-edited starting code, bound by ARN (C10, C11) or by an undocumented local-file path (C12). Python is the customer's choice, not a toolkit runtime. |
| T10 (REF) and its mirror | **K-PY**, with no Rust replacement in this slice | none (see P-3) | The only documented SageMaker MLflow client is Python with SigV4 signing by the plugin (C13). A Rust client would need an undocumented signing scope and endpoint. |

AC3 statement, to be repeated in `python-inventory.json` (S5):

> 56 rows: 0 removed and 56 retained with documented disposition
> (46 TPL, 8 LAMBDA, 2 REF, mirrors included). No row may be marked as
> converted to Rust. The R-CTRL commands replace agent-side manual steps, not
> the Python text.

## 4. Rust design

### 4.1 Placement and reused seams (#4379, #4384, #4385)

**Module.** `crates/toolkit-cli/src/ai_payloads/` holds `mod.rs`,
`template.rs`, `types.rs` (the per-name type table, §4.4.1), `values.rs`,
`assign.rs` (§4.4.3), `notebook.rs`, `script.rs`, `place.rs` (publication,
§4.5.1), `reward.rs` and `tests/`. It registers one `crate::registry::Slice` named
`ai-payloads`, with `synopsis_rank`/`description_rank` set to `NEW_RANK`, `doc`
set to `docs/migration/ai-payloads.md` (the slice's parity document, written in
S2b), and one inserted line in `SLICES`. No new crate.

| Need | Reused seam | Origin |
| --- | --- | --- |
| Command routing, flags, usage exit 2 | `CommandSpec` (`value_flags`, `strict_values: true`, `repeatable_flags` for `--cell`), `Invocation::values_of`, `Output::diagnostic`, `Output::set_exit_status` | toolkit-cli registry #4576, as used by `ai_data` and `ai_models` |
| Bounded, symlink-refusing reads | `toolkit_core::paths::read_regular_file` (1 MiB `MAX_TEXT_FILE_BYTES` for templates, values and cell files), `open_regular_file` | toolkit-core; used by #4384 and #4385 |
| Containment of output paths | `toolkit_core::paths::resolve_within`, `normalize_relative` | toolkit-core |
| Safe directory creation and writes | `toolkit_core::treesync::ensure_dir_chain`, `copy_file_atomic` pattern; `paths::write_regular_file` (refuses symlinks and non-regular files) | toolkit-core (#4377) |
| JSON parse and emit | `toolkit_core::json::{parse_json, Json, Json::to_pretty_string}` (2-space, insertion-ordered objects, duplicate keys rejected); `escape_json_string` | toolkit-core |
| Typed optional members of the values and reward JSON | `toolkit_aws::recorded::{optional_member, optional_text, optional_list, optional_object_list, object_members, list_value}` (absent means default; present with the wrong type, `null` included, is rejected) | toolkit-aws #4379 / #4727, used by #4384 |
| Untrusted text in diagnostics | `toolkit_core::diag::{escape_untrusted, quote_untrusted, is_unsafe_char}`, `crate::safe_text::{safe_path, safe_quoted}` | toolkit-core, toolkit-cli |
| Secret scrubbing of any echoed text | `toolkit_aws::redact::redact_text` | toolkit-aws #4379 |
| Content hash for the append compare-and-swap and for cell ids | `toolkit_core::digest::sha256_hex` (in-tree FIPS 180-4, no dependency) | toolkit-core |
| No-replace publication and the append lock | `std::fs::hard_link` (POSIX `link(2)`, which fails with `EEXIST`), `std::fs::rename`, `std::fs::File::try_lock` (stable since Rust 1.89; the pinned toolchain is 1.94.1) | std only; see §4.5.1 and OD-11 |
| Hostile-argument contract | `crates/toolkit-cli/tests/diag_contract.rs` runs every registered command, and the new ones must pass it unchanged | toolkit-cli |

Not used, on purpose:

- the `toolkit-aws` `sdk` feature, `ApiClient`, `RecordedClient` and the
  `recorded_contract.rs` client rules (#4751), because no command in this slice
  calls AWS;
- `ai_data`'s `ObjectStore` and `ai_models`'s `SageMakerClient`, which belong to
  their slices' live-binding follow-ups (#4597, #4719).

**New crate dependencies: none.** `toolkit_core::json` already parses and emits
the notebook JSON. Template scanning is a hand-written line and token scanner,
as in #4385, which uses no regex, so the existing `fancy-regex` is not needed
either. `serde_json` is already a toolkit-cli dependency, but nothing here
needs it.

### 4.2 Commands

```text
toolkit ai-payloads templates [--root DIR] [--json] [--check]
toolkit ai-payloads render TEMPLATE --mode notebook|script --project-dir DIR
        [--name NAME] [--values FILE] [--cell N=FILE]... [--append | --new]
        [--consent NAME]... [--allow-unresolved] [--dry-run]
toolkit ai-payloads reward sample --format FORMAT
toolkit ai-payloads reward check --format FORMAT --event FILE --response FILE [--batch]

FORMAT = openai | verl | hf-prompt-completion | hf-preference | sagemaker-eval | nova
```

Exit codes: 0 success; 1 data, refusal or runtime error; 2 usage error (shared
router). Diagnostics name placeholders, cells and positions, never values.

**`templates`.**

- Lists every `references/*/code_templates/*.py` under `--root`. The default
  root is the directory holding the skill's `references/`.
- For each template it reports:
  - its path and title (the Cell 0 label);
  - its cells (number, label, whether markdown, whether
    `NOTEBOOK_ONLY_SECTION`);
  - its placeholders (name, kind `string`/`value`/`secret`, code-occurrence
    count, comment-only flag, sentinel count).
- `--check` lints the grammar of §4.3 and exits 1 on any violation.

**`render`.**

- Reads one template and a values file:

  ```text
  {"values": {"NAME": <json>}, "assign": {...}}
  ```

  `values` fills placeholder slots (§4.4, typed by §4.4.1). `assign` sets
  literal configuration fields (§4.4.3; OD-3).
- `[PROJECT_DIR]` defaults to the absolute `--project-dir`.
- Writes exactly one output: the notebook, or the next numbered script. It is
  published atomically and never replaces an existing file, except through the
  `--append` compare-and-swap (§4.5.1).
- Never writes any other file. T28's `transform_fn.py` module is a caller-owned
  prerequisite that `render` reads and does not write (§4.4.4).
- After success, prints the template's catalogued manual edit points (§4.4.3)
  so the caller cannot mistake a render for a complete configuration.
- `--consent NAME` records explicit consent for a consent-typed name that is
  set to `true` (§4.4.1).
- `--dry-run` prints the output path and a size, and writes nothing.

**`reward`.** Offline helpers for the C10 and C11 contracts (§4.6).

### 4.3 Template grammar (the contract `templates --check` enforces)

1. **Marker.** A line matching `# Cell <n>[ [markdown]]: <label>` exactly:
   - the line starts with `# Cell `;
   - then decimal digits;
   - then an optional ` [markdown]`;
   - then `: ` and a non-empty label.
   An optional trailing `  # NOTEBOOK_ONLY_SECTION` is not part of the label.
   Numbers start at 0 and increase by one. Cell 0 must be `[markdown]`, and no
   other cell may be. A line such as `# Cell 2 is intentionally ...` is
   ordinary text.
2. **Section content.** A section runs from the line after its marker to the
   line before the next marker. Leading and trailing blank lines are trimmed;
   inner lines are kept byte for byte. Cell 0's content is its comment lines
   with the leading `# ` removed.
3. **`NOTEBOOK_ONLY`.** A line ending in `# NOTEBOOK_ONLY` (after trailing
   whitespace is stripped) is dropped in script mode. In notebook mode it is
   kept verbatim (OD-2).
4. **Placeholders.** A placeholder is `[` + `A-Z` + `[A-Z0-9_ ]*` + `]`, with no
   leading `^` and no lowercase letters. Each occurrence is classified with a
   minimal Python lexer that tracks strings (including triple-quoted strings),
   comments and bracket depth:
   - *quoted slot*: a string literal whose entire body is the placeholder
     (`"[X]"` or `'[X]'`);
   - *bare slot*: the placeholder as an expression token in code;
   - *comment*: inside a `#` comment;
   - *sentinel*: a quoted occurrence that is an operand of `==` or `!=`, or an
     element of the bracketed right operand of `in` or `not in` (T17:39).
   Placeholders inside other string literals (f-strings, longer text) are a
   lint error, so a later template edit cannot introduce an unescapable slot.
5. **Secret placeholders.** A name with a `_`-delimited segment `TOKEN`,
   `SECRET`, `PASSWORD`, `CREDENTIAL` or `CREDENTIALS` is a secret. Today that
   is only `HF_TOKEN_OR_NONE`. `INPUT_TOKENS_MEAN` is not a secret, because
   `TOKENS` is not the segment `TOKEN`.
6. **Typed bare slots (r1).** Every bare-slot name must have an entry in the
   type table (§4.4.1). An untyped bare slot is a lint error, so a later
   template edit cannot add a bare slot that silently accepts any literal.
7. **Source text (r1).** Template files are checked under the source-text
   policy of §4.4.2, not the scalar policy used for substituted values:
   - LF or CR LF line endings, with tab allowed;
   - any other control, bidi or format character is a lint error, reported by
     line and column.

### 4.4 Substitution rules

- **Quoted slot.** The value must be a JSON string. Rejected: a newline, any
  `is_unsafe_char` character (C0/C1 controls, bidi and format characters), and
  more than 4,096 characters. The value is written as the body of a Python
  string literal of the slot's quote character, with `\` and that quote
  escaped. A JSON bool or number is rejected, so a string `"false"` cannot
  pretend to be a bool (compare T11:77–84).
- **Bare slot (r1, #5122).** There is no generic bare-slot union. Each bare-slot
  name has exactly one type in the closed table of §4.4.1. A value is accepted
  only if its JSON type and range match that entry. It is written as the
  Python literal of that type: `True`/`False`, a decimal integer, or a list of
  escaped string literals. A JSON string is never accepted in a bare slot, so no
  raw code is ever spliced in.
- **Secret slot.**
  - It accepts only `null`, the string `"None"`, or a Secrets Manager ARN
    (`arn:aws[a-z-]*:secretsmanager:<region>:<12 digits>:secret:...`).
  - `null` replaces the whole quoted literal with `None`. `"None"` and an ARN
    are written as escaped string bodies.
  - A literal token is refused (exit 1), and the error message never quotes it.
  - Without a value the slot stays unresolved; the template treats the
    placeholder text as "no token" (T17:39).
- **Comment and sentinel occurrences** are never substituted.
- **Unresolved placeholders.** By default, any code slot left without a value
  is an error. The message lists the slot names and the exit status is 1.
  `--allow-unresolved` writes the placeholder text unchanged; the template's
  own guard (T11:50) and the guide's "Fill every bracketed placeholder before
  running" still apply.
- **Unknown values.** A value key that no slot uses is an error. It is likely a
  typo, and an unused secret must not be written anywhere.
- **`--cell N=FILE`** replaces the body of section N with the file's content.
  - The file is bounded (1 MiB) and symlink-refusing.
  - It must be UTF-8 that satisfies the source-text policy (§4.4.2).
  - Section 0, unknown numbers and duplicate `N` are refused.
  - For T28 it is the module-cell route of §4.4.4.

#### 4.4.1 Per-name types (r1, #5122)

`types.rs` holds one closed table. Every bare-slot name and every consent-like
`assign` target has exactly one row. Matching is exact on the name, and the
table is the only place a type comes from. JSON types are strict:

- a JSON bool is not a number;
- a number is not a bool;
- `null` is accepted only where the row says so;
- an *integer* is a JSON number token with no fraction and no exponent, so
  `512` is an integer and `512.0` and `5.12e2` are refused, as is any value
  outside `i64`.

| Name (where) | Type | Accepted | Refused (examples, each with its own negative test) |
| --- | --- | --- | --- |
| `ACCEPT_EULA` (T15:28 slot; `assign` target in T01–T05, T26) | **consent bool** | exactly `true` or `false`. `true` also requires `--consent ACCEPT_EULA` on the command line, otherwise exit 1 `consent_not_given`. `false` needs no flag. | `"true"`, `"false"`, `1`, `0`, `null`, `[]`, `[true]`, `["false"]`, `{}` |
| `TRUE_OR_FALSE` (T20:38 → `EVALUATE_BASE`) | **flag bool** | exactly `true` or `false` | `"false"`, `1`, `null`, `["false"]`, `{}` |
| `INPUT_TOKENS_MEAN`, `OUTPUT_TOKENS_MEAN` (T16, T18) | integer | 1 ≤ n ≤ 2³¹−1 | `0`, `-1`, `512.0`, `true`, `"512"`, `null`, `[512]` |
| `INPUT_TOKENS_STDDEV`, `OUTPUT_TOKENS_STDDEV` (T16, T18) | integer | **0** ≤ n ≤ 2³¹−1. Zero is valid: a fixed length. | `-1`, `30.5`, `false`, `"30"`, `null` |
| `CONCURRENCY` (T16, T18) | integer | 1 ≤ n ≤ 2³¹−1 | `0`, `-4`, `1.5`, `true`, `"1"` |
| `REQUEST_COUNT` (T16, T18) | integer | 1 ≤ n ≤ 2³¹−1 | `0`, `100.0`, `"100"`, `null` |
| `METRICS_LIST` (T20:31 → `BUILTIN_METRICS`) | list of metric names | a JSON array of 0–64 strings. Each string matches `[A-Za-z][A-Za-z0-9]{0,63}`, and there are no duplicates. `[]` means "no built-in metrics": T20:56 tests `if BUILTIN_METRICS:` and llmaaj-evaluation.md:66 says "(if any)". | `null`, `"Correctness"`, `[1]`, `[""]`, `["a b"]`, `["X","X"]`, `[["X"]]` |

Domains and their evidence:

- **Booleans.** The consent and evaluation flags are passed straight to SDK
  parameters (`model_builder.accept_eula`, T15:44; `evaluate_base_model`,
  T20:51). In Python a non-empty list or string such as `["false"]` or
  `"false"` is truthy, as T11:77–84 itself warns. So only a real JSON bool is
  accepted.
- **Consent.** Explicit EULA consent stays the caller's step:
  - The guides ask the user, and T11:22 says "True ONLY after explicit
    acceptance".
  - `render` never infers, defaults or derives `true`.
  - `--allow-unresolved` leaves `[ACCEPT_EULA]` as text. In T15 that is the
    expression `[ACCEPT_EULA]`, which raises `NameError` before any SDK call,
    so it fails closed.
  - Typed dummy rendering (AP-CAT-10) uses `false` and never `true`.
  - The separate `--consent` flag makes acceptance a distinct, visible act in
    the command line. A values file that someone else generated cannot carry
    acceptance on its own. (B-11, OD-12.)
- **Workload integers.** The documented defaults are
  `benchmark-workflow.md`:53 and :113 (input 512, stddev 50, output 256,
  stddev 30, concurrency 1, request_count 100) and
  `recommendation-workflow.md`:110–115.
  - AIPerf's reference types `--concurrency` and `--request-count` as `int`
    (<https://docs.nvidia.com/aiperf/dev/benchmark-modes/load-generator-options-reference>,
    "Scheduling and Concurrency Options", "Stop Conditions"). It states no
    minimum.
  - The minima are therefore this design's reading, labelled **inferred**:
    - a mean, a concurrency or a request count of zero describes no workload;
    - a standard deviation of zero is mathematically valid (a fixed length),
      so zero is allowed there.
  - The 2³¹−1 ceiling only rejects absurd values. It is not a documented
    limit.
  - The S2b implementation review may tighten a row only with a primary
    citation.
- **Metric names.** `llmaaj-evaluation.md`:229 says `BUILTIN_METRICS` "must be
  a Python list of strings, e.g. `["Faithfulness", "Correctness"]`". The names
  listed in `llmaaj-builtin-evaluation.md`:33–48 are all `[A-Za-z]+`. The
  table does not enumerate them, so the skill stays their single source (OD-8)
  and the service validates membership.

#### 4.4.2 Source-text policy for cells and templates (r1, #5125)

`--cell` files, T28 module files (§4.4.4) and template files are *source text*,
not scalar values. They use their own predicate, `source_text_ok`. It is
distinct from both `is_unsafe_char`, which is the terminal and diagnostic
predicate (`toolkit_core::diag`, where LF, CR and tab count as controls), and
the quoted-slot scalar rule above.

- **Line endings.** LF and CR LF are accepted. CR LF is normalized to LF before
  emission, so the notebook `source` lines and the script bytes use LF. A CR
  not followed by LF is refused.
- **Tab** (U+0009) is accepted anywhere, for indentation or inside literals.
- **Refused anywhere:** every other character for which `is_unsafe_char` is
  true. That covers:
  - NUL and the other C0 controls;
  - ESC, DEL and the C1 controls, NEL (U+0085) included;
  - U+2028 and U+2029;
  - bidi marks, embeddings, overrides and isolates;
  - zero-width and joiner characters, the soft hyphen, U+FFF9–U+FFFB;
  - a BOM (U+FEFF), even at the start of the file.

  The rule is: per character, `c == '\t' || !is_unsafe_char(c)`, applied to
  the text after splitting on LF or CR LF.
- **Empty and final lines.** Blank lines and a missing final newline are
  accepted.
- **Diagnostics** name the file argument, the line and the column, plus the
  code point as `U+XXXX`. They never echo the line.
- **Scalar values** stay single-line: quoted slots still refuse LF, CR and tab
  (§4.4, quoted slot). Only whole source files are multi-line.

#### 4.4.3 `assign`: literal configuration fields (r1, #5128)

`assign` is accepted in S2b (OD-3) under these rules.

- **Targets.** A target is a name `N` for which all of these hold:
  1. The template has exactly **one** assignment statement to `N`, at any
     indentation, counting `N =`, `N +=` and tuple targets. This is checked by
     the §4.3 lexer. Two assignments, such as `_hf_token` at T17:38 and :40,
     make `N` unassignable.
  2. That statement is at bracket depth 0 and column 0, in a non-markdown
     section, and has one of two forms:
     - *single-line*: `N = <literal>`, optionally followed by a `#` comment;
     - *parenthesized*: `N = (`, then one line with `<literal>` and an
       optional comment, then a line holding only `)`. This is the
       T04:36–38, T05:38–40 and T03:41–43 form.
  3. `<literal>` is a non-f-string, single-line string literal (`""`, `''`
     or a non-empty one), a decimal integer, a float, `True`, `False` or
     `None`.
  4. The literal contains **no placeholder**. A target such as
     `INPUT_LOCATION = "[INPUT_LOCATION]"` must be filled through `values`.

  `templates --json` lists each template's assignable targets, with their
  section, line and literal kind. AP-CAT-02 freezes that list.
- **Types.**
  - A target with a row in §4.4.1 takes that row's type. Today that means
    `ACCEPT_EULA`, a consent bool that needs `--consent ACCEPT_EULA` for
    `true`.
  - Otherwise the type comes from the template's literal:

    | Template literal | Accepted value |
    | --- | --- |
    | string | a JSON string under the quoted-slot scalar rule, written in the original quote character |
    | `True`/`False` | a JSON bool (flag) |
    | integer | a JSON integer ≥ 0 |
    | float | a finite JSON number |

  - `None` targets (for example `OVERRIDE_INSTANCE_COUNT`, `CUSTOM_METRICS`)
    are **not** assignable unless §4.4.1 gives them a row. None has one in r1.
  - The S2b review may add `≥ 1` rows for counts and durations
    (`INSTANCE_COUNT`, `MAX_RUNTIME_SECONDS`, …), each with a primary API
    citation.
- **Secrets.** A target whose name contains a secret segment (§4.3 rule 5,
  matched case-insensitively) takes the secret-slot rule. Any other value is
  refused without being echoed. `HF_HUB_TOKEN` (T17:23) is therefore refused
  twice over:
  - its literal holds the `[HF_TOKEN_OR_NONE]` slot (rule 4 above);
  - its name has the `TOKEN` segment.

  The diagnostic points the caller to `values.HF_TOKEN_OR_NONE`. `_hf_token` is
  not a target (rule 1 above). The T17:39 sentinel is a comparison, not an
  assignment, so no `assign` input can reach it. That keeps B-4 for every
  route.
- **Merge order** (deterministic):
  1. Parse the template and the values file. Duplicate JSON keys are already
     refused by `parse_json`, so no name appears twice within `values` or
     within `assign`.
  2. Apply `--cell` replacements to section bodies.
  3. Resolve every `assign` target against the *unreplaced* sections:
     - a target that is unknown or unassignable gives exit 1,
       `assign_target_invalid`;
     - a target in a section replaced by `--cell` gives exit 1,
       `assign_target_replaced`.
  4. Type-check all `values` and `assign` entries. All errors are collected,
     sorted by name and reported together.
  5. Rewrite the spans: slot spans and assignment-literal spans are disjoint by
     rule 4 above, so the order cannot change the output.
  6. Emit and publish (§4.5).
- **Retained manual edits.** `assign` does not claim to complete a
  configuration. These stay manual and are listed by `render` after success
  (`manual: Cell N line L: <first 60 chars of the instruction>`, comment text
  only):
  - catalogued `NOTE FOR AGENT` and `Uncomment` lines (27 lines in 7
    templates, §1.4), for example removing T15's EULA lines for non-Meta
    models, or uncommenting `# accept_eula=ACCEPT_EULA`;
  - dict members, such as T11's `"accept_eula"`;
  - expression-valued fields such as `S3_OUTPUT_PATH = f"..."`;
  - `None` targets.

  S4 prose tells the caller to make these edits on the rendered file. `render`
  never overwrites a rendered file, and `--append` refuses a concurrent change
  (§4.5.1), so manual edits already in a notebook are kept.

#### 4.4.4 T28 module cell (r1, #5124)

T28 is the only template whose generated section is a separate Python module.
A small table in `template.rs`, keyed by the template's path relative to
`references/`, declares:

```text
dataset-transformation/code_templates/transformation.py:
  cell 2 is a module cell
  module path: scripts/transform_fn.py
  required definition: def transform_dataset(
```

AP-CAT-11 checks that this table still matches the template text: the
`transform_fn.py` comment at T28:16 and the `from transform_fn import
transform_dataset` line in Cell 3.

- **Who writes the module.** The caller does, in overview Step 7, before
  rendering. That step already "write[s] the file to disk for testing" and runs
  it (overview.md:128–145).
  - `render` **never writes** `transform_fn.py`, so it keeps its single output.
  - `render` reads the module through `--cell 2=FILE`. `FILE` must canonicalize
    (`resolve_within`, no symlinks) to exactly
    `<canonical project-dir>/scripts/transform_fn.py`. This keeps the notebook
    copy and the on-disk module the same bytes.
  - The module must satisfy §4.4.2 and contain a line starting with
    `def transform_dataset(`.
  - These are refused:
    - a missing `--cell 2` (`module_cell_required`);
    - another path (`module_path_mismatch`);
    - a missing or non-regular file (`module_missing`);
    - a file without the definition (`module_definition_missing`).
- **Notebook mode.** Cell 2 is one code cell. Its first source line is
  `%%writefile <canonical project-dir>/scripts/transform_fn.py`, followed by the
  module bytes.
  - IPython expands `$name` and `{expr}` in magic arguments and splits them on
    whitespace. That is IPython's documented magic behaviour, not re-fetched in
    this repair, so it is treated as a conservative assumption.
  - So the canonical path must match `[A-Za-z0-9._/+@,=-]+`. Otherwise exit 1
    `writefile_path_unsafe`, with nothing written. The caller can re-run with a
    project directory whose absolute path is plain.
- **Script mode.** The module is already on disk beside the script. Cell 2 is
  emitted as its marker plus one generated comment,
  `# transform_dataset is defined in scripts/transform_fn.py (imported in Cell 3).`
  The `%%writefile` line is never emitted in script mode, because it is not
  Python.
- **Import resolution.**
  - Script mode resolves: the script is `scripts/NN_<name>.py`, and Python puts
    the running script's directory first on `sys.path`.
  - Notebook mode does not resolve with the template as written (D-5). `render`
    keeps Cell 3 verbatim ("Use EXACTLY the imports shown"), and P-8 proposes
    the template and guide fix.
  - AP-T28-04 freezes the current notebook Cell 3, so the fix will show up as
    an intentional golden change.

### 4.5 Output and placement

**Notebook** (C14; §1.3 rules):

- Path: `<project-dir>/notebooks/<name>.ipynb`. `--name` must match
  `[a-z0-9][a-z0-9_-]{0,62}`; without it the project directory's base name
  must.
- Format: `{"cells": [...], "metadata": {...}, "nbformat": 4,
  "nbformat_minor": 4}`, two-space indent through `Json::to_pretty_string`.
  `source` is a list of lines, each ending in `\n` except the last. Code cells
  have `execution_count: null`, `metadata: {}` and `outputs: []`. Markdown
  cells have neither `execution_count` nor `outputs`.
- `metadata`: a Python 3 `kernelspec` and `language_info`, per OD-5.
- New notebooks are written as `nbformat` 4, `nbformat_minor` 4 (OD-5).
- An existing notebook is never overwritten silently:
  - `--append` parses it (cap 16 MiB, see OD-6). It appends the new cells,
    Cell 0 included as the section divider, and commits through the
    compare-and-swap of §4.5.1. Member order and values are preserved; the
    whitespace may change (B-9).
  - `--new`, and the default with neither flag, publish with no-replace
    (§4.5.1). An existing file is refused before any write, and a file that
    appears during the write is refused at commit.
  - With neither flag and an existing notebook: exit 1, with a message asking
    the user to choose. This mirrors the guide's question.
- **Supported versions for `--append` (r1, #5126).** `nbformat` must be the
  integer 4 and `nbformat_minor` an integer from 0 to 5. Anything else gives
  exit 1, `notebook_version_unsupported`, and the file is unchanged.
  `nbformat_minor` is preserved; there is never an upgrade or downgrade.
  - **4.0–4.4:** appended cells carry no `id`. Cell ids are not part of those
    schema versions (C14: ids are "required from nbformat 4.5"). An `id` already
    present on an existing cell is preserved unchanged, because the tool does
    not repair input.
  - **4.5:** every appended cell gets an `id`.
    - Every *existing* cell must already have an `id` that is a string matching
      `^[a-zA-Z0-9-_]+$`, 1–64 characters, and unique in the notebook (C14,
      "Cell ids"). Otherwise exit 1, `notebook_cell_ids_invalid`, with the file
      unchanged. The tool does not repair or renumber.
    - New ids come from an `IdSource`. In production it is `ContentIds`:
      `sha256_hex(h0 ‖ ":" ‖ k ‖ ":" ‖ attempt)`, first 8 hex characters. `h0`
      is the original file's hash (§4.5.1), `k` is the appended cell's index,
      and `attempt` starts at 0.
    - A candidate that collides with an existing or already-assigned id is
      retried with `attempt + 1`. After 64 attempts the tool gives exit 1,
      `cell_id_exhausted`.
    - `ContentIds` is deterministic, so the same input bytes give the same ids.
      Tests inject `FixedIds` sequences through the same trait to force
      collisions.

**Script:**

- Path: `<project-dir>/scripts/NN_<name>.py`, where `NN` is one more than the
  largest two-digit prefix among regular files in `scripts/` (`01` when there
  are none).
- Published with no-replace (§4.5.1). If a concurrent writer takes `NN` first,
  the commit fails with `EEXIST`. `NN` is then recomputed and the publication
  retried, up to 8 times, before exit 1 `script_number_contended`. A partially
  written script is never visible under its final name.
- Content, in order:
  - a module docstring built from Cell 0's label and text;
  - for each kept section, `# %% Cell N: Label` and its body;
  - `NOTEBOOK_ONLY` lines and `NOTEBOOK_ONLY_SECTION` sections are omitted.

**Safety:**

- Every path is resolved inside `--project-dir` with `resolve_within`, after
  the project directory itself is canonicalized.
- Parent directories are created with `ensure_dir_chain`, which refuses symlink
  components.
- Every write goes through §4.5.1. The only operation in the slice that can
  replace an existing file is the `--append` commit `rename`.
- File mode is the process default; nothing in the output is secret, because
  of the secret-slot rule.
- Path confinement is static, using `resolve_within` and `ensure_dir_chain`.
  It follows the toolkit-core threat model and makes no claim to resist a
  hostile process that swaps ancestor directories concurrently (#5119
  checklist).

#### 4.5.1 Atomic publication (r1, #5123)

`place.rs` has two primitives. Both write the full bytes to a temporary
sibling first:

- the temporary file is `.<final-name>.toolkit-render-<pid>-<counter>` in the
  destination directory;
- it is created with `create_new` (`O_CREAT|O_EXCL`), so it is neither an
  existing file nor a link;
- the bytes are written in full and then `sync_all` is called.

A write or flush failure, including a fault injected by the test seam, removes
the temporary file and leaves the destination exactly as it was: absent, or
holding the same bytes. The command then gives exit 1.

**`publish_new(dir, name, bytes)`** is used for new notebooks, `--new` and
scripts. It never replaces anything.

1. Write the temporary file as above.
2. Commit with `std::fs::hard_link(temp, dest)`. POSIX `link(2)` creates `dest`
   atomically and fails with `EEXIST` if anything exists at `dest`, a dangling
   symlink included. So a destination that appears after the earlier existence
   check is never replaced. This is the link-and-unlink no-replace primitive.
   Linux `renameat2(RENAME_NOREPLACE)` is not used, because std does not expose
   it and reaching it would add a direct `libc` dependency (OD-11).
3. Remove the temporary name with `remove_file(temp)`. If that fails, the
   output is already correctly published. The command still exits 0, with a
   diagnostic naming the leftover temporary file. A crash between steps 2 and 3
   leaves only a second link to the published bytes.
4. On `EEXIST`:
   - notebooks give exit 1, `destination_exists`;
   - scripts retry with the next number, as in §4.5.

   In both cases the temporary file is removed and the competitor's file is
   untouched.
5. If the filesystem refuses hard links (`EPERM`, `ENOTSUP`/`EOPNOTSUPP`, for
   example vfat, exFAT and some FUSE mounts), the command gives exit 1,
   `publish_no_atomic_noreplace`. The temporary file is removed and nothing is
   published. There is **no** fallback to a non-atomic create or rename.
6. **Platforms.** Publication is compiled for `cfg(unix)` only. On any other
   target `render` gives exit 1, `publish_unsupported_platform`, before
   writing; `--dry-run` still works. A Windows primitive is out of scope.

**`publish_append(dest, h0, id0, bytes)`** is the `--append` compare-and-swap.

1. Open the notebook with `open_regular_file`, which refuses symlinks. Take
   `id0 = (dev, ino)` from `fstat` and read the bytes `b0` (≤ 16 MiB). Compute
   `h0 = sha256_hex(b0)`.
2. Take an exclusive advisory lock on that descriptor with
   `File::try_lock()`. If it is already held, exit 1, `notebook_busy`, with the
   file unchanged. The lock serializes concurrent toolkit renders. Jupyter
   does not take it, which is why steps 4 and 6 exist.
3. Parse, append and serialize, then write the temporary sibling as above.
4. **Verify.** `lstat(dest)` must be a regular file with `(dev, ino) == id0`.
   Re-reading the held descriptor from offset 0 must hash to `h0`. Any
   difference gives exit 1, `lost_update`: the temporary file is removed and
   the destination keeps the other writer's bytes. This covers an in-place
   edit (same inode, new bytes), a rename-replace (new inode), deletion and a
   symlink swap.
5. **Commit.** `rename(temp, dest)`.
6. **Post-commit check.** Re-read the held descriptor, which still refers to
   the old inode. If its hash is no longer `h0`, an in-place writer changed it
   between steps 4 and 5. The tool then:
   - preserves those bytes with `publish_new` as
     `<name>.ipynb.conflict-NN`, with no-replace and the lowest free `NN`;
   - gives exit 1, `lost_update_after_commit`, naming that file.

   No edit is silently dropped.
7. **Residual window, stated honestly.** POSIX has no conditional rename. One
   case remains: a non-cooperating writer that *replaces* the file by rename in
   the interval between the `lstat` in step 4 and the `rename` in step 5. Both
   are back-to-back system calls with no I/O between them. That writer's file
   would be replaced, and the replacement cannot be detected afterwards.
   Linux `renameat2(RENAME_EXCHANGE)` would close the window: exchange, verify
   the swapped-out inode, and exchange back on a mismatch. It needs the `libc`
   dependency of OD-11. Cooperating toolkit renders cannot hit this case
   because of the lock in step 2.

**Test seam.** Both primitives take `&mut dyn PublishHooks`. The trait is
`pub(crate)`, and production passes `NoHooks`, so tests and production run the
same code path. Its callbacks are:

- `after_temp_written`;
- `before_commit`, which runs after verification for append;
- `after_commit_before_unlink`;
- `fault(stage) -> Option<io::Error>`, at stages `temp_create`, `write`,
  `sync`, `link`, `unlink` and `rename`.

The interleaving fixtures in R18 use these hooks to run a competing writer at
an exact point. No test depends on timing.

### 4.6 Reward-contract checker (C10, C11)

- **`reward sample --format F`** prints the documented sample input for format
  F as compact JSON. The examples are Rust constants, each with its source URL
  and section anchor in a comment.
  - The five SageMaker formats print the C10 per-format payload: a **list**
    holding one sample.
  - `nova` prints the C11 "Sample payload for the reward lambda": a single
    **object**. The page's block contains a `# Following section ...` comment
    line that JSON does not allow, and the constant omits only that line.
- **`reward check --format F --event E --response R [--batch]`** reads both
  files (bounded, symlink-refusing, strict JSON). `--format` is **required**,
  and there is no auto-detection (r1, #5127). Two of the documented shapes are
  supersets of others (see §4.6.1), so detection could not be deterministic.
  `E` is validated against format F (§4.6.1) before the response is examined.
  `R` must be one of two envelopes:
  - **Option A:** a list.
  - **Option B:** an object `{"statusCode", "body", "headers"?}`.
    `statusCode` must be the integer 200; any other value is reported as the
    documented per-sample failure, with exit 1. `body` must be a **string**
    that parses as a list. A `body` that is already a list or object is
    rejected with the C10 quotation (D-1).

  Both envelopes apply to `nova` as well. C11's contract handler returns
  `lambda_grader`'s list (Option A), and its example handler returns Option B.
  A bare single result object, not wrapped in a list, is refused for every
  format. C11's "Output format" block shows one *element* of the returned list,
  while its contract says "Returns: List of dictionaries".

  Every result needs:
  - `id`, a string. Results are matched to samples by `id`:
    - each sample id must appear in exactly one result;
    - a result id that matches no sample is an error, and so is a duplicate.

    The only exception is a **verl** sample without `id` (C10: for verl, `id`
    "appears only if set in the dataset entry"). There the result at the same
    position must still carry a string `id`, because the output field is
    required, but it is not compared. The report says `id not compared (verl
    sample without id)`. A sample of any other format without `id` is an event
    error (§4.6.1), so the exception cannot hide a malformed event.
  - `aggregate_reward_score`, a finite number;
  - optional `metrics_list` entries of `{name: string, value: finite number,
    type: "Reward" | "Metric"}`.

  The number of results must equal the number of samples. An `{"error": ...}`
  object matches neither envelope, so it is reported as unparseable (D-2).
- **Output.** Success prints
  `ok: <format>, N sample(s), N result(s), envelope A|B` and exits 0. Failure
  prints one line per finding, naming positions such as
  `event[0].messages[1].role` or `body[0].metrics_list[1].type`, never values,
  and exits 1. An envelope failure reads
  `matches neither documented envelope (C10 Option A/B)`. It never claims
  which service exception would follow (§2, C10 evidence note).

#### 4.6.1 Event formats (r1, #5127)

**Event shape.**

- For the five SageMaker formats, `E` must be a JSON **array**. C10: "passing
  a list that contains a single sample object ... currently it contains exactly
  one item per invocation".
  - Without `--batch` it must hold exactly **1** sample. That is the documented
    single-sample invocation, and any other count gives exit 1,
    `event_sample_count`.
  - `--batch` accepts 1–1,000 samples. That is a deliberate local extension
    for testing a handler's iteration, which C10 requires ("Your Lambda must
    iterate the list"). The success line then adds `(batch: local extension,
    the service sends 1)`.
- For `nova`, `E` may be a single **object** (the C11 sample payload) or an
  **array** of 1–1,000 objects (the C11 contract's `samples: list[dict]`). C11's
  own example handler accepts both. `--batch` is accepted for symmetry but
  changes nothing. An object event is treated as one sample at position 0.
- An empty array, a non-object element (`[1]`, `[[]]`) or a top-level scalar is
  refused for every format, with exit 1 `event_shape`.

**Per-sample required members.** Extra members are always allowed, because
C10 and C11 pass dataset fields through. A "message" means
`{role: string, content: string}`.

| Format | Required members (types) | Optional members checked when present | Source |
| --- | --- | --- | --- |
| `openai` | `id` (string); `messages` (non-empty array of messages, the last with role `"assistant"`); `reference_answer` (object with `text` string) | — | C10 `#…-lambda-input-openai` |
| `verl` | `data_source` (string); `prompt` (non-empty array of messages); `response` (string); `extra_info` (object) with `reference_answer` (object with `text` string) | `id` (string), `reward_model` (object) | C10 `#…-lambda-input-verl` |
| `hf-prompt-completion` | `id`; `prompt` (string); `completion` (string); `reference_answer.text` (string) | — | C10 `#…-lambda-input-hf-prompt` |
| `hf-preference` | `id`; `prompt`; `completion`; `chosen`; `rejected` (all strings); `reference_answer.text` (string) | — | C10 `#…-lambda-input-hf-preference` |
| `sagemaker-eval` | `id`; `model_response`; `query`; `response` (all strings); `reference_answer.text` (string) | `system` (string) | C10 `#…-lambda-input-sm-eval` |
| `nova` | `id` (string); `messages` (non-empty array of messages) | `reference_answer` (any JSON; C11's example is an object with `compliant` and `explanation`) | C11 `#nova-reward-interface`, `#nova-reward-input-fields` |

- `[{}]` fails every format: each format requires at least one member, and the
  check names each missing member.
- **Supersets, stated explicitly:**
  - an `hf-preference` sample also satisfies `hf-prompt-completion`;
  - an `openai` sample also satisfies `nova`.

  This is why `--format` is required, not detected. The round-trip matrix in
  AP-RW-04 asserts these two passes and every other cross-format failure.

## 5. Requirements and offline test IDs

All tests are deterministic and offline. Fixtures are files under
`crates/toolkit-cli/src/ai_payloads/tests/fixtures/` or in temporary
directories. Tests make no network call, read no credentials, run no paid
inference and take no destructive action. The real templates are read from
`skills/core-skills/aws-ai-ml/references/` in the repository, and only read.

| Req | Requirement | Test IDs |
| --- | --- | --- |
| R1 | The grammar of §4.3 is parsed exactly, including the T11:101 non-marker line, `NOTEBOOK_ONLY_SECTION`, trimming and markdown Cell 0 | AP-TPL-01 consecutive numbering; -02 a gap, duplicate or missing Cell 0 is an error; -03 `[markdown]` on a later cell is an error; -04 `# Cell 2 is ...` is text; -05 the section suffix is not part of the label; -06 an empty label is an error; -07 CRLF and a missing final newline |
| R2 | Placeholder lexing and classification | AP-SUB-01 quoted, both quote characters; -02 bare; -03 comment; -04 sentinel `in (...)`, `==`, `!=`; -05 a regex `"[^A-Za-z0-9-]"` and `[markdown]` are not placeholders; -06 inside a triple-quoted string, an f-string or a longer literal is a lint error; -07 a `#` inside a string is not a comment |
| R3 | Typed, escaped substitution | AP-SUB-10 a string with `"`, `'` or `\` round-trips in a Python literal (escaping table golden); -11 newline, ESC, U+202E and over-length are rejected with the name and no value; -12 a bool or number in a quoted slot is rejected; -13 bare slots emit the typed Python literal of their §4.4.1 row (`True`, `False`, a decimal integer, a list of escaped strings); -14 a JSON string, NaN, an object or an untyped bare-slot name (lint) is rejected; -15 an unknown value key is rejected; -16 `[PROJECT_DIR]` defaults to the canonical project directory |
| R4 | Secret slots never carry literal secrets | AP-SEC-01 a literal token for `HF_TOKEN_OR_NONE` gives exit 1, with stdout, stderr and the output file free of the token (sentinel string checked); -02 `null`, `"None"` and a Secrets Manager ARN are accepted; -03 the T17:39 sentinel is unchanged in every case; -04 `INPUT_TOKENS_MEAN` is not a secret |
| R5 | Unresolved handling fails closed | AP-SUB-20 a missing slot gives exit 1, lists names sorted, writes no file; -21 `--allow-unresolved` keeps the text; -22 comment occurrences never count as unresolved |
| R6 | Notebook output matches §4.5 and C14 | AP-NB-01 a golden notebook for a synthetic fixture, byte-exact; -02 `parse_json` round trip, `nbformat` 4/4, per-cell shape; -03 source line splitting (last line without `\n`, blank inner lines kept); -04 `NOTEBOOK_ONLY` kept verbatim in notebook mode; -05 `--append` keeps existing cells and metadata semantically identical and adds Cell 0 as the divider; -06 an existing file without `--append`/`--new` gives exit 1 and leaves the file unchanged; -07 a non-v4 or over-cap notebook is refused; -08 append to a real 4.5-shaped fixture (`nbformat_minor` 5, every cell with an id) keeps every existing id and minor 5, and each appended cell has an 8-hex id unique in the notebook (golden with `ContentIds`; identical across two runs on the same input); -09 `FixedIds` sequence `["a1", "a1", "b2"]` against an existing id `a1` assigns `b2` and then the next attempt (collision handling), and 64 forced collisions give exit 1 `cell_id_exhausted` with the file unchanged; -10 a 4.5 notebook with a missing, duplicate, empty, over-64-character or `bad id!` cell id gives exit 1 `notebook_cell_ids_invalid`, file unchanged; -11 append to 4.0, 4.3 and 4.4 fixtures adds no `id` and preserves the minor version; -12 `nbformat_minor` 6, `-1`, `"5"`, and `nbformat` 3 or 5 give exit 1 `notebook_version_unsupported` |
| R7 | Script output matches §4.5 | AP-SCR-01 golden script; -02 `NOTEBOOK_ONLY` lines and `NOTEBOOK_ONLY_SECTION` sections dropped (sections against T01-shaped fixtures); -03 numbering `01` on an empty directory, `max+1` otherwise, ignoring non-matching names and non-regular entries; -04 never overwrites: an existing `NN` file is skipped by numbering, and a file created at `NN` by the `before_commit` hook makes the render publish at `NN+1` with both files intact (AP-PUB-03) |
| R8 | Placement safety | AP-FS-01 `--name` with `/`, `..`, uppercase or empty is a usage error; -02 a symlinked `notebooks/` or `scripts/` component is refused; -03 a symlinked template, values or cell file is refused; -04 an over-cap input is refused; -05 `--dry-run` writes nothing |
| R9 | `--cell` replacement | AP-CELL-01 replaces section 2 of a synthetic fixture template (a non-module cell); -02 section 0, an unknown N, a duplicate N or a malformed `N=FILE` is an error; -03 unsafe characters in the cell file are refused with line and column and no echo; -04 an LF multi-line `def transform_dataset(df):` body with 4-space indentation, inner blank lines and a trailing comment is accepted and emitted byte-exact (notebook and script goldens); -05 the same file with CR LF endings is accepted and produces output byte-identical to -04; -06 tab indentation is accepted and kept; -07 a lone CR, NUL, ESC, DEL, NEL (U+0085), U+2028, U+202E, U+2066 or a leading BOM is refused, each as its own case; -08 a file without a final newline is accepted |
| R10 | The real templates satisfy the grammar (contract) | AP-CAT-01 `templates --check` over the repository's 23 TPL files exits 0; -02 a frozen golden JSON catalog (cells, placeholder sets, assignable targets with section, line and literal kind, and manual edit points per template) detects any template drift; -03 every TPL placeholder in a code context is a quoted or bare slot, so none is unescapable; -04 the canonical and mirror trees produce identical catalogs; -05 every TPL file satisfies the §4.4.2 source-text policy; -11 the T28 module-cell table matches the template text (the T28:16 `transform_fn.py` comment and the Cell 3 `from transform_fn import transform_dataset` line) |
| R11 | Every real template renders in both modes | AP-CAT-10 each template renders with generated typed dummy values for every slot (strings `x`, numbers 1, bools false, lists `[]`, secrets `null`, consent bools `false` and never `true`, T28 Cell 2 from a fixture module at `scripts/transform_fn.py`), in notebook and script mode, into temporary directories; output parses (notebook) or is checked for markers and dropped lines (script). Nothing is executed. |
| R12 | Reward samples equal the documented examples and round-trip | AP-RW-01 each `--format` output equals the doc example constant (golden; the five SageMaker formats print a one-element list, `nova` prints an object); -02 an unknown format, or `check` without `--format`, is a usage error; -03 round trip for **all six** formats: `sample --format F` output plus a fixture Option A response, and the same response as Option B, passes `check --format F` with exit 0; -04 cross-format matrix: each format's sample checked under each of the other five formats fails with named missing members, except the two documented supersets (`hf-preference` under `hf-prompt-completion`, `openai` under `nova`), which pass |
| R13 | Reward envelope checks (C10, C11) | AP-RW-10 Option A ok; -11 Option B with a string body ok; -12 Option B with a list body rejected (D-1 reproduction); -13 `{"error": ...}` rejected (D-2 reproduction); -14 status 400/500 reported; -15 count mismatch; -16 id mismatch, a duplicate result id, a result id matching no sample, and a result without `id`; a verl sample without `id` is accepted with `id not compared` while its result still needs a string `id`; -17 metrics type outside `Reward`/`Metric`, a non-finite value, a missing name; -18 diagnostics carry positions only (a sentinel string in a value is absent from the output), and an envelope failure reads `matches neither documented envelope (C10 Option A/B)` without naming a service exception; -19 duplicate JSON keys and a BOM are parse errors (shared parser) |
| R14 | CLI contract | AP-CLI-01 `diag_contract.rs` passes with the new routes; -02 help lists every route and `docs/migration/ai-payloads.md` mentions each (registry test `every_slice_documents_its_commands`); -03 exit codes 0/1/2 through the real binary (`tests/ai_payloads.rs`, process tests) |
| R15 | Callers repointed and mirrors in sync | AP-DOC-01 the four `code_output_guide.md` files and the training/processing `configuration.md` name `toolkit ai-payloads render`; -02 `toolkit sync plugin-skills --check` exits 0 after S4; -03 each reward-template caller page names `toolkit ai-payloads reward check` |
| R16 | Inventory dispositions | AP-INV-01 every row with `issue == 4386` carries the §3 disposition, none `converted`; -02 T08 and T09 reclassified (OD-7); -03 totals 0 removed and 56 retained |
| R17 | Per-name types (§4.4.1; #5122) | AP-TYP-01 golden: `ACCEPT_EULA` `false` emits `ACCEPT_EULA = False` in a T15-shaped fixture; `true` with `--consent ACCEPT_EULA` emits `True`; -02 `ACCEPT_EULA` `true` **without** `--consent` gives exit 1 `consent_not_given` and writes no file; -03 `ACCEPT_EULA` rejects `["false"]`, `[true]`, `[]`, `"false"`, `"true"`, `1`, `0`, `null` and `{}`, each its own case, each exit 1 naming the slot and never the value; -04 `TRUE_OR_FALSE` accepts `true` and `false` (goldens) and rejects `["false"]`, `"false"`, `1`, `null`; -05 `--consent` naming a non-consent or unused name gives exit 1 `consent_name_invalid`; -06 workload integers: `INPUT_TOKENS_STDDEV` `0` and `OUTPUT_TOKENS_STDDEV` `0` are accepted (golden); `INPUT_TOKENS_MEAN`, `OUTPUT_TOKENS_MEAN`, `CONCURRENCY` and `REQUEST_COUNT` reject `0`; every integer row rejects `-1`, `512.0`, `5.12e2`, `2147483648`, `true`, `"512"`, `null` and `[512]`; -07 `METRICS_LIST` `["Correctness", "Faithfulness"]` emits `["Correctness", "Faithfulness"]` and `[]` emits `[]` (goldens); `null`, `"Correctness"`, `[1]`, `[""]`, `["a b"]`, `["X", "X"]`, `[["X"]]` and 65 names are rejected; -08 the type table covers exactly the bare-slot names found in the 23 TPL files (AP-CAT contract, so a new bare slot without a row fails) |
| R18 | Atomic publication and append compare-and-swap (§4.5.1; #5123) | AP-PUB-01 new notebook: the `before_commit` hook creates the destination with competitor bytes, giving exit 1 `destination_exists`; the destination equals the competitor bytes and no `.toolkit-render-` file remains; -02 two renders into one fresh project, interleaved through hooks so both pass the existence check: the first commits, the second gets `destination_exists`, and the first file is byte-identical to its golden; -03 script: the hook takes `NN` and the render publishes `NN+1`; 8 consecutive takes give exit 1 `script_number_contended`, with no temp left and no partial file under any `NN`; -04 an injected fault at `temp_create`, `write` or `sync` gives exit 1; for new output the destination is absent, for append it is byte-identical; no temp file remains; -05 an injected `link` fault of `EPERM` or `EOPNOTSUPP` gives exit 1 `publish_no_atomic_noreplace`, destination absent, temp removed; -06 an injected `unlink` fault after the commit gives exit 0, the output is published, and the diagnostic names the leftover temp file; -07 the destination is a dangling symlink created by the hook: the link fails and nothing is written through it; -10 append: the `after_temp_written` hook rewrites the notebook in place (same inode, new bytes), giving exit 1 `lost_update`; the notebook holds the hook's bytes and the temp is removed; -11 append: the hook replaces the notebook by rename (new inode, even identical bytes), giving exit 1 `lost_update`; -12 append: the hook deletes the notebook or swaps in a symlink, giving exit 1 `lost_update`; -13 append: the `before_commit` hook (after verification) rewrites the old inode in place; the commit happens, the hook's bytes are preserved at `<name>.ipynb.conflict-01`, and the result is exit 1 `lost_update_after_commit`; -14 append while the test holds `try_lock` on the notebook gives exit 1 `notebook_busy`, file unchanged; -15 the platform guard is a unit test of the `cfg(not(unix))` refusal function, compiled on every target, and `--dry-run` works without publication |
| R19 | T28 module cell (§4.4.4; #5124) | AP-T28-01 notebook mode with `--cell 2=<project>/scripts/transform_fn.py`: Cell 2's first source line is `%%writefile <canonical project>/scripts/transform_fn.py` and the remaining lines equal the module bytes (LF-normalized); `render` writes only the notebook, and `transform_fn.py` is byte-unchanged with the same mtime and inode; -02 script mode: the script contains Cell 2's marker and the generated comment, no `%%writefile`, and no copy of the module, and sits in the same directory as `transform_fn.py`; -03 refusals: no `--cell 2` gives `module_cell_required`, another path gives `module_path_mismatch`, a symlinked or missing module gives `module_missing`, and a module without `def transform_dataset(` gives `module_definition_missing`, each exit 1 with no file written; -04 the notebook-mode Cell 3 import is frozen verbatim (records D-5, so a P-8 fix is a visible golden change); -05 a project path containing a space, `$`, `{` or `'` gives exit 1 `writefile_path_unsafe` with nothing written |
| R20 | `assign` (§4.4.3; #5128) | AP-ASN-01 single-line targets: `BASE_MODEL` (string), `WAIT_FOR_COMPLETION` (bool), `INSTANCE_COUNT` (integer) and a float target are rewritten in place, keeping the trailing comment and quote style (goldens); -02 the parenthesized form: `ACCEPT_EULA` in a T04/T05-shaped fixture and `reward_function_path` in a T03-shaped fixture rewrite only the inner literal line; -03 a section-3 target (`CUSTOM_PROMPT_PATH`, T05-shaped) is assignable; -04 unknown target, markdown-section target, a target assigned twice (`_hf_token`) or tuple-assigned, an f-string literal, a `None` literal without a type row, and an expression right-hand side (`S3_OUTPUT_PATH = f"..."`) each give exit 1 `assign_target_invalid`; -05 types: a string target rejects `1`, `true` and a newline; a bool target rejects `"False"`, `0` and `["false"]`; an integer target rejects `-1` and `1.5`; `ACCEPT_EULA` via `assign` follows AP-TYP-02/03 (consent flag required for `true`); -06 secrets: `assign.HF_HUB_TOKEN` with a literal token, with `null` or with an ARN is refused (the literal holds a slot), and the token sentinel is absent from stdout, stderr and the filesystem; a fixture target `API_TOKEN = ""` accepts only `null`, `"None"` or an ARN; -07 the T17:39 sentinel line is byte-identical after any combination of `values` and `assign`; -08 a target in a section replaced by `--cell` gives `assign_target_replaced`; -09 the target `INPUT_LOCATION`, whose literal holds a slot, is refused with a pointer to `values`; -10 duplicate keys inside `assign` are a parse error; -11 after success, stdout lists the catalogued manual edit points (cell, line, comment prefix) for a T15-shaped fixture, and an `--append` run never alters existing cells (manual edits kept, AP-NB-05) |
| R21 | Notebook versions (§4.5; #5126) | AP-NB-08 to AP-NB-12 (listed in R6) |
| R22 | Event formats (§4.6.1; #5127) | AP-RW-20 `[{}]` fails every format, naming the missing members; -21 a non-verl sample (openai, both hf formats, sagemaker-eval, nova) without `id` fails as an event error, even when the response carries any `id`; -22 a SageMaker-format event with 2 samples fails `event_sample_count` without `--batch` and passes with it, and the success line names the batch extension; -23 `[]`, `{}` (non-nova), `1`, `[1]`, `[[]]` and `"x"` give `event_shape`; -24 nova: the object event (the C11 sample) and a one-element array of it both pass with the same response; an array of 1,001 objects is refused; -25 per-format type errors: openai `messages` empty or last role `user`; verl `prompt` a string; hf `chosen` a number; sagemaker-eval `model_response` missing; each names the position; -26 D-1 and D-2 reproductions under their real formats: T09's envelope shape (Option B with a list `body`) under `nova`, and T06's `{"error": ...}` under `verl` and `openai`, are both flagged (#5120); -27 a bare single result object (no list) is refused for `nova` and for every SageMaker format |

## 6. Intentional behaviour changes

Before this slice, the customer's agent did every one of these steps by hand,
following prose rules. Each change is recorded in the slice's parity document
(`docs/migration/ai-payloads.md`).

- **B-1.** Notebook JSON and script files are written by `toolkit ai-payloads
  render`, not composed by the agent. The structure is the one the guide
  specifies; `metadata` is now fixed (OD-5).
- **B-2.** Unresolved code placeholders fail closed (exit 1, no file). Before,
  this was advisory ("Fill every bracketed placeholder before running"); now it
  is opt-out through `--allow-unresolved`.
- **B-3.** Placeholders in comments are never substituted. Before, the result
  depended on the agent.
- **B-4.** Sentinel comparisons (T17:39) are never substituted. A replace-all
  would have broken gated-model downloads.
- **B-5.** A literal secret for a secret slot is refused. This follows the
  skill's "never echo it back" and the template's preference for a Secrets
  Manager ARN, so the secret never lands in a notebook file.
- **B-6.** Typed slots: quoted slots take only strings, and each bare slot takes
  only the JSON type and range of its own §4.4.1 row. A value can no longer
  inject code. A truthy non-boolean, whether `"false"`, `1` or `["false"]`,
  can no longer reach a consent or flag parameter. (r1: the r0 text claimed
  this without a per-name schema; #5122.)
- **B-7.** Script numbering and name validation are deterministic, and the tool
  refuses to overwrite.
- **B-8.** In script mode each section starts with `# %% Cell N: Label`, and
  Cell 0 becomes the module docstring. The guide allowed any IDE-compatible
  form.
- **B-9.** `--append` rewrites the existing notebook with 2-space indentation.
  Its content is unchanged, but the whitespace may differ from what Jupyter
  wrote (Jupyter uses a 1-space indent).
- **B-10.** The new reward checker reports D-1 and D-2 when given those
  templates' responses. The templates themselves are unchanged (P-4, #5120).
- **B-11 (r1).** Setting a consent-typed name (`ACCEPT_EULA`) to `true` needs a
  second, explicit `--consent ACCEPT_EULA` flag. Before, the agent edited the
  literal by hand, and nothing distinguished acceptance from a typo.
- **B-12 (r1).** Every output is published atomically with no-replace. A
  notebook or script that appears during a render is never replaced. An
  `--append` that finds the notebook changed since it was read refuses
  (`lost_update`), or preserves the concurrent bytes as a `.conflict-NN` file.
  Before, the agent's own file write could clobber either.
- **B-13 (r1).** T28 needs its module on disk at `scripts/transform_fn.py`
  before rendering, and `render` never writes it. In notebook mode Cell 2 is a
  `%%writefile` cell. In script mode Cell 2 is a pointer comment. This matches
  the guide's mode split, made mechanical. D-5, the notebook-mode import, is
  recorded and not fixed (P-8).
- **B-14 (r1).** `--append` supports nbformat 4.0–4.5. On 4.5 it adds unique
  cell ids and refuses a notebook whose existing ids are invalid. The guide
  only described writing 4.4.
- **B-15 (r1).** `reward check` needs `--format` and validates the event's
  per-format shape. SageMaker formats take exactly one sample unless
  `--batch` is given.
- **B-16 (r1).** `assign` sets the catalogued literal fields that it can type,
  and `render` prints the edit points that remain manual. Before, the agent
  hand-edited every field.
- **Unchanged.** The template text, the `python3 <script>.py` execution step,
  mode selection, the append question and every SDK call.

## 7. Implementation slices (ordered; tests first in each)

Each slice commits its failing tests first, then the implementation, then
docs, on one RSI branch. Verification is daemon recipe jobs plus a
`scoped_test`. Review is by the other model family against the exact SHA.

| Slice | Content | Tests written first | Depends on |
| --- | --- | --- | --- |
| S1 `ai-payloads-model` | `template.rs` (grammar, lexer, classification), `templates` command with `--json` and `--check`, registry line, `docs/migration/ai-payloads.md` skeleton | AP-TPL-*, AP-SUB-01..07, AP-CAT-01..04, AP-CLI-01/02 for `templates` | #4386 design accepted (this document) |
| S2a `ai-payloads-publish` (r1) | `place.rs`: `publish_new`, `publish_append`, `PublishHooks` and the fault seam (§4.5.1). No command yet. | AP-PUB-* | S1 (module only) |
| S2b `ai-payloads-render` | `types.rs`, `values.rs`, `assign.rs`, `notebook.rs` (including nbformat 4.0–4.5 and `IdSource`), `script.rs`, the T28 module-cell table, the `render` command | AP-SUB-10..22, AP-SEC-*, AP-TYP-*, AP-ASN-*, AP-T28-*, AP-NB-*, AP-SCR-*, AP-FS-*, AP-CELL-*, AP-CAT-10/11, AP-CLI-03 | S1, S2a |
| S3 `ai-payloads-reward` | `reward.rs`, `reward sample` and `reward check` with `--format`/`--batch` (§4.6.1) | AP-RW-* | S1 (registry and module only); independent of S2a/S2b |
| S4 `ai-payloads-callers` | Edit the canonical skill prose: the four `code_output_guide.md` files, training/processing `configuration.md`, the `overview.md` and reference pages in §1.2 that describe hand rendering, `create-reward-function.md` and `rlvr_reward_function.md` (testing step). Also: the EULA consent step now names `--consent ACCEPT_EULA`; dataset-transformation Step 9 passes `--cell 2=<project-dir>/scripts/transform_fn.py`; the manual edit points stay as prose (§4.4.3). Mirrors are regenerated with `toolkit sync plugin-skills`. Templates are not edited. | AP-DOC-* | S2b, S3 |
| S5 `ai-payloads-inventory` | `python-inventory.json`: dispositions, OD-7 reclassification, `status` values; this document's status line; `docs/migration/README.md` row note | AP-INV-* | S4 |

S1–S3 are small enough for one implementer each. r1 splits S2 so that
publication safety (S2a, about 500 lines including the interleaving tests) is
reviewed alone, before any command uses it. S2b is the largest, at about 1,500
lines of Rust including tests. S4 and S5 are prose and JSON edits with
contract tests.

## 8. Unresolved items: proposed Issues (AC4)

None was created by this design, as the brief requires. The manager files the
Issues it accepts. r1 status from #5119's PM rulings:

- P-1 is declined for #4386 (OD-1);
- P-2 is follow-up #5121;
- P-4 is bug #5120;
- P-8 is new in r1.

- **P-1. Product choice: a Python-free controller mode for SageMaker serverless
  customization, training, processing and deployment.** C2 shows that
  `CreateTrainingJob` with `ServerlessJobConfig`, and the Bedrock and SageMaker
  hosting APIs, are callable from Rust. A
  `toolkit ai-payloads train|deploy submit` built on a live `toolkit-aws`
  binding would remove the client Python. But it replaces the product's
  notebook UX and SDK conveniences (recommended hyperparameters, AI registry
  `DataSet`), and it needs primary-doc verification of every API involved,
  including how the AI registry dataset and evaluator are created without the
  SDK. Needs an operator or product decision before any engineering.
- **P-2. Product choice: ship a Rust reward-Lambda template** (cargo-lambda,
  `provided.al2023`, C9) alongside the Python ones, bound by ARN (C10, C11).
  This adds customer content; it does not migrate anything. **PM ruling: a
  separate follow-up, #5121, not part of #4386.**
- **P-3. Documented non-Python access to SageMaker managed MLflow.** Find, in
  primary AWS documentation, the SigV4 signing scope and REST endpoint (C13),
  then decide on `toolkit ai-payloads mlflow metrics --training-job NAME`
  (`DescribeTrainingJob` for `MlflowConfig`/run id, then MLflow
  `metrics/get-history`). Until then T10 stays K-PY. Blocked on documentation,
  not on engineering.
- **P-4. Reward-template envelope defects D-1 (T09) and D-2 (T06).** These are
  product-content fixes to customer templates. They need an explicit decision,
  because the existing Python is migration evidence. Evidence: C10 Option B
  text, and the AP-RW-12/13 and AP-RW-26 reproductions from S3. **PM ruling:
  the templates stay unchanged in #4386, the operator decides through #5120,
  and `reward check` must flag both.**
- **P-5. Inconsistent `NOTEBOOK_ONLY` install lines (D-3).** Make the install
  lines uniform: commented or bare `%pip`. Today a commented install never
  runs in notebook mode.
- **P-6. Inventory classification (D-4).** T08 and T09 are Lambda payloads
  marked `toolkit-helper`. The 23 TPL rows are client-side templates, not
  service payloads. Done in S5 if OD-7 is accepted; this Issue only records
  the audit correction (inventory-audit U-1, U-2).
- **P-7. Python fenced blocks in aws-ai-ml markdown (§1.5).** 24 blocks in 12
  pages. Record their customer-language disposition in the inventory audit, so
  the cutover (#4414) and retire (#4415) slices do not treat them as unowned
  Python.
- **P-8 (r1). T28 notebook-mode import resolution (D-5).** Change the
  template's Cell 3, or the guide, so the notebook resolves
  `from transform_fn import transform_dataset`, for example with
  `sys.path.insert(0, str(Path(PROJECT_DIR) / "scripts"))`. That mirrors the
  guide's own test command at overview.md:137. It is a product-content change
  to read-only Python, so it falls under the same operator rule as P-4. Until
  then AP-T28-04 freezes the current text.

## 9. Open decisions (each with a recommendation)

| ID | Decision | Recommendation |
| --- | --- | --- |
| OD-1 | Replace the notebook/script product flow with Rust controllers that call AWS directly? | **No, not in #4386.** Keep the customer-language templates (K-PY) and file P-1 as a product choice. C1 and C2 show the services allow it, but the customer UX is Python notebooks. |
| OD-2 | In notebook mode, uncomment `# %pip ... # NOTEBOOK_ONLY` lines? | **No.** Keep lines verbatim, so the output equals the template, and fix the templates through P-5. |
| OD-3 | Support `assign` for empty configuration fields (`BASE_MODEL = ""`)? | **Yes, in S2b, under §4.4.3 (r1, #5128).** Any non-markdown section; single-line or parenthesized single-literal form; exactly one assignment to the name in the whole template; literal free of placeholders. Types come from §4.4.1 (consent) or from the literal's kind; secret names take the secret rule. The merge order is fixed, and the edit points that remain manual are printed. r0 limited this to single-line literals in sections 1–2, which missed `CUSTOM_PROMPT_PATH` (T05:50, Cell 3) and the parenthesized `ACCEPT_EULA` (T04:36–38, T05:38–40). |
| OD-4 | Rename `.py` templates to a template extension so the Python count drops? | **No.** That hides Python without removing it (AC3, AGENTS.md), loses editor support for customers, and breaks every caller path. |
| OD-5 | Notebook `nbformat_minor` and `metadata` | **4.4** (the guide's value; no cell ids required). `metadata` = `{"kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"}, "language_info": {"name": "python"}}`. This applies to **new** notebooks. r1 (#5126): `--append` accepts 4.0–4.5, preserves the minor version, and generates unique ids only for 4.5 (§4.5). Writing *new* notebooks as 4.5 stays a later, separate change. |
| OD-6 | Size cap for `--append` notebooks | **16 MiB.** Notebooks with outputs exceed the 1 MiB text cap; refuse above 16 MiB with a clear message. |
| OD-7 | Inventory representation of K-PY rows | **Keep the `classification` vocabulary.** Correct T08 and T09 to `external-service-payload`. Add `disposition: "retained-customer-language"` and `language_contract: "C1"`/`"C9"`/`"C13"` (anchors to §2), and set `status` to `retained`. The 23 TPL rows keep `external-service-payload` with the TPL disposition note, rather than adding a new class. |
| OD-8 | Where the toolkit finds templates | **Explicit path.** `render TEMPLATE` takes the file path the agent already read. `templates --root` defaults to the installed skill's `references/`. No copy is embedded in the binary, so the skill tree stays the single source (AGENTS.md: avoid reimplementing canonical skills). |
| OD-9 | Reward `check` for non-200 Option B | **Report as failure (exit 1) and name the documented effect.** The sample is counted in `byoc_failure_count` and its metrics are dropped (C10). Do not treat it as valid. |
| OD-10 | Owner model | **Keep the Issue's owner** (DeepSeek primary, GLM review) for S1, S2a, S2b and S3. S4 and S5 are prose and JSON and can go to either family. |
| OD-11 (r1) | Primitive for no-replace publication | **Use `link(2)` and then `unlink` through `std::fs::hard_link`, on every Unix target.** Refuse filesystems without hard links, and refuse non-Unix platforms. No new dependency. Linux `renameat2(RENAME_NOREPLACE \| RENAME_EXCHANGE)` would also close the append residual window (§4.5.1 step 7), but std does not expose it, and it needs a direct `libc` dependency (only transitive today). Revisit only if a reviewer judges that residual window, against non-cooperating rename-replacing writers, material. |
| OD-12 (r1) | How explicit EULA consent reaches `render` | **A JSON `true` plus a separate `--consent ACCEPT_EULA` flag** (B-11). The alternatives were rejected: `true` alone would let a generated values file carry consent invisibly, and an interactive prompt breaks non-interactive agents. The caller's step of asking the user stays in the skill prose (S4). |

## 10. Sources

Every source is listed in the §2 table with its section. The SageMaker Python
SDK pages (C1, C12) are vendor documentation for the AWS-owned SDK. All other
sources are AWS service documentation, except C13's MLflow REST reference
(mlflow.org) and C14 (nbformat, Jupyter project).

r1 adds these primary sources:

- the custom-scorer page's output Note (C10 evidence note, §2);
- the per-format input subsections of the same page (§4.6.1);
- the Nova input-fields and LLM-judge implementation sections (C11);
- the nbformat "Cell ids" section (C14, already cited);
- the AIPerf load-generator options reference, for the integer types of
  `--concurrency` and `--request-count` (§4.4.1). AIPerf is NVIDIA vendor
  documentation for the benchmark engine behind `Workload.synthetic`.

Facts this repair labels **inferred**:

- the workload integer minima (§4.4.1);
- IPython magic-argument expansion (§4.4.4);
- the Jupyter kernel working directory (D-5);
- the reason pre-review #5119 missed the C10 Note (§2).

## 11. r1 resolution of pre-review #5119

| Finding | Severity | Resolution | Where | Test IDs |
| --- | --- | --- | --- | --- |
| #5122 bare-slot union lets a truthy non-boolean accept the EULA | P1 | The union is removed. A closed per-name type table: consent and flag names take only JSON `true`/`false`; `true` for consent also needs `--consent`; workload integers follow their domains, with zero allowed for the stddevs; `METRICS_LIST` is a list of names. B-6 is corrected and B-11 added. | §1.4, §4.4, §4.4.1, §6 B-6/B-11, OD-12 | AP-TYP-01..08 (negatives include `["false"]`, `[true]`, `1`, `null`, `"false"`), AP-SUB-13/14, AP-ASN-05 |
| #5123 a notebook rename can overwrite a competing new file | P1 | All publication goes through `place.rs`: the temporary file is created with `create_new` and synced, then committed with `link(2)` no-replace and unlinked. There is no non-atomic fallback; filesystems without hard links and non-Unix platforms are refused. Scripts use the same primitive, retrying the next `NN`. `--append` is a compare-and-swap on `(dev, ino)` plus SHA-256, under an advisory lock, with a post-commit check that preserves concurrent bytes. The temporary file is cleaned up on every failure. The residual window is stated. A hook and fault seam give deterministic interleavings. Publication is its own slice, S2a. | §4.5, §4.5.1, §6 B-12, §7 S2a, OD-11 | AP-PUB-01..07, AP-PUB-10..15, AP-SCR-04, AP-NB-06 |
| #5124 the T28 `transform_fn.py` artifact contract | P2 | The module cell is declared per template. The caller writes the module first, as in the guide's Step 7, and `render` reads it from its canonical path and never writes it. Notebook mode: `%%writefile` with a path-safety rule. Script mode: a pointer comment. Import resolution is specified per mode; the notebook gap is recorded as D-5 and P-8, and Python stays read-only. | §1.4, §2.2 D-5, §4.4.4, §6 B-13, §8 P-8 | AP-T28-01..05, AP-CAT-10/11 |
| #5125 the `--cell` rule rejects multi-line source | P2 | A separate `source_text_ok` policy: LF and CR LF (normalized to LF) and tab are accepted; a lone CR and every other control, bidi or format character, BOM included, are refused with line and column. It also applies to template scanning. | §4.3 rule 7, §4.4.2 | AP-CELL-03..08, AP-CAT-05 |
| #5126 append to 4.5 emits cells without ids | P2 | `--append` supports nbformat 4.0–4.5 and preserves the minor version. On 4.5, existing ids are validated (never repaired) and new ids are unique 8-hex ids from `ContentIds`, with collision retry; tests inject `FixedIds`. 4.0–4.4 get no ids. New notebooks stay 4.4. | §4.5, OD-5, §6 B-14 | AP-NB-08..12 |
| #5127 the reward checker does not validate event formats | P2 | `--format` is required, with no detection (two documented shapes are supersets). Per-format required members come from C10 and C11. SageMaker formats take a single sample, with `--batch` as an explicit local extension. Nova accepts an object or a list. The verl id exception applies to verl only. A bare result object is refused. | §2 C10/C11, §4.6, §4.6.1, §6 B-15 | AP-RW-01..04 (six-format round trips and cross-format matrix), AP-RW-16, AP-RW-20..27 |
| #5128 `assign` has no tests and misses live fields | P2 | Defined in full: targets (any non-markdown section, parenthesized form, exactly one assignment, no placeholder), types, secret enforcement identical to `render` (the `HF_HUB_TOKEN` and `_hf_token` cases, with the sentinel unreachable), consent enforcement identical to slots, a fixed merge order with `--cell` conflicts, and manual edit points listed. | §1.4, §4.4.3, §6 B-16, OD-3 | AP-ASN-01..11 |
| #5129 RLVR `InvalidLambdaResponseException` lacks a source | P3 | **Sourced.** The claim is the Note in `#model-customize-evaluation-custom-scorers-lambda-output` of the C10 custom-scorer page, quoted verbatim, with the raw-HTML hash and method recorded. It is qualified as "commonly surfaces", and diagnostics never promise the exception. | §2 C10 and its evidence note, §2.2 D-2, §4.6 | AP-RW-18 (diagnostic wording), AP-RW-13 |

Unchanged by r1:

- inventory and AC3 accounting: 0 of 56 removed;
- dispositions;
- AC1 rows C1–C9 and C12–C14;
- OD-1, OD-2, OD-4, OD-6, OD-7, OD-8 and OD-9;
- slices S1, S3, S4 and S5, apart from the noted test additions.
