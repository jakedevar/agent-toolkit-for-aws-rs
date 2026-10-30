# RLVR Lambda Reward Function Guide

Plugin references are plain files; the host does not substitute their variables.
Get the absolute installed plugin root from the loaded parent skill (its
`${CLAUDE_PLUGIN_ROOT}` token is substituted there). In your own terminal, set
`export CLAUDE_PLUGIN_ROOT='/absolute/installed/plugin'` to that actual path.
The guarded commands below refuse an unset or empty root; never use PATH.

## What Is a Lambda Reward Function?

For RLVR (Reinforcement Learning from Verifiable Rewards) training, a Lambda reward function is an AWS Lambda that evaluates model outputs during training and returns numerical reward scores. SageMaker invokes this Lambda within the training loop to provide learning signals that guide model optimization.

---

## Helping Users Create Custom Reward Functions

Tell user: I will now review your use case and data, as well as my own resources, to propose a reward function that you can use to train your model. I will do my best to match it to your needs, but I strongly suggest that you review it carefully before starting the training.

### Step 1: Analyze the Use Case

Reward functions are specific to the use case and dataset. Consider the task and data format to understand what constitutes a good output and how to measure it.

1. Review these materials:
   - `use_case_spec.md` — problem description and success criteria
   - Conversation context — the user's goals
   - 20 rows of training data — structure and content of the expected responses

2. Answer these questions internally (involve the user if you need clarification):
   - Given the analysis in (1), what makes a good response? A bad response? A partially correct response?
   - Which aspects of the response can be verified programmatically?
   - Are there specific constraints or formats the output must follow?
   - How would the base model's initial responses during early training likely look?

---

### Step 2: Analyze the Structure of the Response

- Which parts of the response contain the content you want to verify programmatically?
- How are those parts delimited? How can they be parsed?
- How rigid should the extraction patterns be, given the 20 rows of data reviewed?
- Are there special formats to account for (fractions, LaTeX, Unicode, Markdown, etc.)? How do they affect the extraction logic?
- Does the base model include a thinking block in its output?
- Does the use case require changing the model's behavior within the thinking block, or only in the final response?
- If warranted, how can the response format/schema be verified programmatically?
- If there is a ground truth in the data:
  - Does the model's response need to match it exactly?
  - Does a partial match count? If so, how?
  - How can you deterministically decide whether the response is close enough to the ground truth?

---

### Step 3: Plan the Verification Logic

- Write a function that extracts the verifiable parts identified in Steps 1 and 2 from the response.
- Identify the most suitable and performant tools for checking format or schema (e.g., which Python libraries?).
- If you need to validate generated code, write a function that executes it and returns a pass/fail/test result with a corresponding reward score.
- Are there keywords to check for? Which ones, and how many need to be present?
- What is the appropriate similarity function for comparing the response to the ground truth?
- If the response contains a block of text where the choice of words can vary slightly and still be correct, how can you verify that it is similar enough to the ground truth?
- Share the plan with the user and get confirmation before proceeding.

### Step 4: Add Anti-Gaming Checks

Add at least two mechanisms to detect and penalize gaming behavior. Common gaming patterns include:

- **Padding** — inserting filler characters to inflate response length
- **Skipping steps** — jumping to a final answer without showing required reasoning
- **Repetition** — filling length requirements with repeated whitespace or words
- **Dummy content** — using placeholder text instead of genuine answers
- **Echo attack** — repeating the prompt or question back as the answer
- **Nonsense** — producing incoherent or irrelevant text

---

### Step 5: Design the Aggregation Method

If the use case allows, think of rewards as a pyramid where each layer depends on the one beneath it. No credit is given for higher layers until lower ones are fully satisfied.

- **Layer 1 (Foundation) — Structure**
  - Is the output formatted correctly and machine-parsable?
  - Example: If JSON is expected, is it valid JSON? Are all required fields filled?

- **Layer 2 (Core) — Semantics**
  - Is the output factually correct and does it deliver real value?
  - Example: Can generated code pass unit tests? Is the math answer correct?

- **Layer 3 (Polish) — Behavior**
  - Does the output meet operational and safety requirements?
  - Example: Is the response concise? Free from toxic content? Complete?

- **Aggregation**
  - What is the most suitable weighted distribution across these layers and their sub-components?
  - Ensure each component function returns a spread of scores even for low-quality responses. If a component returns 0 for 90%+ of plausible early-training outputs, it will flatten the reward signal and stall learning.
  - Briefly share the reasoning with the user and get confirmation.

---

### Step 6: Write the Function as a Python Script

1. Create a file called `lambda_function.py` in the project's scripts directory.
2. Read the `directory-management` reference to determine the correct directory for storing scripts.
3. Consult the reward function templates for structural reference:
   - Nova 2.0 Lite → `templates/nova_rlvr_reward_function_source_template.py`
   - All other models → `templates/rlvr_reward_function_source_template.py`

**Critical rules:**

- The `lambda_handler` function must be copied to `lambda_function.py` exactly as given in the template. Do not change its signature or internal logic.
- The chat template used in the example reward functions is correct. Use it to extract the assistant's response. Then apply the parsing logic from Step 2 to extract the parts of the response you want to score.
- Do not copy anything beyond the `lambda_handler` and the assistant-response extraction. The rest of the template is an example that **will not work** out of the box. You **must customize** the reward logic based on the use case and data, as described in Steps 1–5. Copying the template's reward logic without customization will likely produce flat rewards, wasting the user's time and compute budget.

