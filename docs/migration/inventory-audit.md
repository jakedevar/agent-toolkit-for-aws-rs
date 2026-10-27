# Python inventory audit (#4371) — corrected revision

Status: documentation audit only, delivered for independent exact-SHA review. No slice
implementation, no Rust verification, no build job and no landed state is claimed here.
Implementation work belongs to the domain Issues named in the slice table (§6).

Provenance:

- Programme: #4369. This Issue: #4371 (slice `inventory`, wave 0).
- Audit source revision: `71319f9aecb7f81c872aa1ee0060f9474bc7cd1b` ("SRC"), the revision
  whose report the manager rejected with CHANGES REQUESTED. It is not this document's
  commit.
- Repair revision: the commit that contains this file on the allocated branch
  `rsi/c0d775a7-2819-42a4-86f0-91adc6219073` (worker `c0d775a7`, continuing worker
  `8268e555`); its exact SHA is named in the #4371 handoff. Commit `28de1124` on the same
  branch only imported the unverified `8268e555` draft; every figure below was recomputed
  afterwards. No reset, rebase or branch change was performed; this branch descends from
  SRC.
- Inventory under audit: `docs/migration/python-inventory.json`, whose `baseline_commit` is
  `86edeadd0da48e03a8d3045e2775881bd6450e68` ("baseline"). Baseline and SRC are different
  objects and are reported separately. They agree on the `*.py` tree: no `*.py` path differs
  between them (R2b = 0), and every row's `original_blob` equals the blob committed at SRC
  (R3). All measurements below were taken from committed Git objects at SRC.
- Write scope of this repair: exactly two documentation paths,
  `docs/migration/inventory-audit.md` and `docs/migration/python-inventory.json`. The
  manifest change is limited to the `classification_status` provenance string; no row, blob,
  path, baseline or schema field was altered.
- Earlier uncommitted drafts (DeepSeek `70120f07`, GLM `8268e555`) were treated as
  unverified input. Where they disagreed with the commands in §3 they were corrected; see
  §1.

## 1. Corrections applied to the rejected report and to the earlier drafts

| # | Earlier claim | Corrected position in this revision | Evidence |
| --- | --- | --- | --- |
| C1 | 216 distinct blobs | 271 distinct blobs; 271 canonical rows | §4, R3, R4 |
| C2 | 95 non-self canonical rows | 120 non-self (mirror) rows | §4, R4 |
| C3 | Audit baseline `f86cc8f` | Audit source is `71319f9a`; inventory baseline is `86edeadd`; reported separately | header, R1, R2b |
| C4 | Duplicate families "46 notebook templates + 45 telemetry copies + 3 MSK + 1 emit-plan-json" (=95) | Eight families; 84 multi-row blob groups (75 of size 2, 9 of size 6) covering 204 rows, 187 singleton blobs, 120 mirror rows | §5, R5, R7 |
| C5 | Rust scope narrowed to 341 files; 50 remote payload files "not port targets" | All 391 rows carry an explicit final disposition. 341 rows are Rust port targets; the 50 payload rows stay in scope as unresolved managed-service compatibility questions owned by #4386 | §7 |
| C6 | `uvx mcp-proxy-for-aws` classified out of scope | Local Python runtime dependency in six `.mcp.json`/`mcp.json` files; explicitly in scope of #4413 | §8.2 |
| C7 | Telemetry Rust replacement must retain the nine-file vendored Python surface | Byte parity is evidence for #4398/#4377; a Rust implementation may change bundle layout, callers and sidecars atomically; no Python surface is preserved by contract | §8.5 |
| C8 | Secret-hook fail-open presented as a routine operator choice | Safety defect, resolved by engineering in #4412 | §8.4 |
| C9 | "no Python executed from the repository tree" implying compliance | The predecessor executed Python in at least 17 ToolUse events; stated plainly | §2 |
| C10 | `ast.parse` results used as evidence | Withdrawn; replaced by permitted source inspection or marked unknown | §2, U-3 |
| C11 | Findings numbering and cross-references | F-1…F-14 with an evidence pointer each; every reference resolves inside this document | §10 |
| C12 | "391/391 paths tracked, equal to `git ls-files -- '*.py'`" | Reconfirmed with a working command (`git ls-tree -r --name-only` piped to `rg '\.py$'`; `git ls-tree` does not glob its pathspec) | R2 |
| C13 | Earlier draft commands read `.paths[]` and `.slice_issue` | The manifest array is `.files[]` and the issue field is `.issue`; every command in §3 was rewritten and run as printed | §3 |
| C14 | Earlier draft: helper 158/95, partition "100 groups / 220 rows / 171 singletons", startup-advisor scripts "5 groups / 5 mirrors", family mirror column summing to 124 | Helper is 160 canonical / 93 mirror; partition is 84 / 204 / 187; startup-advisor scripts is 1 group / 1 mirror; family mirrors sum to 120 | §4, §5 |
| C15 | Earlier draft: 46 AI/ML rows under `code_templates/templates/`, 7 of 23 files starting with IPython magic lines | No tracked inventory path contains `code_templates/templates`; the claim had no support and is withdrawn | §5, U-3 |
| C16 | Earlier draft: the secret hook launcher "succeeds without running the hook" when no interpreter exists | The chain ends in `py -3 "$0"` with no `|| true`, so a missing interpreter ends in a non-zero shell exit (127), not success; whether the host treats that as non-blocking is host behaviour, not verified here (U-7) | §8.4 |
| C17 | Earlier draft: `uvx` MCP launch in one file | Six config files across three plugins | §8.2, R12 |
| C18 | Earlier draft: §9 restated the ACs under different wording | §9 now answers the Issue's own AC1–AC5 | §9 |

## 2. Instruction-compliance record

The work rule for this Issue, from repository `AGENTS.md` and the launch brief, prohibits
executing Python at all: no `python`, no `python3`, no `python3 -c`, no stdin script, no
parser, no wrapper, no repository import, no scratch analysis.

- **Predecessor session (source `71319f9a`).** ToolUse events 15, 19, 24, 28, 30, 32, 34,
  36, 38, 40, 42, 48, 90, 92, 94, 96 and 100, and further events not itemised by the
  manager, invoked `python3`, including `-c` stdin analysis and `ast.parse` over notebook
  templates. That violates the rule. Pointing the interpreter at files outside the
  repository tree does not excuse it, and the rejected report's phrase "not from repository
  tree" is withdrawn as a compliance claim.
- **Consequence for evidence.** No predecessor conclusion that depended on a Python process
  is accepted. Each was re-derived with a permitted tool and is restated with its command
  (C1, C2, C4, C11, §5) or is marked unknown (U-3). Nothing in §4–§7 rests on executed
  Python.
