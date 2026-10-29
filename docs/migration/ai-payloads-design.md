# Slice `ai-payloads` (Issue #4386): design

Status: design only. No implementation code. Python in scope was read as
migration evidence and was not run.

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
  - *bare slots*, which are typed expressions: `[INPUT_TOKENS_MEAN]`,
    `[CONCURRENCY]` and `[REQUEST_COUNT]` in T16 and T18, `[METRICS_LIST]` and
    `[TRUE_OR_FALSE]` in T20, `[ACCEPT_EULA]` in T15;
  - *comment occurrences*, such as T16:51 and T18:117.
  `"[^A-Za-z0-9-]"` (T25–T27) is a regex, not a placeholder.
- **Sentinel occurrences.** T17:39 is
  `if _hf_token in ("", "None", "[HF_TOKEN_OR_NONE]"):`. A blind replace-all
  would turn a real token into the sentinel that disables it. T11:50 rejects
  any value starting with `[` as an unsubstituted placeholder.
- **Secrets.** `[HF_TOKEN_OR_NONE]` (T17:23) holds a secret.
  `huggingface-to-s3.md`:11 says "store it securely; never echo it back", and
  the template prefers a Secrets Manager ARN.
- **Empty fields.** Some configuration fields are empty strings for the user to
  fill, not placeholders, for example `BASE_MODEL = ""` in T01–T05 and
  `REGION = ""` in T25–T27.
- **Generated cell.** T28 Cell 2 is generated per dataset. In notebook mode it
  is a `%%writefile <project-dir>/scripts/transform_fn.py` cell.

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
| C10 | SageMaker custom scorers and RLVR reward Lambdas (T06, T08) | Lambda, invoked by the eval or RFT container | "You must implement your custom scorer as an AWS Lambda function". Input: a list holding **one** sample per invocation, shaped like the dataset format (OpenAI, verl, HF prompt-completion, HF preference, SageMaker Eval). Output: Option A, a raw list of `{id, aggregate_reward_score, metrics_list?}`; or Option B, `{"statusCode": 200, "body": "<JSON string of that list>"}`, where `body` "must be a JSON string (not a nested JSON object)". Each `id` must match its input, and `metrics_list[].type` is `"Reward"` or `"Metric"`. A malformed response fails RLVR with `InvalidLambdaResponseException`. The execution role needs `lambda:InvokeFunction`. No runtime is mandated. | <https://docs.aws.amazon.com/sagemaker/latest/dg/model-customize-evaluation-preset-custom-scorers.html> (`#model-customize-evaluation-custom-scorers-lambda-input`, `#model-customize-evaluation-custom-scorers-lambda-output`, `#model-customize-evaluation-custom-scorers-permissions`); <https://docs.aws.amazon.com/sagemaker/latest/dg/customizing-models-rft.html> (`#technique-rlvr`) |
| C11 | Nova reward Lambdas (T07, T09) | Lambda, invoked by Nova RFT and evaluation | "It must be implemented as an Lambda function". Input: OpenAI-format samples with an `id`. The grader returns a list of `{id, aggregate_reward_score, metrics_list?}`; the documented example handler returns `{"statusCode": 200, "body": json.dumps(results)}`. Bound by Lambda ARN in the recipe field `reward_lambda_arn`. 15 min timeout; must handle `rollout_worker_replicas * 64` concurrent calls. No runtime is mandated. | <https://docs.aws.amazon.com/nova/latest/nova2-userguide/nova-implementing-reward-functions.html> (`#nova-reward-interface`, `#nova-reward-constraints`, `#nova-reward-using-custom`) |
| C12 | AI registry `Evaluator.create(type=REWARD_FUNCTION, source=...)` | client SDK call | `source` is a Lambda ARN ("Method: Lambda") or a local `.py` path ("Method: BYOC"). The docs do not say what happens to a local file, or which runtime it gets. | <https://sagemaker.readthedocs.io/en/stable/v3-examples/model-customization-examples/serverless/ai_registry_example.html> |
| C13 | SageMaker managed MLflow (T10; T01–T05 Cell 7/8) | a tracking server or MLflow App | The documented client is the MLflow **Python** client plus the `sagemaker-mlflow` plugin, which "authenticates API calls made with MLflow using AWS Signature Version 4" and accepts the server ARN as the tracking URI. MLflow's own REST API is language-neutral: `GET /api/2.0/mlflow/runs/get`; `GET /api/2.0/mlflow/metrics/get-history` with `run_id`, `metric_key`, `max_results`, `page_token`. `DescribeMlflowTrackingServer.TrackingServerUrl` is documented only as "the URL to connect to the MLflow user interface". No AWS page found names the SigV4 signing scope or the REST endpoint for non-Python clients. | <https://docs.aws.amazon.com/sagemaker/latest/dg/mlflow-track-experiments.html> (intro, `#mlflow-track-experiments-install-plugin`); <https://mlflow.org/docs/latest/api_reference/rest-api.html> ("Get Run", "Get Metric History"); <https://docs.aws.amazon.com/sagemaker/latest/APIReference/API_DescribeMlflowTrackingServer.html> (`TrackingServerUrl`) |
| C14 | Jupyter notebook format (render output) | the customer's Jupyter | nbformat 4: top-level `metadata`, `nbformat`, `nbformat_minor`, `cells`. Cells have `cell_type`, `metadata`, `source`; code cells also `execution_count` (integer or null) and `outputs`. Multi-line strings "may be written as a list of strings". Cell `id` (1–64 chars, unique) is required from nbformat 4.5. | <https://nbformat.readthedocs.io/en/latest/format_description.html> ("Top-level structure", "Cell Types", "Multi-line strings", "Cell ids") |

