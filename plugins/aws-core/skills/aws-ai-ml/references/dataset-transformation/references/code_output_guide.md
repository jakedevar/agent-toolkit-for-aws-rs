# Code Output Guide

## Mode Selection

Ask the user once before generating code: **"Would you like me to generate a Jupyter notebook or a Python script?"**

If the output format has already been decided in the conversation context, keep consistent — do not re-ask.

## Shared Rules (Both Modes)

- Do not write the notebook or script from a template by hand. `toolkit ai-payloads render` produces it, and its output uses EXACTLY the imports in the template — do not add extras
- Fill every `[PLACEHOLDER]` with user-specific configuration through the values file, not by editing the output
- Keep the `set_attribution(Attribution.SAGEMAKER_AGENT_PLUGIN)` call in the setup cell/section; the template includes it and `render` keeps it

## Rendering a Template

Code is written by the Rust command `toolkit ai-payloads render`, never by hand. It splits the template on its `# Cell N: Label` markers, fills the placeholders with typed and escaped values, handles `# NOTEBOOK_ONLY` lines and `# NOTEBOOK_ONLY_SECTION` sections for the chosen mode, and publishes the result as a new file. It never overwrites, renames or deletes an existing file.

`<skill-dir>` is the directory that holds this skill's `references/` directory. A template is `<skill-dir>/references/<area>/code_templates/<template>.py`, the path the calling reference names.

1. **List the template's fields.**

   ```bash
   toolkit ai-payloads templates --root <skill-dir>
   ```

   Find the block for your template. `slot NAME` lines are the `[NAME]` placeholders, filled through `values`. `target NAME` lines are literal configuration fields such as `BASE_MODEL = ""`, filled through `assign`. `manual` lines are edit points that stay yours (see Manual Edit Points).

2. **Write the values file** outside the project directory, for example `<scratch-dir>/values.json`. Names have no brackets:

   ```json
   {"values": {"REGION": "us-east-1"}, "assign": {"BASE_MODEL": "meta-textgeneration-llama-3-8b"}}
   ```

   - Strings are plain text; the command escapes them. Use JSON `true`/`false` for flags, JSON integers for counts and JSON arrays of strings for lists.
   - `[PROJECT_DIR]` defaults to `--project-dir`; leave it out.
   - A secret placeholder (a name containing `TOKEN`, `SECRET`, `PASSWORD` or `CREDENTIAL`) accepts only `null`, `"None"` or a Secrets Manager ARN. Never put a literal token in the values file.
   - A missing value is an error that lists the names. Ask the user for them. `--allow-unresolved` only keeps the `[NAME]` text of quoted placeholders; flags, numbers and lists are always required. Do not use it to skip a question.

3. **Render.**

   ```bash
   toolkit ai-payloads render <skill-dir>/references/dataset-transformation/code_templates/transformation.py --mode notebook --project-dir <project-dir> --values <scratch-dir>/values.json
   toolkit ai-payloads render <skill-dir>/references/dataset-transformation/code_templates/transformation.py --mode script --project-dir <project-dir> --values <scratch-dir>/values.json
   ```

   Run the one that matches the mode. `--name <name>` chooses the file name (default: the project directory's name; lowercase letters, digits, `_` and `-`, and not ending in `-` and two digits). `--dry-run` prints the output path and writes nothing. `--cell N=FILE` replaces the body of Cell N with a file when the calling reference says so.

4. **Read the result.** Success prints `output: <path>`, and then one `manual:` line per edit point that is left to you. Any failure writes nothing and names the placeholder or cell, never its value. Fix the cause and run the command again.

## Consent (EULA)

A placeholder or field named `ACCEPT_EULA` is the user's acceptance of a model license. Never accept for the user, and "proceed without asking" is not acceptance. Only when the user has explicitly accepted the license, set `ACCEPT_EULA` to `true` in the values file (under `values` for a `[ACCEPT_EULA]` placeholder, under `assign` for an `ACCEPT_EULA = False` field) and add `--consent ACCEPT_EULA` to the render command:

```bash
toolkit ai-payloads render <skill-dir>/references/dataset-transformation/code_templates/transformation.py --mode notebook --project-dir <project-dir> --values <scratch-dir>/values.json --consent ACCEPT_EULA
```

`true` without that flag is refused. Otherwise set `false` and leave the flag out.

## Manual Edit Points

For each `manual:` line, `render` leaves an edit to you: `NOTE FOR AGENT` and `Uncomment` lines in the template, dict members such as `"accept_eula"`, and expression values such as `S3_OUTPUT_PATH = f"..."`. Make these edits on the rendered file, as the calling reference instructs, and tell the user what you changed. Do not render again over them: an append copies the earlier cells unchanged, so manual edits already made stay in both the old and the new notebook.

## Notebook Mode

`--mode notebook` writes a Jupyter notebook in `<project-dir>/notebooks/`. `# Cell 0 [markdown]:` becomes a markdown cell and every other cell a code cell. Do not write notebook JSON yourself or with shell commands; change a notebook only through the manual edit points.

**Naming and appending:**

- Notebook path: `<project-dir>/notebooks/<project-name>.ipynb`
- If it doesn't exist → `render` creates it
- If it already exists → `render` refuses and changes nothing. Ask: _"Would you like me to append cells to the existing notebook, or create a new one?"_
  - **Append:** ask the user to save the notebook, and wait. Then run the same command with `--append`:

    ```bash
    toolkit ai-payloads render <skill-dir>/references/dataset-transformation/code_templates/transformation.py --mode notebook --project-dir <project-dir> --values <scratch-dir>/values.json --append
    ```

    The existing notebook is never modified: `render` publishes a new versioned copy (`<project-name>-01.ipynb`, then `-02`, and so on) that holds the existing cells, the template's `# Cell 0 [markdown]:` cell as the section divider and the new cells. It prints `output:` and `source:`, and `open:` with the path of the new copy. Tell the user to open the `open:` path and continue there; the earlier file stays as it was.
  - **Create a new one:** run it with `--new` and a different `--name`.

**Execution:**

- If notebook execution tools (e.g., `run_cell` MCP) are available, offer to run cells for the user. If not available, tell the user to run cells themselves.
- Do NOT use bash commands or inline scripts to execute notebook cells.

## Script Mode

`--mode script` writes the next numbered script, `<project-dir>/scripts/NN_<name>.py` (`01` in an empty directory, otherwise one past the highest number). It never overwrites a script. The script is plain Python with `# %%` cell markers (IDE-compatible) and a docstring made from Cell 0; `# NOTEBOOK_ONLY` lines and sections are dropped.

**Dependencies:**

- Install any required pip packages directly (e.g., `pip install 'sagemaker>=3.7.1,<4.0'`) before running the script. Do not embed install commands in the script itself.

**Execution:**

- Run the script using standard Python execution (`python3 <script>.py`).