- **This revision (`c0d775a7`).** No Python was invoked in any form. The earlier drafts were not trusted for
  any figure; `8268e555`'s final report also states it ran no Python. Analysis used `git`, `jq`,
  `rg`, `awk`, `sed`, `sort`, `uniq`, `comm`, `join`, `cut`, `wc` and `bash` for analysis (and `perl` only to edit this
  document's text), over
  committed Git objects and working-tree text. No Cargo build or test, no AWS call (live or
  paid) and no network request was made; no file under audit was executed. The agent harness file tools and RSI controls were also used. The
  corrective checks are reproducible from §3 without Python.

## 3. Method and reproduction

All commands are read-only and non-Python. Run from the repository root with `bash`;
`/tmp/inv-audit` is scratch space outside the repository. The block below is the literal
script that produced every number in §4–§10; the section after it ("Observed output") is its
real output, captured from a run of the exact block as printed here, against this document
as committed (the R8/R9 diffs compare the tables in §6 and §7 with the regenerated ones).

```sh
# Run from the repository root with bash. Read-only: git, jq, rg, awk, sort, join, comm, wc, sed.
INV=docs/migration/python-inventory.json
DOC=docs/migration/inventory-audit.md
SRC=71319f9aecb7f81c872aa1ee0060f9474bc7cd1b
T=/tmp/inv-audit; mkdir -p "$T"

echo "== R1 baseline, provenance string, row count"
jq -r '.baseline_commit, .classification_status, (.files|length)' "$INV"

echo "== R2 tracked *.py paths at SRC; set difference with inventory paths (empty)"
git ls-tree -r --name-only "$SRC" | rg '\.py$' | sort > "$T/tracked.txt"
wc -l < "$T/tracked.txt"
comm -3 <(jq -r '.files[].path' "$INV" | sort) "$T/tracked.txt"
echo "== R2b *.py paths changed between the inventory baseline and SRC"
git diff --name-only 86edeadd0da48e03a8d3045e2775881bd6450e68 "$SRC" | rg '\.py$' | wc -l

echo "== R3 distinct blobs; rows joined to a committed blob; original_blob mismatches (empty)"
git ls-tree -r "$SRC" | rg '\.py$' | awk '{print $3}' | sort -u | wc -l
git ls-tree -r "$SRC" | rg '\.py$' | awk '{print $4"\t"$3}' | sort > "$T/blobs.tsv"
jq -r '.files[] | [.path, .original_blob] | @tsv' "$INV" | sort | join -t$'\t' "$T/blobs.tsv" - > "$T/joined.tsv"
wc -l < "$T/joined.tsv"
awk -F'\t' '$2!=$3' "$T/joined.tsv"

echo "== R4 canonical paths: distinct; resolved to a tracked blob; blob mismatches (empty); non-self rows"
jq -r '.files[] | [.canonical_identical_path, .original_blob] | @tsv' "$INV" | sort -u > "$T/canon.tsv"
cut -f1 "$T/canon.tsv" | sort -u | wc -l
join -t$'\t' "$T/canon.tsv" "$T/blobs.tsv" > "$T/canon-joined.tsv"
wc -l < "$T/canon-joined.tsv"
awk -F'\t' '$2!=$3' "$T/canon-joined.tsv"
jq -r '.files[] | select(.path != .canonical_identical_path) | .path' "$INV" | wc -l

echo "== R5 blob-group sizes (count of groups, group size)"
jq -r '.files[].original_blob' "$INV" | sort | uniq -c | awk '{print $1}' | sort -n | uniq -c

echo "== R6 row table with per-blob line counts (newline count of the committed blob)"
jq -r '.files[] | [.path, .original_blob, .classification, .slice, .issue, (if .path == .canonical_identical_path then "canonical" else "mirror" end)] | @tsv' "$INV" | LC_ALL=C sort > "$T/rows.tsv"
while IFS=$'\t' read -r p b c s i r; do
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$p" "$b" "$c" "$s" "$i" "$r" "$(git cat-file blob "$b" | wc -l)"
done < "$T/rows.tsv" > "$T/rows-lines.tsv"
wc -l < "$T/rows-lines.tsv"
echo "-- raw lines, then lines over distinct blobs"
awk -F'\t' '{raw+=$7; if(!($2 in seen)){seen[$2]=1; dd+=$7}} END{print raw; print dd}' "$T/rows-lines.tsv"
echo "-- class: rows, canonical, mirror, raw lines, dedup lines"
awk -F'\t' '{n[$3]++; r[$3]+=$7; if($6=="canonical"){c[$3]++; d[$3]+=$7} else m[$3]++}
  END{for(k in n) printf "%s\t%d\t%d\t%d\t%d\t%d\n", k, n[k], c[k], m[k], r[k], d[k]}' "$T/rows-lines.tsv" | sort

echo "== R7 multi-row blob groups by family: groups, rows, mirrors (family = slice of the canonical row)"
awk -F'\t' '{n[$2]++; if($6=="mirror") m[$2]++; else s[$2]=$4}
  END{for(b in n) if(n[b]>1){f=s[b]; if(f ~ /^ai-/) f="ai-ml"; else if(f ~ /^cw-/) f="cloudwatch"; else if(f=="startup-validators") f="startup-scripts"
        g[f]++; r[f]+=n[b]; mm[f]+=m[b]; G++; R+=n[b]; M+=m[b]}
      for(f in g) printf "%s\t%d\t%d\t%d\n", f, g[f], r[f], mm[f]; printf "total\t%d\t%d\t%d\n", G, R, M}' "$T/rows-lines.tsv" | sort
echo "-- rows in multi-row groups, by owning slice"
awk -F'\t' '{n[$2]++; b[NR]=$2; sl[NR]=$4} END{for(i=1;i<=NR;i++) if(n[b[i]]>1) c[sl[i]]++; for(k in c) print k"\t"c[k]}' "$T/rows-lines.tsv" | sort

echo "== R8 per-slice table in section 6, regenerated and diffed against this document (empty)"
awk -F'\t' '{k=$4" | #"$5; n[k]++; if($3=="toolkit-helper")t[k]++; else if($3=="test-or-diagnostic")d[k]++; else p[k]++}
  END{for(k in n) printf "| %s | %d | %d | %d | %d |\n", k, n[k], t[k], d[k], p[k]}' "$T/rows-lines.tsv" | LC_ALL=C sort > "$T/sec6.txt"
wc -l < "$T/sec6.txt"
rg '^\| [a-z-]+ \| #[0-9]+ \| [0-9]+ \| [0-9]+ \| [0-9]+ \| [0-9]+ \|$' "$DOC" | LC_ALL=C sort | diff - "$T/sec6.txt"

echo "== R9 disposition table in section 7, regenerated and diffed against this document (empty); counts"
awk -F'\t' '{cl=($3=="toolkit-helper"?"T":($3=="test-or-diagnostic"?"D":"P"));
  if($3=="external-service-payload") d=($6=="canonical"?"PAYLOAD-COMPAT-Q":"PAYLOAD-COMPAT-Q-MIRROR"); else d=($6=="canonical"?"PORT-RUST":"PORT-RUST-VIA-CANONICAL");
  printf "| `%s` | %s | %s | %s | %s |\n", $1, $4, cl, $6, d}' "$T/rows-lines.tsv" | LC_ALL=C sort > "$T/sec7.txt"
wc -l < "$T/sec7.txt"
rg '^\| `(plugins|skills|tools)/' "$DOC" | LC_ALL=C sort | diff - "$T/sec7.txt"
awk -F' [|] ' '{print $5}' "$T/sec7.txt" | sed 's/ |$//' | sort | uniq -c

echo "== R10 basename-rule tests vs the test-or-diagnostic class"
awk -F'\t' '{n=split($1,a,"/"); f=a[n]; if(f ~ /^test_/ || f ~ /_test\.py$/ || f=="conftest.py"){k++; raw+=$7; if(!($2 in s)){s[$2]=1; dd+=$7}; if($3!="test-or-diagnostic") print "name-match but not class D:", $1}}
  END{print k, raw, dd}' "$T/rows-lines.tsv"
awk -F'\t' '$3=="test-or-diagnostic"{n=split($1,a,"/"); f=a[n]; if(!(f ~ /^test_/ || f ~ /_test\.py$/ || f=="conftest.py")) print $6, $1}' "$T/rows-lines.tsv"
echo "-- README claim"
rg -o 'including [0-9,]+ lines in named tests/diagnostics' docs/migration/README.md

echo "== R11 non-.py tracked files whose first line is a python shebang; total non-.py files"
git ls-tree -r "$SRC" | awk '{print $3"\t"$4}' | rg -v '\.py$' > "$T/nonpy.tsv"
wc -l < "$T/nonpy.tsv"
while IFS=$'\t' read -r b p; do
  l=$(git cat-file -p "$b" | head -n1 | head -c 200 | tr -d '\0')
  case "$l" in '#!'*python*) echo "$p: $l";; esac
done < "$T/nonpy.tsv"

echo "== R12 local Python launch surfaces: uvx MCP config files; hook launchers; mise/CI references"
git grep -l '"command": "uvx"' "$SRC" -- 'plugins/*/.mcp.json' 'plugins/*/mcp.json' | sed 's/^[^:]*://'
git grep -c -E 'python3 .*\|\| python ' "$SRC" -- '*hooks.json' | sed 's/^[^:]*://'
git grep -n -E 'python|uv run' "$SRC" -- mise.toml plugins/aws-startup-advisor/mise.toml | wc -l
git grep -c -E 'language: python|mise (-C [^ ]+ )?run' "$SRC" -- .github/workflows | sed 's/^[^:]*://'
git grep -l uvx "$SRC" -- plugins skills ':!*.json' | wc -l
git grep -c uvx "$SRC" -- README.md | sed 's/^[^:]*://'
git grep -h -o -E 'uvx iam-policy-autopilot@latest' "$SRC" -- plugins skills | wc -l
git grep -l 'uvx iam-policy-autopilot' "$SRC" -- plugins skills | sed 's/^[^:]*://'

echo "== R13 contract-surface signals over the 271 canonical blobs (static text search; not parsed, not executed)"
awk -F'\t' '$6=="canonical"{print $1"\t"$2"\t"$4}' "$T/rows-lines.tsv" > "$T/canon-rows.tsv"
wc -l < "$T/canon-rows.tsv"
while IFS=$'\t' read -r p b s; do
  git cat-file blob "$b" | awk -v p="$p" '
    /^[[:space:]]*(import|from)[[:space:]]+(boto3|botocore)/ {sdk=1}
    /^[[:space:]]*(import|from)[[:space:]]+(argparse|click|typer)/ {cli=1}
    /^[[:space:]]*if __name__ == .__main__.:/ {main=1}
    /json\.dump|json\.dumps/ {js=1}
    /sys\.exit|SystemExit/ {ex=1}
    END{printf "%s\t%d\t%d\t%d\t%d\t%d\n", p, sdk+0, cli+0, main+0, js+0, ex+0}'
done < "$T/canon-rows.tsv" > "$T/signals.tsv"
awk -F'\t' '{b+=$2; c+=$3; m+=$4; j+=$5; e+=$6} END{printf "boto3/botocore=%d argparse|click|typer=%d __main__=%d json-dump=%d sys.exit=%d\n", b,c,m,j,e}' "$T/signals.tsv"
echo "-- selected non-stdlib import lines: module, canonical files, slices"
while IFS=$'\t' read -r p b s; do
  git cat-file blob "$b" | sed -nE 's/^[[:space:]]*(import|from)[[:space:]]+([A-Za-z0-9_]+).*/\2/p' | sort -u | awk -v s="$s" '{print $1"\t"s}'
done < "$T/canon-rows.tsv" | rg '^(sagemaker|mlflow|bedrock_agentcore|aws_bedrock_token_generator|aws_lambda_powertools|gremlin_python|oracledb|pymongo|valkey|aws_cdk|openai|anthropic|httpx|matplotlib|pandas|pydantic|jsonschema|yaml|jmespath|certifi)\b' \
  | sort | uniq -c | awk '{a[$2]=a[$2]" "$3"("$1")"} END{for(k in a)print k":"a[k]}' | sort

echo "== R14 canonical rows whose basename is mentioned by no tracked non-.py file (outside docs/migration), by slice"
while IFS=$'\t' read -r p b s; do
  n=$(git grep -l -F -- "${p##*/}" "$SRC" -- . ':!*.py' ':!docs/migration' | wc -l)
  printf '%s\t%s\t%s\n' "$p" "$s" "$n"
done < "$T/canon-rows.tsv" > "$T/callers.tsv"
awk -F'\t' '{if($3==0)z++; else r++} END{print "referenced:", r, "unreferenced:", z}' "$T/callers.tsv"
awk -F'\t' '$3==0{print $2}' "$T/callers.tsv" | sort | uniq -c | sort -k1,1nr -k2,2 | awk '{printf "%s=%s ", $2, $1} END{print ""}'