**Not verified.** The Bedrock RFT reward page
(<https://docs.aws.amazon.com/bedrock/latest/userguide/nova-rft.html>) returned
only its title when fetched. No file in scope uses Bedrock RFT jobs:
T12 and T14 deploy and import, and do not customize. Nothing depends on that
page. `CreateCustomModelDeployment` (used by T12 through the SDK) was not
fetched separately either.

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
  `json.dumps`. T08, its open-weights sibling, does use a string. (P-4)
- **D-2.** T06's `lambda_handler` returns `{"error": ...}` with no `statusCode`
  and no results, both for an empty batch and for the first failing sample. C10
  says such a payload cannot be parsed (`InvalidLambdaResponseException`), and
  the result count must equal the input count. T07 instead returns score 0.0
  for each failed sample. (P-4)
- **D-3.** The install lines are inconsistent, commented in some templates and
  bare in others (§1.4). A commented `# %pip` line never runs in notebook mode.
  (P-5)
- **D-4.** Inventory classes are wrong for T08 and T09, and inaccurate for the
  TPL group (§1.2). (P-6)

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
`template.rs`, `values.rs`, `notebook.rs`, `script.rs`, `place.rs`,
`reward.rs` and `tests/`. It registers one `crate::registry::Slice` named
`ai-payloads`, with `synopsis_rank`/`description_rank` set to `NEW_RANK`, `doc`
set to `docs/migration/ai-payloads.md` (the slice's parity document, written in
S2), and one inserted line in `SLICES`. No new crate.

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
        [--allow-unresolved] [--dry-run]
toolkit ai-payloads reward sample --format openai|verl|hf-prompt-completion|hf-preference|sagemaker-eval|nova
toolkit ai-payloads reward check --event FILE --response FILE
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

  `assign` is subject to OD-3.
- `[PROJECT_DIR]` defaults to the absolute `--project-dir`.
- Writes exactly one output: the notebook, or the next numbered script.
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

### 4.4 Substitution rules

- **Quoted slot.** The value must be a JSON string. Rejected: a newline, any
  `is_unsafe_char` character (C0/C1 controls, bidi and format characters), and
  more than 4,096 characters. The value is written as the body of a Python
  string literal of the slot's quote character, with `\` and that quote
  escaped. A JSON bool or number is rejected, so a string `"false"` cannot
  pretend to be a bool (compare T11:77–84).
- **Bare slot.** The value must be a JSON number, bool, `null`, or an array of
  strings and numbers. It is written as a Python literal: `True`, `False`,
  `None`, a number in shortest form, or a list of escaped string literals. The
  values `NaN` and `Infinity` are rejected. A JSON string is rejected; no raw
  code is ever spliced in.
- **Secret slot.**
  - It accepts only `null`, the string `"None"`, or a Secrets Manager ARN
    (`arn:aws[a-z-]*:secretsmanager:<region>:<12 digits>:secret:...`).
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
  It is meant for T28 Cell 2. The file is bounded and symlink-refusing, and
  must be UTF-8 without unsafe characters. Section 0 and unknown numbers are
  refused.

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
- An existing notebook is never overwritten silently:
  - `--append` parses it (cap 16 MiB, see OD-6) and requires `nbformat` 4.
    It appends the new cells, Cell 0 included as the section divider, and
    rewrites the file. Member order and values are preserved; the whitespace
    may change (B-9).
  - `--new` refuses when the file exists.
  - With neither flag and an existing notebook: exit 1, with a message asking
    the user to choose. This mirrors the guide's question.

**Script:**

- Path: `<project-dir>/scripts/NN_<name>.py`, where `NN` is one more than the
  largest two-digit prefix among regular files in `scripts/` (`01` when there
  are none). Created with `create_new`, so it never overwrites.
- Content, in order:
  - a module docstring built from Cell 0's label and text;
  - for each kept section, `# %% Cell N: Label` and its body;
  - `NOTEBOOK_ONLY` lines and `NOTEBOOK_ONLY_SECTION` sections are omitted.

**Safety:**

- Every path is resolved inside `--project-dir` with `resolve_within`, after
  the project directory itself is canonicalized.
- Parent directories are created with `ensure_dir_chain`, which refuses symlink
  components.
- Notebook writes go to a temporary sibling, which is then renamed into place.
- File mode is the process default; nothing in the output is secret, because
  of the secret-slot rule.

### 4.6 Reward-contract checker (C10, C11)

- **`reward sample --format F`** prints the documented sample input for format
  F as compact JSON: the C10 example payloads for OpenAI, verl, HF
  prompt-completion, HF preference and SageMaker Eval, and the C11 Nova
  payload. The examples are Rust constants, each with its source URL in a
  comment.
- **`reward check --event E --response R`** reads both files (bounded,
  symlink-refusing, strict JSON). `E` must be a non-empty list of sample
  objects. `R` must be one of two envelopes:
  - **Option A:** a list.
  - **Option B:** an object `{"statusCode", "body", "headers"?}`.
    `statusCode` must be the integer 200; any other value is reported as the
    documented per-sample failure, with exit 1. `body` must be a **string**
    that parses as a list. A `body` that is already a list or object is
    rejected with the C10 quotation (D-1).

  Every result needs:
  - `id`, a string equal to the matching input's `id` when the input has one
    (verl samples may lack it, per C10);
  - `aggregate_reward_score`, a finite number;
  - optional `metrics_list` entries of `{name: string, value: finite number,
    type: "Reward" | "Metric"}`.

  The number of results must equal the number of samples. An `{"error": ...}`
  object is reported as unparseable (D-2).
- **Output.** Success prints `ok: N result(s), envelope A|B` and exits 0.
  Failure prints one line per finding, naming positions such as
  `body[0].metrics_list[1].type`, never values, and exits 1.

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
| R3 | Typed, escaped substitution | AP-SUB-10 a string with `"`, `'` or `\` round-trips in a Python literal (escaping table golden); -11 newline, ESC, U+202E and over-length are rejected with the name and no value; -12 a bool or number in a quoted slot is rejected; -13 bare slot literals `True`, `False`, `None`, int, float and list; -14 a string, NaN or object in a bare slot is rejected; -15 an unknown value key is rejected; -16 `[PROJECT_DIR]` defaults to the canonical project directory |
| R4 | Secret slots never carry literal secrets | AP-SEC-01 a literal token for `HF_TOKEN_OR_NONE` gives exit 1, with stdout, stderr and the output file free of the token (sentinel string checked); -02 `null`, `"None"` and a Secrets Manager ARN are accepted; -03 the T17:39 sentinel is unchanged in every case; -04 `INPUT_TOKENS_MEAN` is not a secret |
| R5 | Unresolved handling fails closed | AP-SUB-20 a missing slot gives exit 1, lists names sorted, writes no file; -21 `--allow-unresolved` keeps the text; -22 comment occurrences never count as unresolved |
| R6 | Notebook output matches §4.5 and C14 | AP-NB-01 a golden notebook for a synthetic fixture, byte-exact; -02 `parse_json` round trip, `nbformat` 4/4, per-cell shape; -03 source line splitting (last line without `\n`, blank inner lines kept); -04 `NOTEBOOK_ONLY` kept verbatim in notebook mode; -05 `--append` keeps existing cells and metadata semantically identical and adds Cell 0 as the divider; -06 an existing file without `--append`/`--new` gives exit 1 and leaves the file unchanged; -07 a non-v4 or over-cap notebook is refused |
| R7 | Script output matches §4.5 | AP-SCR-01 golden script; -02 `NOTEBOOK_ONLY` lines and `NOTEBOOK_ONLY_SECTION` sections dropped (sections against T01-shaped fixtures); -03 numbering `01` on an empty directory, `max+1` otherwise, ignoring non-matching names and non-regular entries; -04 never overwrites (the `create_new` race is simulated by pre-creating the file) |
| R8 | Placement safety | AP-FS-01 `--name` with `/`, `..`, uppercase or empty is a usage error; -02 a symlinked `notebooks/` or `scripts/` component is refused; -03 a symlinked template, values or cell file is refused; -04 an over-cap input is refused; -05 `--dry-run` writes nothing |
| R9 | `--cell` replacement | AP-CELL-01 replaces section 2 of a T28-shaped fixture; -02 section 0, an unknown N, a duplicate N or a malformed `N=FILE` is an error; -03 unsafe characters in the cell file are refused |
| R10 | The real templates satisfy the grammar (contract) | AP-CAT-01 `templates --check` over the repository's 23 TPL files exits 0; -02 a frozen golden JSON catalog (cells and placeholder sets per template) detects any template drift; -03 every TPL placeholder in a code context is a quoted or bare slot, so none is unescapable; -04 the canonical and mirror trees produce identical catalogs |
| R11 | Every real template renders in both modes | AP-CAT-10 each template renders with generated typed dummy values for every slot (strings `x`, numbers 1, bools false, lists `[]`, secrets `null`, T28 Cell 2 from a fixture), in notebook and script mode, into temporary directories; output parses (notebook) or is checked for markers and dropped lines (script). Nothing is executed. |
| R12 | Reward samples equal the documented examples | AP-RW-01 each `--format` output equals the doc example constant (golden); -02 an unknown format is a usage error |
| R13 | Reward envelope checks (C10, C11) | AP-RW-10 Option A ok; -11 Option B with a string body ok; -12 Option B with a list body rejected (D-1 reproduction); -13 `{"error": ...}` rejected (D-2 reproduction); -14 status 400/500 reported; -15 count mismatch; -16 id mismatch and missing id (verl input without id is allowed); -17 metrics type outside `Reward`/`Metric`, a non-finite value, a missing name; -18 diagnostics carry positions only (a sentinel string in a value is absent from the output); -19 duplicate JSON keys and a BOM are parse errors (shared parser) |
| R14 | CLI contract | AP-CLI-01 `diag_contract.rs` passes with the new routes; -02 help lists every route and `docs/migration/ai-payloads.md` mentions each (registry test `every_slice_documents_its_commands`); -03 exit codes 0/1/2 through the real binary (`tests/ai_payloads.rs`, process tests) |
| R15 | Callers repointed and mirrors in sync | AP-DOC-01 the four `code_output_guide.md` files and the training/processing `configuration.md` name `toolkit ai-payloads render`; -02 `toolkit sync plugin-skills --check` exits 0 after S4; -03 each reward-template caller page names `toolkit ai-payloads reward check` |
| R16 | Inventory dispositions | AP-INV-01 every row with `issue == 4386` carries the §3 disposition, none `converted`; -02 T08 and T09 reclassified (OD-7); -03 totals 0 removed and 56 retained |

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
- **B-6.** Typed slots: quoted slots take only strings and bare slots only
  literals. A value can no longer inject code or a truthy `"false"`.
- **B-7.** Script numbering and name validation are deterministic, and the tool
  refuses to overwrite.
- **B-8.** In script mode each section starts with `# %% Cell N: Label`, and
  Cell 0 becomes the module docstring. The guide allowed any IDE-compatible
  form.
- **B-9.** `--append` rewrites the existing notebook with 2-space indentation.
  Its content is unchanged, but the whitespace may differ from what Jupyter
  wrote (Jupyter uses a 1-space indent).
- **B-10.** The new reward checker reports D-1 and D-2 when given those
  templates' responses. The templates themselves are unchanged (P-4).
- **Unchanged.** The template text, the `python3 <script>.py` execution step,
  mode selection, the append question and every SDK call.

## 7. Implementation slices (ordered; tests first in each)

Each slice commits its failing tests first, then the implementation, then
docs, on one RSI branch. Verification is daemon recipe jobs plus a
`scoped_test`. Review is by the other model family against the exact SHA.

| Slice | Content | Tests written first | Depends on |
| --- | --- | --- | --- |
| S1 `ai-payloads-model` | `template.rs` (grammar, lexer, classification), `templates` command with `--json` and `--check`, registry line, `docs/migration/ai-payloads.md` skeleton | AP-TPL-*, AP-SUB-01..07, AP-CAT-01..04, AP-CLI-01/02 for `templates` | #4386 design accepted (this document) |
| S2 `ai-payloads-render` | `values.rs`, `notebook.rs`, `script.rs`, `place.rs`, `render` command | AP-SUB-10..22, AP-SEC-*, AP-NB-*, AP-SCR-*, AP-FS-*, AP-CELL-*, AP-CAT-10, AP-CLI-03 | S1 |
| S3 `ai-payloads-reward` | `reward.rs`, `reward sample` and `reward check` | AP-RW-* | S1 (registry and module only); independent of S2 |
| S4 `ai-payloads-callers` | Edit the canonical skill prose: the four `code_output_guide.md` files, training/processing `configuration.md`, the `overview.md` and reference pages in §1.2 that describe hand rendering, `create-reward-function.md` and `rlvr_reward_function.md` (testing step). Mirrors are regenerated with `toolkit sync plugin-skills`. Templates are not edited. | AP-DOC-* | S2, S3 |
| S5 `ai-payloads-inventory` | `python-inventory.json`: dispositions, OD-7 reclassification, `status` values; this document's status line; `docs/migration/README.md` row note | AP-INV-* | S4 |

S1–S3 are small enough for one implementer each. S2 is the largest, at about
1,200 lines of Rust including tests. S4 and S5 are prose and JSON edits with
contract tests.

## 8. Unresolved items: proposed Issues (AC4)

None has been created, as the brief requires. The manager files the Issues it
accepts.

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
  This adds customer content; it does not migrate anything.
- **P-3. Documented non-Python access to SageMaker managed MLflow.** Find, in
  primary AWS documentation, the SigV4 signing scope and REST endpoint (C13),
  then decide on `toolkit ai-payloads mlflow metrics --training-job NAME`
  (`DescribeTrainingJob` for `MlflowConfig`/run id, then MLflow
  `metrics/get-history`). Until then T10 stays K-PY. Blocked on documentation,
  not on engineering.
- **P-4. Reward-template envelope defects D-1 (T09) and D-2 (T06).** These are
  product-content fixes to customer templates. They need an explicit decision,
  because the existing Python is migration evidence. Evidence: C10 Option B
  text, and the AP-RW-12/13 reproductions from S3.
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

## 9. Open decisions (each with a recommendation)

| ID | Decision | Recommendation |
| --- | --- | --- |
| OD-1 | Replace the notebook/script product flow with Rust controllers that call AWS directly? | **No, not in #4386.** Keep the customer-language templates (K-PY) and file P-1 as a product choice. C1 and C2 show the services allow it, but the customer UX is Python notebooks. |
| OD-2 | In notebook mode, uncomment `# %pip ... # NOTEBOOK_ONLY` lines? | **No.** Keep lines verbatim, so the output equals the template, and fix the templates through P-5. |
| OD-3 | Support `assign` for empty configuration fields (`BASE_MODEL = ""`)? | **Yes, in S2, narrowly.** Allow only single-line, top-level `NAME = <literal>` in sections 1–2, with the same typed and escaped rules as slots. Refuse when `NAME` occurs more than once as an assignment target. Without it the agent must still hand-edit rendered files. |
| OD-4 | Rename `.py` templates to a template extension so the Python count drops? | **No.** That hides Python without removing it (AC3, AGENTS.md), loses editor support for customers, and breaks every caller path. |
| OD-5 | Notebook `nbformat_minor` and `metadata` | **4.4** (the guide's value; no cell ids required). `metadata` = `{"kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"}, "language_info": {"name": "python"}}`. Moving to 4.5 with generated ids is a later, separate change. |
| OD-6 | Size cap for `--append` notebooks | **16 MiB.** Notebooks with outputs exceed the 1 MiB text cap; refuse above 16 MiB with a clear message. |
| OD-7 | Inventory representation of K-PY rows | **Keep the `classification` vocabulary.** Correct T08 and T09 to `external-service-payload`. Add `disposition: "retained-customer-language"` and `language_contract: "C1"`/`"C9"`/`"C13"` (anchors to §2), and set `status` to `retained`. The 23 TPL rows keep `external-service-payload` with the TPL disposition note, rather than adding a new class. |
| OD-8 | Where the toolkit finds templates | **Explicit path.** `render TEMPLATE` takes the file path the agent already read. `templates --root` defaults to the installed skill's `references/`. No copy is embedded in the binary, so the skill tree stays the single source (AGENTS.md: avoid reimplementing canonical skills). |
| OD-9 | Reward `check` for non-200 Option B | **Report as failure (exit 1) and name the documented effect.** The sample is counted in `byoc_failure_count` and its metrics are dropped (C10). Do not treat it as valid. |
| OD-10 | Owner model | **Keep the Issue's owner** (DeepSeek primary, GLM review) for S1–S3. S4 and S5 are prose and JSON and can go to either family. |

## 10. Sources

Every source is listed in the §2 table with its section. The SageMaker Python
SDK pages (C1, C12) are vendor documentation for the AWS-owned SDK. All other
sources are AWS service documentation, except C13's MLflow REST reference
(mlflow.org) and C14 (nbformat, Jupyter project).