**Code writing principles:**

1. **Provide a learning gradient:** Return diverse scores across [-1.0, 1.0], with partial credit for partial answers where appropriate — not just {-1, 0, 1}.
2. **Verify correctly:** Use actual parsing tools (`json.loads`, `ast.parse`, etc.), not string matching.
3. **Include all necessary imports:** Add every required import statement at the top of the file.
4. **Execute fast:** Complete in <100 ms with no API calls or blocking operations.
5. **Be deterministic:** Same input → same output, always.
6. **Be bounded:** The final score must always fall within [-1.0, 1.0]. Add `return min(1.0, max(-1.0, score))` at the end.
7. **Comment thoroughly:** Include detailed comments explaining the reward logic.

### Step 7: Test Locally

Test the reward function by executing it against crafted sample data, then check the response against the documented Lambda contract with the Rust checker:

1. **Build test input.** Choose the `--format` that matches the training data: `openai` (chat messages), `verl`, or `nova` for Nova 2.0 Lite. Print the documented event for it:

   ```bash
   "${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" ai-payloads reward sample --format verl
   ```

   Infer the expected Lambda event and response format from that sample and from the `lambda_handler` function in the appropriate source template. Choose one prompt from the training data reviewed in Step 1. Construct four test events that mimic what SageMaker sends to the Lambda:
   - An **excellent** response — use the response from the data.
   - A **partially correct** response — generate one that gets some things right but misses others.
   - A **bad** response — generate one that is clearly wrong or off-topic, but without gaming.
   - A **gaming** response — generate one that tries to get rewards by gaming.

2. Explain what you are doing and show the user the four responses that you want to test.

3. Write the batch to a temp file (e.g., `/tmp/test_reward_input.json`).

4. **Run the function** Invoke `lambda_function.py` via the shell and save the response (`/tmp/test_reward_output.json`):

   ```bash
   python3 -c "
   import sys, json
   sys.path.insert(0, '<project-dir>/scripts')
   from lambda_function import lambda_handler
   with open('/tmp/test_reward_input.json') as f:
       event = json.load(f)
   result = lambda_handler(event, None)
   with open('/tmp/test_reward_output.json', 'w') as f:
       json.dump(result, f)
   print(json.dumps(result, indent=2))
   "

   ```

5. **Check the response format.** SageMaker sends one sample per invocation, so `--batch` is needed for the four-sample file (a local test extension). The checker reads both files offline; it does not run the Lambda or call AWS:

   ```bash
   "${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" ai-payloads reward check --format verl --event /tmp/test_reward_input.json --response /tmp/test_reward_output.json --batch
   ```

   Use the same `--format` as in 1 (`--format openai`, `--format nova`). Exit 0 means the response matches one of the two documented envelopes with one result per sample. On exit 1, fix `lambda_handler`'s return value (it must not be changed from the template's schema, so a failure usually means the function was edited incorrectly) and re-run.

6. **Verify the output.** Check that:
   - All scores fall within [-1.0, 1.0].
   - The excellent response scores highest, the bad response scores lowest.
   - No errors or exceptions occurred.

7. **Show the user** the test inputs, expected score ordering, and actual scores.

8. If the test fails or scores don't match expectations, fix the function and re-run until it passes. Inform the user about what you are doing.

### Step 8: Check In with the User

- Share the path to the reward function with the user.
- Remind user that this is only a suggestion, and emphasize the need to review the reward function before launching the training. It is up to them to decide if they want to use it, edit it, or choose not to use it.
- Let the user know that the source templates are also available for them under finetuning/templates, if they want to compare your function to them or customize them on their own.

### Step 9: Register the Reward Function in the Finetuning Output

The registration code is Cell 3 of `code_templates/rlvr.py`. Do not write it by hand: `"${CLAUDE_PLUGIN_ROOT:?Set CLAUDE_PLUGIN_ROOT to the absolute installed plugin root}/libexec/toolkit" ai-payloads render` renders it with the rest of the finetuning output (see `finetuning/overview.md` section 1.3), so finish this step before that render. After the reward function is written and tested, put the path where `lambda_function.py` was saved in Step 6 and the Evaluator name into the values file:

```json
{
  "values": {"GENERATE A NAME FOR THE EVALUATOR HERE": "<evaluator-name>"},
  "assign": {"reward_function_path": "<project-dir>/scripts/lambda_function.py"}
}
```

`reward_function_path` is a literal field of Cell 3 and `GENERATE A NAME FOR THE EVALUATOR HERE` is its Evaluator name placeholder. Cell 3 then reads `Evaluator.create(name=..., type="RewardFunction", source=reward_function_path)` and `CUSTOM_REWARD_FUNCTION = evaluator.arn`, in the format already chosen for this session (notebook or script).

Generate an appropriate name for the Evaluator based on the use case and current context.

- Format: lowercase, alphanumeric with hyphens only, 1–20 characters
- Pattern: `[a-zA-Z0-9](-*[a-zA-Z0-9]){0,20}`