echo "== R15 sizing: top 8 slices by canonical lines (lines, canonical rows, slice); split candidates"
awk -F'\t' '$6=="canonical"{n[$4]++; l[$4]+=$7} END{for(k in n) printf "%d\t%d\t%s\n", l[k], n[k], k}' "$T/rows-lines.tsv" | sort -rn | head -8
echo "-- startup-contracts by file-name theme: theme, rows, lines"
awk -F'\t' '$4=="startup-contracts"{f=$1; sub(/.*\//,"",f); t="other"; if(f~/azure/)t="azure"; else if(f~/gcp|gemini/)t="gcp"; else if(f~/heroku/)t="heroku"; else if(f~/ai_|_ai|gpt6|llm|openrouter|mantle/)t="ai-gpt6"; c[t]++; l[t]+=$7}
  END{for(t in c) printf "%s\t%d\t%d\n", t, c[t], l[t]}' "$T/rows-lines.tsv" | sort
echo "-- telemetry canonical rows by area: area, rows, lines"
awk -F'\t' '$4=="telemetry"&&$6=="canonical"{p=$1; sub(/^.*scripts\/telemetry\//,"",p); a=(p ~ /\//)?p:"top-level"; sub(/\/.*/,"",a); c[a]++; l[a]+=$7} END{for(a in c) printf "%s\t%d\t%d\n", a, c[a], l[a]}' "$T/rows-lines.tsv" | sort
echo "-- ai-payloads canonical rows by reference directory"
awk -F'\t' '$4=="ai-payloads"&&$6=="canonical"{p=$1; sub(/^.*aws-ai-ml\/references\//,"",p); sub(/\/.*/,"",p); c[p]++} END{for(k in c) print k"\t"c[k]}' "$T/rows-lines.tsv" | sort
```

Observed output (real run of the block above; `rg` prints nothing for an empty diff or set):

```text
== R1 baseline, provenance string, row count
86edeadd0da48e03a8d3045e2775881bd6450e68
audited; #4371 report in docs/migration/inventory-audit.md awaits independent review
391
== R2 tracked *.py paths at SRC; set difference with inventory paths (empty)
391
== R2b *.py paths changed between the inventory baseline and SRC
0
== R3 distinct blobs; rows joined to a committed blob; original_blob mismatches (empty)
271
391
== R4 canonical paths: distinct; resolved to a tracked blob; blob mismatches (empty); non-self rows
271
271
120
== R5 blob-group sizes (count of groups, group size)
    187 1
     75 2
      9 6
== R6 row table with per-blob line counts (newline count of the committed blob)
391
-- raw lines, then lines over distinct blobs
127558
97599
-- class: rows, canonical, mirror, raw lines, dedup lines
external-service-payload	50	25	25	7008	3504
test-or-diagnostic	88	86	2	29781	29065
toolkit-helper	253	160	93	90769	65030
== R7 multi-row blob groups by family: groups, rows, mirrors (family = slice of the canonical row)
ai-ml	37	74	37
bedrock-fetch	1	2	1
cloudwatch	26	52	26
launch	6	12	6
msk	3	6	3
serverless	1	2	1
startup-scripts	1	2	1
telemetry	9	54	45
total	84	204	120
-- rows in multi-row groups, by owning slice
ai-data	6
ai-models	12
ai-payloads	56
bedrock-fetch	2
cw-api	26
cw-domain	26
heroku	1
launch	12
msk	6
serverless	2
startup-validators	1
telemetry	54
== R8 per-slice table in section 6, regenerated and diffed against this document (empty)
36
== R9 disposition table in section 7, regenerated and diffed against this document (empty); counts
391
     25 PAYLOAD-COMPAT-Q
     25 PAYLOAD-COMPAT-Q-MIRROR
    246 PORT-RUST
     95 PORT-RUST-VIA-CANONICAL
== R10 basename-rule tests vs the test-or-diagnostic class
84 28630 28314
mirror plugins/aws-data-analytics/skills/migrate-to-msk/scripts/simulation_load_test_config.py
canonical skills/specialized-skills/analytics-skills/migrate-to-msk/scripts/simulation_load_test_config.py
canonical skills/specialized-skills/database-skills/amazon-neptune/scripts/cdk_test_env.py
canonical skills/specialized-skills/database-skills/amazon-neptune/scripts/seed_test_data.py
-- README claim
including 28,208 lines in named tests/diagnostics
== R11 non-.py tracked files whose first line is a python shebang; total non-.py files
3160
plugins/aws-core/skills/aws-secrets-manager/references/asm-exec: #!/usr/bin/env python3
== R12 local Python launch surfaces: uvx MCP config files; hook launchers; mise/CI references
plugins/aws-core/.mcp.json
plugins/aws-core/mcp.json
plugins/aws-data-analytics/.mcp.json
plugins/aws-data-analytics/mcp.json
plugins/aws-startup-advisor/.mcp.json
plugins/aws-startup-advisor/mcp.json
plugins/aws-core/com.anthropic.claude-code/hooks/hooks.json:2
plugins/aws-startup-advisor/.cursor-plugin/hooks.json:4
plugins/aws-startup-advisor/com.anthropic.claude-code/hooks/hooks.json:5
13
.github/workflows/build.yml:3
.github/workflows/codeql.yml:1
.github/workflows/startup-advisor.yml:1
12
README.md:3
22
plugins/aws-core/skills/aws-iam/references/aws-iam-policy-generation.md
skills/core-skills/aws-iam/references/aws-iam-policy-generation.md
== R13 contract-surface signals over the 271 canonical blobs (static text search; not parsed, not executed)
271
boto3/botocore=69 argparse|click|typer=62 __main__=110 json-dump=124 sys.exit=103
-- selected non-stdlib import lines: module, canonical files, slices
anthropic: startup-diagram(1)
aws_bedrock_token_generator: bedrock-probes(1) bedrock-report(1) startup-diagram(1)
aws_cdk: docgraph(1)
aws_lambda_powertools: serverless(1)
bedrock_agentcore: payments-policy(4)
certifi: payments-policy(1)
gremlin_python: docgraph(2)
httpx: bedrock-report(1) payments-policy(3)
jmespath: bedrock-resolve(1)
jsonschema: bedrock-report(1) spec(1) startup-diagram(2) startup-models(3) startup-scoring(2)
matplotlib: ai-payloads(5)
mlflow: ai-payloads(6)
openai: bedrock-probes(1) bedrock-report(1) bedrock-resolve(1) startup-diagram(1)
oracledb: relational(1)
pandas: ai-payloads(1)
pydantic: ai-data(1)
pymongo: docgraph(1)
sagemaker: ai-data(1) ai-payloads(23)
valkey: cache-analysis(2) cache-ops(1)
yaml: spec(1)
== R14 canonical rows whose basename is mentioned by no tracked non-.py file (outside docs/migration), by slice
referenced: 179 unreferenced: 92
startup-contracts=32 cw-domain=13 cw-api=9 bedrock-report=4 bedrock-resolve=4 startup-models=4 telemetry=4 heroku=3 bedrock-probes=2 docgraph=2 payments-bridge=2 startup-diagram=2 startup-fixtures=2 startup-validators=2 cache-analysis=1 manifests=1 payments-policy=1 relational=1 startup-scoring=1 startup-tools=1 sync=1 
== R15 sizing: top 8 slices by canonical lines (lines, canonical rows, slice); split candidates
10219	37	startup-contracts
7878	11	startup-validators
4976	6	aurora
4962	7	startup-models
4548	17	telemetry
4546	9	payments-policy
4534	16	startup-fixtures
4452	3	dynamo-bench
-- startup-contracts by file-name theme: theme, rows, lines
ai-gpt6	14	4538
azure	4	742
gcp	2	114
heroku	3	700
other	14	4125
-- telemetry canonical rows by area: area, rows, lines
consent	6	551
metric_emission	4	1513
test	5	2343
top-level	2	141
-- ai-payloads canonical rows by reference directory
dataset-transformation	1
finetuning	8
model-deployment	8
model-evaluation	4
processing-jobs	4
training-jobs	3
```

## 4. Measured totals

| Measurement | Value | Command |
| --- | --- | --- |
| Tracked `*.py` paths at SRC | 391 | R2 |
| Inventory rows (`.files[]`) | 391 (0 missing, 0 extra) | R1, R2 |
| `*.py` paths that differ between baseline and SRC | 0 | R2b |
| Rows joined to a committed blob; `original_blob` mismatches | 391; 0 | R3 |
| Distinct blobs (= canonical rows) | 271 | R3 |
| Distinct `canonical_identical_path` values; resolved to a tracked blob; blob mismatches | 271; 271; 0 | R4 |
| Rows whose `canonical_identical_path` is not themselves (mirror rows) | 120 | R4 |
| Raw lines over all 391 paths | 127,558 | R6 |
| Lines after exact-blob deduplication | 97,599 | R6 |
| `toolkit-helper` rows: canonical / mirror / total | 160 / 93 / 253 | R6 |
| `test-or-diagnostic` rows: canonical / mirror / total | 86 / 2 / 88 | R6 |
| `external-service-payload` rows: canonical / mirror / total | 25 / 25 / 50 | R6 |
| `toolkit-helper` lines raw / dedup | 90,769 / 65,030 | R6 |
| `test-or-diagnostic` lines raw / dedup | 29,781 / 29,065 | R6 |
| `external-service-payload` lines raw / dedup | 7,008 / 3,504 | R6 |

"Lines" is the newline count of the committed blob (`wc -l`); a final line without a
trailing newline is not counted. Arithmetic reconciliation: 90,769 + 29,781 + 7,008 =
127,558 and 65,030 + 29,065 + 3,504 = 97,599; 160 + 86 + 25 = 271 canonical rows and 93 + 2
+ 25 = 120 mirror rows, so 271 + 120 = 391 rows. The manifest's own metadata is therefore
internally consistent at SRC and `docs/migration/python-inventory.json` needed no row
correction. These figures equal the ones in `docs/migration/README.md` for paths, blobs and
both line totals; its test-line figure does not reconcile (F-5).

## 5. Duplicate structure

Exact-blob partition (R5, R7): 271 blob groups over 391 rows. 84 groups hold two or more
rows (75 groups of size 2 and 9 groups of size 6) and cover 204 rows; 187 groups are
singletons. 120 rows are byte-identical copies of a row recorded as canonical, and in every
case the copy's canonical target is itself a tracked row with the same blob (0 exceptions,
R4). Each multi-row group has exactly one canonical row. The measured partition resolves
into eight families; the mirror column sums to 120 and the group column to 84:

| Family | Canonical root | Groups | Rows | Mirrors | Owning slices (rows in multi-row groups) |
| --- | --- | --- | --- | --- | --- |
| telemetry bundle copies | `plugins/aws-startup-advisor/scripts/telemetry/` | 9 | 54 | 45 | telemetry #4398 (54) |
| AI/ML skills | `skills/core-skills/aws-ai-ml/` | 37 | 74 | 37 | ai-data #4385 (6), ai-models #4384 (12), ai-payloads #4386 (56) |
| CloudWatch references | `skills/core-skills/aws-observability/scripts/` | 26 | 52 | 26 | cw-api #4381 (26), cw-domain #4380 (26) |
| Launch with AWS | `skills/core-skills/launch-with-aws/scripts/` | 6 | 12 | 6 | launch #4382 (12) |
| startup-advisor script pair | `plugins/aws-startup-advisor/scripts/` | 1 | 2 | 1 | startup-validators #4399 (1), heroku #4406 (1) |
| MSK scripts | `skills/specialized-skills/analytics-skills/{migrate-to-msk,managing-amazon-msk}/scripts/` | 3 | 6 | 3 | msk #4387 (6) |
| Lambda powertools example | `skills/core-skills/aws-serverless/assets/` | 1 | 2 | 1 | serverless #4397 (2) |
| Bedrock agent discovery | `skills/core-skills/amazon-bedrock/scripts/` | 1 | 2 | 1 | bedrock-fetch #4383 (2) |
| — | **total** | **84** | **204** | **120** | — |

Notes that constrain later slices:

- Telemetry: the `telemetry` slice has 62 rows (R8). 17 are canonical files under
  `plugins/aws-startup-advisor/scripts/telemetry/`; 9 of those 17 are copied into five
  bundle locations (the `references/vendored/` directories of the `azure-to-aws`,
  `gcp-to-aws`, `heroku-to-aws` and `llm-to-bedrock` skills, and `skills/shared/`), giving 9
  groups of 6, 54 rows and 45 mirrors; the other 8 canonical files are single-copy. Byte
  parity is a fact at SRC and is evidence for #4398 and #4377, not a requirement to keep any
  file layout (§8.5).
- AI/ML: 74 rows form 37 pairs. The `ai-payloads` slice holds 28 of them (56 rows): 25 payload
  pairs plus 3 helper pairs. Per side, the 25 payload files sit under `code_templates/` (23) and
  `finetuning/templates/` (2). No tracked path contains `code_templates/templates` (C15).
- One pair crosses slice boundaries: `emit-plan-json.py` in `heroku` #4406 is byte-identical
  to a canonical row owned by `startup-validators` #4399 (F-7). Both slices must agree on the
  replacement before either lands.
- Canonical *direction* is the manifest's choice and differs by family: `skills/` is canonical for
  AI/ML, CloudWatch, Launch, MSK, serverless and Bedrock; `plugins/aws-startup-advisor/` is
  canonical for telemetry and the script pair. This audit verified byte identity, not which
  side is authored; that question is U-4.

## 6. Slice ownership

36 slices hold rows; every one of the 391 rows carries a slice key and a slice Issue, and no
row is unassigned. Class columns are T = `toolkit-helper`, D = `test-or-diagnostic`, P =
`external-service-payload`. Slice scope text and dependencies live in
`docs/migration/issues.json` and are not repeated here. The table is regenerated and diffed
by R8.

| Slice | Issue | Rows | T | D | P |
| --- | --- | --- | --- | --- | --- |
| ai-data | #4385 | 6 | 6 | 0 | 0 |
| ai-models | #4384 | 12 | 12 | 0 | 0 |
| ai-payloads | #4386 | 56 | 6 | 0 | 50 |
| aurora | #4388 | 6 | 6 | 0 | 0 |
| bedrock-fetch | #4383 | 2 | 2 | 0 | 0 |
| bedrock-probes | #4404 | 6 | 3 | 3 | 0 |
| bedrock-report | #4405 | 6 | 2 | 4 | 0 |
| bedrock-resolve | #4403 | 8 | 4 | 4 | 0 |
| cache-analysis | #4392 | 9 | 9 | 0 | 0 |
| cache-ops | #4393 | 3 | 2 | 1 | 0 |
| cw-api | #4381 | 26 | 24 | 2 | 0 |
| cw-domain | #4380 | 26 | 26 | 0 | 0 |
| docgraph | #4395 | 6 | 4 | 2 | 0 |
| dynamo-bench | #4390 | 3 | 3 | 0 | 0 |
| dynamo-cost | #4389 | 2 | 2 | 0 | 0 |
| dynamo-deploy | #4391 | 2 | 2 | 0 | 0 |
| heroku | #4406 | 5 | 2 | 3 | 0 |
| launch | #4382 | 12 | 12 | 0 | 0 |
| manifests | #4375 | 2 | 1 | 1 | 0 |
| marketplace | #4396 | 11 | 10 | 1 | 0 |
| msk | #4387 | 6 | 4 | 2 | 0 |
| payments-bridge | #4411 | 3 | 1 | 2 | 0 |
| payments-policy | #4410 | 9 | 6 | 3 | 0 |
| relational | #4394 | 6 | 5 | 1 | 0 |
| security-hooks | #4412 | 1 | 1 | 0 | 0 |
| serverless | #4397 | 4 | 4 | 0 | 0 |
| spec | #4376 | 1 | 1 | 0 | 0 |
| startup-contracts | #4409 | 37 | 0 | 37 | 0 |
| startup-diagram | #4402 | 4 | 2 | 2 | 0 |
| startup-fixtures | #4408 | 16 | 16 | 0 | 0 |
| startup-models | #4401 | 7 | 3 | 4 | 0 |
| startup-scoring | #4400 | 9 | 2 | 7 | 0 |
| startup-tools | #4407 | 3 | 2 | 1 | 0 |
| startup-validators | #4399 | 11 | 9 | 2 | 0 |
| sync | #4377 | 3 | 2 | 1 | 0 |
| telemetry | #4398 | 62 | 57 | 5 | 0 |

Slice-size facts relevant to AC4 (§9, §12): `telemetry` #4398 holds 62 rows, `ai-payloads`
#4386 56, `startup-contracts` #4409 37, `cw-domain` #4380 and `cw-api` #4381 26 each,
`startup-fixtures` #4408 16. Those six slices hold 223 of 391 rows (57%).

## 7. Final per-row dispositions

Every one of the 391 rows carries exactly one disposition. Nothing is excluded from the
programme, and no row is left without an owner. The table is regenerated from the manifest
and diffed against this document by R9, so each of the 391 rows below was checked once
against its manifest row (slice, class, role, canonical path). Codes:

- `PORT-RUST` — canonical row; the owning slice must deliver Rust behaviour for it.
- `PORT-RUST-VIA-CANONICAL` — byte-identical mirror; the owning slice ports the canonical
  row and re-synchronises this copy, or removes it if the replacement makes the copy
  unnecessary.
- `PAYLOAD-COMPAT-Q` — canonical managed-service payload; disposition is an explicit
  compatibility question, not an exclusion. #4386 AC1/AC3 must record the language contract
  and the disposition per file before the row can be retired.
- `PAYLOAD-COMPAT-Q-MIRROR` — mirror of a payload row; follows its canonical decision.

Counts (R9): 246 `PORT-RUST`, 95 `PORT-RUST-VIA-CANONICAL`, 25 `PAYLOAD-COMPAT-Q`, 25
`PAYLOAD-COMPAT-Q-MIRROR` (391 total; 341 port rows, 50 payload rows). Class column: T =
`toolkit-helper`, D = `test-or-diagnostic`, P = `external-service-payload`; role column:
`canonical` or `mirror` (`path` equal or not equal to `canonical_identical_path`).

| Path | Slice | Class | Role | Disposition |
| --- | --- | --- | --- | --- |
| `plugins/aws-agents/skills/agents-build/scripts/process_payment_tool.py` | payments-policy | T | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-build/scripts/setup_payment_user.py` | payments-policy | T | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/packages/openclaw/runtime/agentcore_bridge.py` | payments-bridge | T | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/packages/openclaw/test/test_agentcore_bridge.py` | payments-bridge | D | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/packages/openclaw/test/test_setup_openclaw.py` | payments-bridge | D | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/scripts/agents_pay_admin.py` | payments-policy | T | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/scripts/test_portability.py` | payments-policy | D | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/scripts/test_resource_resolution.py` | payments-policy | D | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/scripts/test_x402_policy.py` | payments-policy | D | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/scripts/x402_fetch.py` | payments-policy | T | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/scripts/x402_fetch_cli.py` | payments-policy | T | canonical | PORT-RUST |
| `plugins/aws-agents/skills/agents-pay/scripts/x402_policy.py` | payments-policy | T | canonical | PORT-RUST |
| `plugins/aws-core/com.anthropic.claude-code/hooks/secret-safety.py` | security-hooks | T | canonical | PORT-RUST |
| `plugins/aws-core/skills/amazon-bedrock/scripts/fetch_bedrock_agent.py` | bedrock-fetch | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/dataset-evaluation/scripts/format_detector.py` | ai-data | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/dataset-transformation/code_templates/transformation.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/dataset-transformation/scripts/transformation_tools.py` | ai-data | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/endpoint-diagnostics/scripts/collect_diagnostics.py` | ai-models | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning-technique/scripts/get_recipes.py` | ai-models | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/code_templates/dpo.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/code_templates/rlaif_builtin.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/code_templates/rlaif_custom_prompt.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/code_templates/rlvr.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/code_templates/sft.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/scripts/mlflow_reference.py` | ai-payloads | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/templates/nova_rlvr_reward_function_source_template.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/finetuning/templates/rlvr_reward_function_source_template.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/deploy-jumpstart-sagemaker.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/deploy-nova-bedrock.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/deploy-nova-sagemaker.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/deploy-oss-bedrock.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/deploy-oss-sagemaker.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/optimize-benchmark.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/optimize-huggingface-to-s3.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-deployment/code_templates/optimize-recommendation.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-evaluation/code_templates/custom_scorer_evaluator.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-evaluation/code_templates/llmaaj_evaluator.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/model-evaluation/scripts/nova_reward_function_source_template.py` | ai-payloads | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/model-evaluation/scripts/reward_function_source_template.py` | ai-payloads | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/model-evaluation/scripts/validate_custom_metrics.py` | ai-data | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/model-selection/scripts/filter_deployable_models.py` | ai-models | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/model-selection/scripts/get_deployable_models.py` | ai-models | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/model-selection/scripts/get_model_names.py` | ai-models | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/model-selection/scripts/search_deployable_models.py` | ai-models | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-ai-ml/references/processing-jobs/code_templates/dataset_definition_processing.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/processing-jobs/code_templates/framework_processing.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/processing-jobs/code_templates/script_processing.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/processing-jobs/code_templates/spark_processing.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/training-jobs/code_templates/custom_training.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/training-jobs/code_templates/jumpstart_training.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-ai-ml/references/training-jobs/code_templates/recipe_training.py` | ai-payloads | P | mirror | PAYLOAD-COMPAT-Q-MIRROR |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch-omni/capture_dataset_from_traces.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch-omni/evaluate_traces.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch-omni/test_evaluate_traces.py` | cw-api | D | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_app_signals_client.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_capture.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_constants.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_crud_rendering.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_crud_tools.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_error_translation.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_formatting.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_gateway.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_instrumentation.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_location.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_logs_client.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_region.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_result.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_session.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_snapshot_parsing.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_snapshot_queries.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_snapshot_rendering.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_snapshot_tools.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_snapshots.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_status_assessment.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_status_rendering.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_status_tools.py` | cw-api | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-observability/scripts/cloudwatch/di_validation.py` | cw-domain | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/aws-serverless/assets/powertools-handler.py` | serverless | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/launch-with-aws/scripts/archive.py` | launch | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/launch-with-aws/scripts/auth.py` | launch | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/launch-with-aws/scripts/auth_callback_server.py` | launch | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/launch-with-aws/scripts/launch_api_client.py` | launch | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/launch-with-aws/scripts/launch_config.py` | launch | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-core/skills/launch-with-aws/scripts/launch_with_aws.py` | launch | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-data-analytics/skills/managing-amazon-msk/scripts/msk_sizing.py` | msk | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-data-analytics/skills/migrate-to-msk/scripts/compatibility.py` | msk | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-data-analytics/skills/migrate-to-msk/scripts/simulation_load_test_config.py` | msk | D | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/fixtures/azure-iac-terraform/check_expected_clarify.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/azure-iac-terraform/check_expected_clarify_complete.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/azure-iac-terraform/check_expected_clarify_fast_path.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/azure-iac-terraform/check_expected_design.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/azure-iac-terraform/check_expected_estimate.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/azure-iac-terraform/check_expected_iac_terraform.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/gcp-decision-gate/check_expected_decide.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/gcp-live-capture/check_expected_baseline.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/gcp-live-capture/check_expected_drift.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/gcp-workshop/check_expected_workshop.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/heroku-decision-gate/check_expected_decide.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/heroku-decision-gate/check_expected_decide_with_retained_pack.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/heroku-live-capture/check_expected_drift.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/heroku-live-capture/check_expected_estimate.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/heroku-nonweb-scaling/check_expected_nonweb_scaling.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/fixtures/heroku-workshop/check_expected_workshop.py` | startup-fixtures | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/check-bedrock-pricing-parity.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/consent-disclosure-audit.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/emit-plan-json.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/model-id-lint.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/cli.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/consent/accept.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/consent/cli.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/consent/notice.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/consent/opt_out.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/consent/record.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/consent/session_start.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/metric_emission/client.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/metric_emission/migration.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/metric_emission/migration_attributes.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/metric_emission/skill_invoked.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/sync_bundles.py` | telemetry | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/test/conftest.py` | telemetry | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/test/test_cli.py` | telemetry | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/test/test_consent.py` | telemetry | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/test/test_metric_emission.py` | telemetry | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/telemetry/test/test_migration.py` | telemetry | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/test_validate_migration_report_decision_core.py` | startup-validators | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/validate-artifacts.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/validate-migration-report.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/validate-model-pricing.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/scripts/validate-startup-program-artifacts.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/anthropic_model_recommendation.py` | startup-models | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/build_diagram.py` | startup-diagram | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/model_recommendation.py` | startup-models | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/openai_model_recommendation.py` | startup-models | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/score_units.py` | startup-scoring | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/scoring.py` | startup-scoring | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_build_diagram.py` | startup-diagram | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_collapse_invariant.py` | startup-scoring | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_cost_levers.py` | startup-scoring | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_model_recommendation.py` | startup-models | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_openai_model_recommendation.py` | startup-models | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_opus55_generation_contracts.py` | startup-models | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_opus55_integration.py` | startup-models | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_poc_shapes.py` | startup-scoring | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_scoring.py` | startup-scoring | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_temporal_decision_refs.py` | startup-scoring | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_unit_grouping.py` | startup-scoring | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_verify_model_path.py` | startup-diagram | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/test_workload_classes.py` | startup-scoring | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/agent-advisor/scripts/verify_model_path.py` | startup-diagram | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/consent/accept.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/consent/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/consent/notice.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/consent/opt_out.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/consent/record.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/metric_emission/client.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/metric_emission/migration.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/azure-to-aws/references/vendored/telemetry/metric_emission/migration_attributes.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/consent/accept.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/consent/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/consent/notice.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/consent/opt_out.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/consent/record.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/metric_emission/client.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/metric_emission/migration.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/gcp-to-aws/references/vendored/telemetry/metric_emission/migration_attributes.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/consent/accept.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/consent/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/consent/notice.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/consent/opt_out.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/consent/record.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/metric_emission/client.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/metric_emission/migration.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/references/vendored/telemetry/metric_emission/migration_attributes.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/scripts/emit-plan-json.py` | heroku | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/scripts/test_validate_heroku_migration_report.py` | heroku | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/scripts/test_validate_heroku_migration_report_cost.py` | heroku | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/scripts/test_validate_heroku_migration_report_currency.py` | heroku | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/heroku-to-aws/scripts/validate-heroku-migration-report.py` | heroku | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/consent/accept.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/consent/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/consent/notice.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/consent/opt_out.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/consent/record.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/metric_emission/client.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/metric_emission/migration.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/references/vendored/telemetry/metric_emission/migration_attributes.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/bedrock_pricing.py` | bedrock-resolve | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/iam_policy.py` | bedrock-probes | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/image_input.py` | bedrock-resolve | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/model_identity.py` | bedrock-resolve | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/preflight_bedrock.py` | bedrock-probes | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/render_report.py` | bedrock-report | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/resolve_source_model.py` | bedrock-resolve | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/source_baseline.py` | bedrock-probes | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_astra_execution_contract.py` | bedrock-report | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_bedrock_pricing.py` | bedrock-resolve | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_delta_reference_schema.py` | bedrock-report | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_iam_policy.py` | bedrock-probes | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_image_input.py` | bedrock-resolve | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_model_identity.py` | bedrock-resolve | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_preflight_bedrock.py` | bedrock-probes | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_render_report.py` | bedrock-report | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_resolve_source_model.py` | bedrock-resolve | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_source_baseline.py` | bedrock-probes | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/test_validate_result.py` | bedrock-report | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/validate_result.py` | bedrock-report | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/shared/telemetry/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/consent/accept.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/consent/cli.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/consent/notice.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/consent/opt_out.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/consent/record.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/metric_emission/client.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/metric_emission/migration.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/shared/telemetry/metric_emission/migration_attributes.py` | telemetry | T | mirror | PORT-RUST-VIA-CANONICAL |
| `plugins/aws-startup-advisor/skills/tf-best-practices/scripts/test_validate_terraform_policy.py` | startup-validators | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/skills/tf-best-practices/scripts/validate-terraform-policy.py` | startup-validators | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_ai_capability_catalog_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_ai_migration_matrix_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_ai_only_option_c_framework_route.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_ai_workload_profile_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_aws_design_ai_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_azure_clarify_fast_path_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_azure_discover_ordering.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_azure_discovery_source_guidance.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_azure_rdfa_discover_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_consent_disclosure_audit.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_cost_completeness.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_credential_lifecycle.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_decision_gate_wiring.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_emit_plan_json.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_estimate_assemble_stale_artifacts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_estimate_clarify_volume_resolution.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_estimate_option_a_single_close.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_estimation_ai_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_gcp_design_guide_selection.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_gemini_guide_body_parity.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_generate_mantle_dispatch.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_generation_ai_contracts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_gpt6_review_round2.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_gpt6_review_round5.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_gpt6_round4_findings.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_gpt6_source_breakpoint.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_heroku_discover_addon_identity.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_heroku_estimate_deferred_cost_comparison.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_heroku_postgres_advanced_pricing.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_llm_to_bedrock_run_state.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_model_id_lint.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_openrouter_byok_detection_fixes.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_report_generator_capture_warnings_gate.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_report_generator_cost_pricing.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_resume_and_sentinel_consistency.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_usage_api_request_params.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tests/test_validate_artifacts.py` | startup-contracts | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tools/check-skill-refs.py` | startup-tools | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tools/model-staleness.py` | startup-tools | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tools/sync-vendored.py` | sync | T | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tools/test_check_skill_refs.py` | startup-tools | D | canonical | PORT-RUST |
| `plugins/aws-startup-advisor/tools/test_sync_vendored.py` | sync | D | canonical | PORT-RUST |
| `skills/core-skills/amazon-bedrock/scripts/fetch_bedrock_agent.py` | bedrock-fetch | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/dataset-evaluation/scripts/format_detector.py` | ai-data | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/dataset-transformation/code_templates/transformation.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/dataset-transformation/scripts/transformation_tools.py` | ai-data | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/endpoint-diagnostics/scripts/collect_diagnostics.py` | ai-models | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/finetuning-technique/scripts/get_recipes.py` | ai-models | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/finetuning/code_templates/dpo.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/finetuning/code_templates/rlaif_builtin.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/finetuning/code_templates/rlaif_custom_prompt.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/finetuning/code_templates/rlvr.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/finetuning/code_templates/sft.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/finetuning/scripts/mlflow_reference.py` | ai-payloads | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/finetuning/templates/nova_rlvr_reward_function_source_template.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/finetuning/templates/rlvr_reward_function_source_template.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/deploy-jumpstart-sagemaker.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/deploy-nova-bedrock.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/deploy-nova-sagemaker.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/deploy-oss-bedrock.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/deploy-oss-sagemaker.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/optimize-benchmark.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/optimize-huggingface-to-s3.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-deployment/code_templates/optimize-recommendation.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-evaluation/code_templates/custom_scorer_evaluator.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-evaluation/code_templates/llmaaj_evaluator.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/model-evaluation/scripts/nova_reward_function_source_template.py` | ai-payloads | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/model-evaluation/scripts/reward_function_source_template.py` | ai-payloads | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/model-evaluation/scripts/validate_custom_metrics.py` | ai-data | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/model-selection/scripts/filter_deployable_models.py` | ai-models | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/model-selection/scripts/get_deployable_models.py` | ai-models | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/model-selection/scripts/get_model_names.py` | ai-models | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/model-selection/scripts/search_deployable_models.py` | ai-models | T | canonical | PORT-RUST |
| `skills/core-skills/aws-ai-ml/references/processing-jobs/code_templates/dataset_definition_processing.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/processing-jobs/code_templates/framework_processing.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/processing-jobs/code_templates/script_processing.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/processing-jobs/code_templates/spark_processing.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/training-jobs/code_templates/custom_training.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/training-jobs/code_templates/jumpstart_training.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-ai-ml/references/training-jobs/code_templates/recipe_training.py` | ai-payloads | P | canonical | PAYLOAD-COMPAT-Q |
| `skills/core-skills/aws-observability/scripts/cloudwatch-omni/capture_dataset_from_traces.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch-omni/evaluate_traces.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch-omni/test_evaluate_traces.py` | cw-api | D | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_app_signals_client.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_capture.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_constants.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_crud_rendering.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_crud_tools.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_error_translation.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_formatting.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_gateway.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_instrumentation.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_location.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_logs_client.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_region.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_result.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_session.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_snapshot_parsing.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_snapshot_queries.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_snapshot_rendering.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_snapshot_tools.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_snapshots.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_status_assessment.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_status_rendering.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_status_tools.py` | cw-api | T | canonical | PORT-RUST |
| `skills/core-skills/aws-observability/scripts/cloudwatch/di_validation.py` | cw-domain | T | canonical | PORT-RUST |
| `skills/core-skills/aws-serverless/assets/powertools-handler.py` | serverless | T | canonical | PORT-RUST |
| `skills/core-skills/launch-with-aws/scripts/archive.py` | launch | T | canonical | PORT-RUST |
| `skills/core-skills/launch-with-aws/scripts/auth.py` | launch | T | canonical | PORT-RUST |
| `skills/core-skills/launch-with-aws/scripts/auth_callback_server.py` | launch | T | canonical | PORT-RUST |
| `skills/core-skills/launch-with-aws/scripts/launch_api_client.py` | launch | T | canonical | PORT-RUST |
| `skills/core-skills/launch-with-aws/scripts/launch_config.py` | launch | T | canonical | PORT-RUST |
| `skills/core-skills/launch-with-aws/scripts/launch_with_aws.py` | launch | T | canonical | PORT-RUST |
| `skills/specialized-skills/analytics-skills/managing-amazon-msk/scripts/msk_sizing.py` | msk | T | canonical | PORT-RUST |
| `skills/specialized-skills/analytics-skills/migrate-to-msk/scripts/compatibility.py` | msk | T | canonical | PORT-RUST |
| `skills/specialized-skills/analytics-skills/migrate-to-msk/scripts/simulation_load_test_config.py` | msk | D | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/aggregator.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/cleanup.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/deprovision_cleanup.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/discoverer.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/expiry.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/metering_core.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/query_metering.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/register.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/submitter.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/subscription.py` | marketplace | T | canonical | PORT-RUST |
| `skills/specialized-skills/aws-marketplace-skills/aws-marketplace-metering/scripts/test_event_publisher.py` | marketplace | D | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-aurora-mysql/scripts/acu_calculator.py` | aurora | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-aurora-mysql/scripts/commitment_pricing_analyzer.py` | aurora | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-aurora-mysql/scripts/io_optimized_analyzer.py` | aurora | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-aurora-postgresql/scripts/acu_calculator.py` | aurora | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-aurora-postgresql/scripts/commitment_pricing_analyzer.py` | aurora | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-aurora-postgresql/scripts/io_optimized_analyzer.py` | aurora | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-documentdb/scripts/wa_review.py` | docgraph | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-dynamodb/scripts/benchmark_lambda.py` | dynamo-bench | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-dynamodb/scripts/benchmark_model.py` | dynamo-bench | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-dynamodb/scripts/calculate_costs.py` | dynamo-cost | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-dynamodb/scripts/deploy_model.py` | dynamo-deploy | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-dynamodb/scripts/generate_perf_report.py` | dynamo-bench | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-dynamodb/scripts/generate_teardown.py` | dynamo-deploy | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-dynamodb/scripts/iterate_design.py` | dynamo-cost | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/command_classifier.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/find_tunnel_host.py` | cache-ops | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/generate_dashboards.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/input_validator.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/migration_preflight.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/price_calculator.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/pricing.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/security_audit.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/serverless_estimator.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/start_tunnel.py` | cache-ops | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/test_connection.py` | cache-ops | D | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-elasticache/scripts/validate_references.py` | cache-analysis | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-neptune/scripts/agentic_memory.py` | docgraph | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-neptune/scripts/cdk_test_env.py` | docgraph | D | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-neptune/scripts/graphrag_pipeline.py` | docgraph | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-neptune/scripts/input_validator.py` | docgraph | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/amazon-neptune/scripts/seed_test_data.py` | docgraph | D | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/aurora-dsql/scripts/safe_query.py` | relational | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/aurora-dsql/scripts/tenant_query.py` | relational | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/rds-oracle/scripts/test_oracle_connection.py` | relational | D | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/rds-oss/scripts/rds_commitment_pricing_analyzer.py` | relational | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/timestream-influxdb/scripts/input_validator.py` | relational | T | canonical | PORT-RUST |
| `skills/specialized-skills/database-skills/timestream-influxdb/scripts/instance_types.py` | relational | T | canonical | PORT-RUST |
| `skills/specialized-skills/serverless-skills/processing-s3-uploads-with-step-functions/scripts/fargate_processor.py` | serverless | T | canonical | PORT-RUST |
| `skills/specialized-skills/serverless-skills/processing-s3-uploads-with-step-functions/scripts/lambda_function.py` | serverless | T | canonical | PORT-RUST |
| `tools/sync-plugin-skills.py` | sync | T | canonical | PORT-RUST |
| `tools/tests/test_validate.py` | manifests | D | canonical | PORT-RUST |
| `tools/validate.py` | manifests | T | canonical | PORT-RUST |
| `tools/validate_spec.py` | spec | T | canonical | PORT-RUST |

## 8. Assigned contracts and unresolved compatibility questions

These are the local Python dependencies and compatibility contracts the audit found. Each is
owned by a slice Issue; this audit records evidence and does not decide the port.

1. **Managed-service payloads — #4386 (56 rows in the slice).** 50 `external-service-payload`
   rows (25 canonical + 25 mirror) are SageMaker/Bedrock customer-side training, evaluation,
   deployment and processing templates. Per side they sit in `finetuning/code_templates` (5),
   `finetuning/templates` (2), `model-deployment/code_templates` (8),
   `model-evaluation/code_templates` (2), `processing-jobs/code_templates` (4),
   `training-jobs/code_templates` (3) and `dataset-transformation/code_templates` (1) under
   `aws-ai-ml/references/`. They are product data whose language contract is a
   managed-service requirement, not a repository implementation detail. #4386 AC3 requires an
   explicit documented disposition per payload and forbids silently counting payload text as
   full Python removal. This audit therefore records them as unresolved compatibility
   questions (`PAYLOAD-COMPAT-Q`), keeps them in scope, and asserts no language contract for
   them (U-1).
2. **`uvx` MCP proxy — #4413.** Six tracked files launch the MCP server with
   `"command": "uvx"`: `plugins/{aws-core,aws-data-analytics,aws-startup-advisor}/.mcp.json`
   and the matching `mcp.json` (R12). That is a local Python runtime dependency on each
   plugin's launch path and is explicitly in #4413 scope (AC5: no `uvx` runtime dependency).
   It cannot be classified out of scope by this audit. R12 also finds `uvx` in 12 prose files
   under `plugins/` and `skills/` (installation and connection instructions, for example
   `setup-devops-agent`, startup-advisor `connecting.md`) and in the root `README.md`, whose
   client examples and "You need uv installed" prerequisites tell users to install the
   Python tool runner. The `mcp-proxy-for-aws-cli` launch references fall under #4413's
   stated scope; whether each prose file is part of its change is for #4413's AC1.
3. **A second `uvx` tool — no owning slice (F-9).** The aws-iam skill instructs agents to
   run `uvx iam-policy-autopilot@latest generate-policies` as the mandatory policy path (22
   occurrences in `plugins/aws-core/skills/aws-iam/references/aws-iam-policy-generation.md`
   and its `skills/core-skills/` mirror, including "You MUST verify `uvx` is available").
   #4413's stated scope is `plugins/*/.mcp.json`, `plugins/*/mcp.json` and
   `mcp-proxy-for-aws-cli` references, which does not name this tool. It is the same
   dependency posture; the manager decides whether it joins #4413 or gets its own slice
   (§12).
4. **Secret-safety hook — #4412 (safety, not preference).** Both PreToolUse registrations in
   `plugins/aws-core/com.anthropic.claude-code/hooks/hooks.json` launch `secret-safety.py`
   through `sh -c 'python3 "$0" 2>/dev/null || python "$0" 2>/dev/null || py -3 "$0"'`
   (R12: 2 launcher lines). The chain has no deny fallback. With no interpreter, `sh` ends
   in exit 127 from the last command; the guard did not run and nothing replaces it. The deny protocol itself is JSON on stdout with exit 0, so
   it works when an interpreter exists; a script crash is retried with the next interpreter
   and ends non-zero, not as a deny. Whether the host treats exit 127 as non-blocking is host behaviour
   and was not verified (U-7); the point is that a missing runtime removes the control
   without a deny. #4412 AC4 requires a Rust launcher *without removing protection*, and AC1
   requires preserving the deny/allow protocol and secret-fetch detection, so the missing-
   binary behaviour is a defect to resolve there by engineering, not a routine operator
   choice (F-10).
5. **Telemetry bundles — #4398 / #4377 (parity evidence, not layout lock).** 17 canonical
   telemetry files exist, 9 of them in 5 further byte-identical bundle copies (62 rows, §5).
   Telemetry is launched by `plugins/aws-startup-advisor/com.anthropic.claude-code/hooks/hooks.json`
   (5 launcher lines, chain ending `… || py -3 "$0" 2>/dev/null || true`) and
   `plugins/aws-startup-advisor/.cursor-plugin/hooks.json` (4 lines, chain ending
   `… || python … || true`), so telemetry failure is non-fatal by construction (F-11).
   `scripts/telemetry/README.md` states that unavailable Python skips telemetry. That is
   evidence #4398 AC1 (consent and opt-out stay fail-closed) must reconcile. Byte parity is
   evidence for the sync contract (#4377); it does not oblige the Rust implementation to keep
   a nine-file Python surface. The Rust replacement may change bundle layout, callers and
   `version.json` sidecars atomically, provided #4398 AC5 (bundle sync verified) holds;
   callers (the two hooks files) and bundle changes land together, not separately.
6. **CI and developer entry points require a Python toolchain (AC5).** Root `mise.toml` pins
   `python = "3.12"` and runs `python3 tools/validate.py`,
   `python3 tools/sync-plugin-skills.py --check`,
   `uv run --with jsonschema --with pyyaml python tools/validate_spec.py` and
   `python3 -m unittest discover -s tools/tests -v`; the `validate` task depends on all four.
   `plugins/aws-startup-advisor/mise.toml` runs eight Python tasks
   (`scripts/validate-artifacts.py`, `tools/sync-vendored.py`, `tools/check-skill-refs.py`,
   `scripts/consent-disclosure-audit.py`, `tools/model-staleness.py`,
   `scripts/model-id-lint.py`, `scripts/validate-model-pricing.py` and
   `uv run --no-project --with pytest python -m pytest tests -q`; R12: 13 Python-invoking
   lines across the two files). `.github/workflows/build.yml` runs `mise run lint:md`,
   `validate` and `security`, `startup-advisor.yml` runs `mise -C plugins/aws-startup-advisor
   run ci`, and `codeql.yml` analyses `language: python`. Every script named above is an
   inventory row (slices manifests #4375, spec #4376, sync #4377, startup-validators #4399,
   startup-tools #4407). The owning slices, with #4378 ci, must retire the tasks; this audit
   neither edits workflows nor claims they are already Python-free.
7. **Extensionless Python executable outside the `*.py` inventory (F-8).**
   `plugins/aws-core/skills/aws-secrets-manager/references/asm-exec` is a tracked file with
   shebang `#!/usr/bin/env python3` (the only one among 3,160 tracked non-`.py` files, R11).
   No slice owns it because the inventory is keyed on the `*.py` suffix. A proposed owner
   is in §12. The row is not added to the manifest by this repair because the manifest's
   path key and schema are the programme's contract.
8. **Static contract signals (AC3, R13).** Over the 271 canonical blobs, text search finds
   `boto3`/`botocore` import lines in 69 files, `argparse`/`click`/`typer` in 62,
   `if __name__ == "__main__"` in 110, `json.dump(s)` in 124 and `sys.exit`/`SystemExit` in
   103. Those are CLI-with-JSON-and-exit-code shaped, so each slice's AC1 must record the
   argument, JSON and exit-code contract of every file it ports; this audit does not copy
   them. Import lines that name a non-stdlib module with no obvious Rust-ecosystem
   equivalent are recorded as candidate Python-only SDK blockers, to be confirmed by the
   owning slice (U-8):

   | Module | Canonical files (slice) | Note |
   | --- | --- | --- |
   | `sagemaker` | 24 (ai-payloads 23, ai-data 1) | SageMaker Python SDK; mostly inside payload templates |
   | `mlflow` | 6 (ai-payloads) | reward-function and MLflow reference files |
   | `bedrock_agentcore` | 4 (payments-policy) | AgentCore Python SDK |
   | `aws_bedrock_token_generator` | 3 (bedrock-probes, bedrock-report, startup-diagram) | Bedrock bearer-token helper |
   | `aws_lambda_powertools` | 1 (serverless) | Lambda handler example (a deployed artifact) |
   | `aws_cdk` | 1 (docgraph) | CDK Python bindings |
   | `gremlin_python`, `pymongo`, `oracledb`, `valkey` | 2 (docgraph), 1 (docgraph), 1 (relational), 3 (cache-analysis 2, cache-ops 1) | database client libraries |
   | `openai`, `anthropic`, `httpx`, `jmespath`, `jsonschema`, `yaml` | openai 4, anthropic 1, httpx 4, jmespath 1, jsonschema 9, yaml 1 | HTTP/model/validation libraries; Rust crates exist for the generic ones, not verified here |
   | `matplotlib`, `pandas`, `pydantic`, `certifi` | 5 (ai-payloads), 1 (ai-payloads), 1 (ai-data), 1 (payments-policy) | data/plotting/validation libraries |

   `pytest` import lines appear in 30 canonical files, all test or diagnostic rows
   (bedrock-probes, bedrock-report, bedrock-resolve, startup-contracts, startup-diagram,
   startup-models, startup-scoring, telemetry).
9. **Existing development instructions (AC5).** `AGENTS.md`, `CLAUDE.md`, `CONTRIBUTING.md`
   and `README.md` (line 3) already state the fork policy that new executable code and tests
   are Rust and that legacy Python is migration evidence; `docs/agents/worker-contract.md`
   carries the worker rules. The Python development instructions that remain active are the
   ones listed in items 2–7 (launchers, `mise`, CI, CodeQL, `uvx` prose) plus the Python
   toolchain prerequisites in root `README.md` (item 2).

## 9. Acceptance criteria — status and limitations

These are #4371's own acceptance criteria, as stated on the Issue. Each is stated with what
was not done.

- **AC1 — Validate every file-to-slice mapping, duplicates and callers.** Met for mapping and
  duplicates: 391 inventory rows equal the 391 tracked `*.py` paths; every row has a slice
  and a slice Issue; 0 `original_blob` mismatches; 271 distinct blobs; 84 multi-row groups,
  187 singletons, 120 mirrors, 0 canonical mismatch (§4–§6, R2–R8). Callers: partly. The
  non-Python launch surfaces are enumerated (§8.2–§8.7: 6 MCP configs, 3 hook files with 11
  launcher lines, 2 `mise.toml` files, 3 workflows). By basename search (R14) 179 of 271
  canonical rows are mentioned in at least one tracked non-`.py` file and 92 are not (by
  slice: startup-contracts 32, cw-domain 13, cw-api 9, bedrock-report 4, bedrock-resolve 4,
  startup-models 4, telemetry 4, heroku 3, others 1–2 each). Those are mostly test files and
  internal modules whose caller is another Python file. Limitation: Python-to-Python import
  edges were not mapped (U-9), and a basename hit is evidence of reference, not of
  invocation (U-8). One byte-identical pair crosses two slices (F-7); the manifest has no
  disposition field beyond `status: "pending"`, so the vocabulary in §7 lives in this
  document (U-6).
- **AC2 — Separate toolkit runtime from remote service payloads.** Met at audit level: the
  manifest class separates 253 `toolkit-helper`, 88 `test-or-diagnostic` and 50
  `external-service-payload` rows, and §7 gives each row a distinct disposition. The 50
  payload rows are not excluded; they are compatibility questions (§8.1). Limitation: the
  class is a static judgement over file content, not an execution result (F-6, U-2); the six
  `ai-payloads` `toolkit-helper` rows (MLflow and reward-function helpers) sit in the same
  slice and share its payload ambiguity.
- **AC3 — Record CLI/JSON/error contracts and Python-only SDK blockers.** Partly met. §8.8
  records measured signals and the candidate SDK-blocker table. No per-file CLI argument,
  JSON schema or error-code contract was written down, because that is the owning slices'
  AC1 work and doing it for 271 files would be the broad second audit this repair excluded.
  Rust crate availability for the SDK candidates was not investigated (no network use).
- **AC4 — Split oversized slices into proposed child Issues.** Not met as filed Issues: this
  Issue may publish only to #4371, so it cannot create child Issues, and dispatch belongs to
  the manager. Measured sizing and concrete split proposals are in §6 and §12, with R15.
- **AC5 — Audit remaining active development instructions.** Met at the evidence level:
  §8.2–§8.7 and §8.9 list the remaining Python-dependent launchers, tasks, workflows and
  prose with file references. No dependency was removed or replaced and the repository is not
  Python-free; the install and development posture still assumes a Python toolchain.

Not claimed anywhere in this document: Rust verification, contract tests, job ids, review
outcome, landed SHA, or live/paid AWS behaviour. No build job was run for this docs-only
revision, and the Rust candidate-verification gate remains unsupported and is not waived.

## 10. Findings

- **F-1 Inventory completeness.** 391 rows = 391 tracked `*.py` paths; 0 missing, 0 extra,
  0 `original_blob` mismatches at `71319f9a`; no `*.py` path differs between baseline and
  SRC (R2, R2b, R3).
- **F-2 Deduplication.** 271 distinct blobs; 127,558 raw / 97,599 deduplicated lines (R3,
  R6).
- **F-3 Canonical mapping integrity.** 271 distinct canonical paths, all inventory rows, all
  tracked, all with byte-identical blobs; 0 errors (R4).
- **F-4 Duplicate partition.** 84 multi-row groups (75 of size 2, 9 of size 6) covering 204
  rows; 187 singletons; 120 mirror rows; eight families (§5, R5, R7).
- **F-5 `docs/migration/README.md` test-line claim does not reconcile.** The README states
  28,208 lines "in named tests/diagnostics". Measured at SRC: the manifest's
  `test-or-diagnostic` class is 29,781 raw / 29,065 dedup, and a basename rule (`test_*`,
  `*_test.py`, `conftest.py`) gives 84 paths / 28,630 raw / 28,314 dedup (R6, R10). Neither
  equals 28,208. The README is outside this repair's write scope; recorded as U-5 and as a
  follow-up (§12).
- **F-6 Classification divergence is small and deliberate.** 4 `test-or-diagnostic` rows
  are not name-based tests: `simulation_load_test_config.py` (canonical + mirror),
  `cdk_test_env.py`, `seed_test_data.py` (R10). 84 rows match the basename rule; the class
  holds 88.
- **F-7 Cross-slice duplicate.** `plugins/aws-startup-advisor/scripts/emit-plan-json.py`
  (startup-validators #4399, canonical) and
  `plugins/aws-startup-advisor/skills/heroku-to-aws/scripts/emit-plan-json.py` (heroku
  #4406, mirror) are byte-identical; the two slices must coordinate.
- **F-8 One Python executable is invisible to the `*.py` inventory.** `asm-exec` (§8.7, R11)
  is unowned.
- **F-9 A second `uvx` runtime dependency is unowned.** `uvx iam-policy-autopilot@latest`
  (§8.3).
- **F-10 Secret-safety hook has no deny fallback.** Both registrations end in a launcher
  with no fallback control, so a missing interpreter removes the guard (§8.4, #4412).
- **F-11 Telemetry hooks are non-fatal by construction** (`… || true`), which must be
  reconciled with #4398 AC1 fail-closed consent (§8.5).
- **F-12 Slice concentration is partly a mirror artefact.** Six slices hold 223 of 391 rows
  (57%): telemetry 62, ai-payloads 56, startup-contracts 37, cw-domain 26, cw-api 26,
  startup-fixtures 16 (§6). By canonical rows the real port surface is smaller for four of
  them: telemetry 17, ai-payloads 28, cw-domain 13, cw-api 13; startup-contracts stays 37
  and startup-fixtures 16. By canonical lines the largest slices are startup-contracts
  10,219, startup-validators 7,878, aurora 4,976, startup-models 4,962, telemetry 4,548
  (R15).
- **F-13 Many ports are CLI-shaped with SDK imports.** 62 canonical files import an
  argument parser, 69 import `boto3`/`botocore`, 124 emit JSON, 103 set exit codes (§8.8,
  R13); contract fixtures are a slice-level requirement.
- **F-14 92 canonical files have no non-Python reference by basename** (R14), concentrated
  in tests and internal modules; their callers are other Python files that were not mapped
  (U-9).

## 11. Unknowns

- **U-1 Managed-service language contracts.** Which payload rows are genuine managed-service
  text requirements is undetermined; it needs primary documentation review under #4386 AC1.
- **U-2 Payload execution reality.** Static inspection shows template-shaped files, but
  whether repository tooling or only a customer executes any payload row was not
  established. No execution was permitted or attempted.
- **U-3 Parse validity of templates and helpers.** The predecessor's `ast.parse` claims are
  withdrawn as prohibited evidence, and the later draft's IPython-magic observation (C15)
  had no supporting path. No file was parsed or run in this revision, so parse validity of
  every file is unknown and is not needed for any disposition in §7.
- **U-4 Canonical direction.** Whether `skills/` or `plugins/` is authored per family is the
  manifest's assertion; verifying it requires running the sync tool, which is Python
  (#4377 AC1 will settle it).
- **U-5 Basis of the README line-count claim** (F-5); the README is outside this repair's
  write scope.
- **U-6 No machine-readable disposition field.** The manifest records `status: "pending"`
  per row; a disposition field would be a schema change owned by the programme, so this
  audit keeps the vocabulary in this document.
- **U-7 Host semantics for a failing hook.** How each host treats exit 127 or a crashed
  hook command (blocking or non-blocking) was not tested; the finding (F-10) rests only on
  the launcher text.
- **U-8 Signal quality of R13/R14.** They are line-oriented text searches over blobs. They
  can count an import word in a docstring, miss dynamic imports, and treat a basename match
  as a reference. They indicate where to look, not what a file does.
- **U-9 Python-to-Python callers.** Import edges between inventory files were not mapped.

## 12. Proposed follow-ups (not filed by this Issue)

These are proposals for the manager. This Issue publishes only to #4371 and files no
programme Issues.

1. Reconcile `docs/migration/README.md` with measured line counts or record its counting
   rule (F-5, U-5).
2. Give `asm-exec` an owner and decide whether the inventory key widens beyond `*.py` (F-8).
3. Decide whether `uvx iam-policy-autopilot` joins #4413 or needs its own slice (F-9), and
   whether the prose `uvx` instructions and root `README.md` prerequisites join #4413's
   change (§8.2).
4. Child-Issue proposals for the largest slices (R15 gives the numbers):
   - `startup-contracts` #4409 (37 test files, 10,219 lines): split by file-name theme into
     ai/gpt6 (14 files, 4,538 lines), azure + gcp (6 files, 856 lines), heroku (3 files, 700
     lines) and other (14 files, 4,125 lines). All 37 are tests; their subjects are validators
     and contracts, so they should follow #4399.
   - `telemetry` #4398 (17 canonical files; 62 rows): consent (6 files, 551 lines), metric emission (4 files, 1,513 lines), tests (5 files,
     2,343 lines) and the two top-level files (`cli.py`, `sync_bundles.py`, 141 lines) with
     bundle synchronisation of the 45 mirrors (with #4377).
     They share one consent contract and callers, so they must land together or behind one
     integration check; do not split the callers from the bundles.
   - `ai-payloads` #4386 (28 canonical files: 25 payload + 3 helper): split by reference
     directory, for example finetuning (8) with model-evaluation (4), model-deployment (8),
     and processing-jobs (4) with training-jobs (3) and dataset-transformation (1).
   - `cw-api` #4381 and `cw-domain` #4380 (13 canonical files each) and `startup-fixtures`
     #4408 (16 files; six are `azure-iac-terraform`, 3,301 lines) need no split on file
     count; `startup-validators` #4399 (11 files, 7,878 lines) should be judged by line
     weight rather than rows.
5. Require the #4412 and #4398 ports to state explicitly what happens when the new Rust
   binary is missing, replacing today's launchers (F-10, F-11).
6. Optionally add a machine-readable `disposition` field to the manifest in a programme
   change, so §7 stops being prose-only (U-6).

## 13. Reproduction summary

Every number in §4–§10 comes from the commands in §3 run against committed Git objects at
`71319f9aecb7f81c872aa1ee0060f9474bc7cd1b`, joined with `jq` and reduced with `awk`.
Section 3's "Observed output" is the real output of that block when it was run exactly as
printed. No Python process, parser, wrapper or repository import was used at any point in
this repair revision, and no file under audit was executed.

Friction: none
