# ci-bill

Find out which GitHub Actions workflow costs the money, and why.

`ci-bill` ranks cost per repository, workflow, and runner SKU, and points to
likely causes with `file:line`. It reads two kinds of input:

- the **detailed usage report** CSV that GitHub Actions billing exports,
  given with `--usage`
- the **Actions REST API**'s runs and jobs, saved ahead of time by
  `ci-bill fetch` (or any other means) into a directory of
  `runs-*.json`/`jobs-<run_id>*.json` files, given with `--runs`

Either input works on its own; together they add measured, per-job cost on
top of the invoice. It also reads `.github/workflows/*.yml`/`*.yaml`,
including reusable workflows and composite actions, with `--workflows`.

`report` and `prices` are offline: no token, no network. Only `fetch` talks
to the network, using a `GH_TOKEN` or `GITHUB_TOKEN` from the environment.

## Installation

ci-bill is not published on crates.io. With Rust 1.85 or newer, install it
from this repository:

```sh
cargo install --git https://github.com/qwpp7g79y4-maker/ci-bill --locked
```

## Commands

```sh
ci-bill report --usage CSV [--workflows DIR] [--runs DIR] [--top N] \
    [--json] [--budget AMOUNT] [--fix-diff] [--md]
ci-bill prices [--json] [--runner-map CSV] [--label LABEL] [--sku SKU] \
    [--usage CSV --repository OWNER/REPO --workflow PATH]
ci-bill fetch --repo OWNER/REPO --out DIR --since DATE [--until DATE]
```

`report` ranks cost from the usage CSV and, with `--workflows`, scans the
workflow YAML for findings. Add `--runs DIR` to bring in each job's billed
minutes, re-run waste, matrix rounding loss, and overlap savings from a
saved API cache. `--top N` keeps the N highest-net workflows in the table;
the rest are summarized as one line and their findings move to a
"Findings outside the top N" section. `--json` prints one JSON object
(schema_version 2, amounts as 6-decimal USD strings) instead of text.
`--budget AMOUNT` compares the Actions net total (not "Other products") to
a USD amount and exits 1 with a `Budget exceeded: X > Y` line when it is
over. `--fix-diff` prints a unified diff that fixes the highest-savings
finding instead of the report. `--md` prints a one-page Markdown teardown
(period, repository, total, top 5 jobs, findings with savings, and the
same diff) instead of the report.

`prices` prints the built-in Actions runner price table, with its sources
and retrieval date, or the USD/minute rate for one `--label` (a runner
label such as `ubuntu-latest` or `[self-hosted, macos]`) or `--sku`. Add
`--usage`, `--repository`, and `--workflow` to prefer that workflow's own
CSV rate over the table.

`fetch` downloads a repository's Actions runs and jobs, and any reusable
workflows they call, into `--out DIR` in the layout `--runs` reads.

## Measured vs invoice

Each workflow's measured cost is its jobs' runtime priced at the current
list rate; its invoice is the CSV `net_amount` for that workflow. The two
differ because the invoice can include a negotiated or bundled rate, a
credit, or usage that predates the current price table. The CSV net stays
authoritative for totals and `--budget`; measured cost only explains where
the invoice amount came from and feeds the savings in each finding.

## Findings

- **F1** a lint/format/fmt/clippy/eslint/prettier job (by name or by what
  its steps run) on a macOS or Windows runner, including a runner label
  built from a matrix variable
- **F2** a job with no `timeout-minutes` (GitHub's default is 360 minutes)
- **F3** a workflow with `pull_request` or `pull_request_target` and no
  job- or workflow-level `concurrency` with `cancel-in-progress: true`
- **F4** a matrix that expands to six or more billed jobs, or whose size
  depends on an expression
- **F5** an extra execution of the same job name inside one workflow run
  (a re-run), separate from a job inherited unchanged from an earlier
  attempt
- **F6** a job's `uses: ./.github/workflows/<file>` with no matching file
  under `--workflows`
- **F7** a reusable workflow that calls back into a workflow already being
  expanded
- **F8** the same mapping key written more than once in one YAML document;
  the later value is kept
- **F9** a usage row where `gross_amount` disagrees with
  `quantity * applied_cost_per_quantity`, or `net_amount` disagrees with
  `gross_amount - discount_amount`; the CSV `net_amount` stays authoritative

Findings with a workflow show their savings in USD. A finding whose YAML
file is not matched to a usage row is listed under "Workflows without
usage".

## Exit codes

| Code | Meaning |
| ---- | ------- |
| 0 | ok |
| 1 | `--budget` given and the Actions net total is over it |
| 2 | an input file could not be read: missing column, broken number, invalid YAML/JSON, unreadable file |
| 3 | a usage error: unknown option, missing required flag, an invalid `--budget` amount, or a runner label not in the price table |

## Example

```sh
cargo run --quiet -- report --usage samples/usage-sample.csv --workflows samples/.github/workflows
```

```text
USD (from net_amount)
Period: 2026-09-01 .. 2026-09-30
Repositories: example-org/sample-app

Total net: 284.21
Total minutes: 12208
Other products: 0.00

Workflow                                                 Net   Gross  Discount  Minutes   Share
example-org/sample-app .github/workflows/lint.yml     230.14  230.14      0.00     3712  80.98%
  actions_macos                                       230.14  230.14      0.00     3712        
  measured 0.00  invoice 230.14  difference 230.14
  lint  samples/.github/workflows/lint.yml:8 = 230.14
  F1 samples/.github/workflows/lint.yml:8: lint on macOS
    savings 207.87 USD (linux_rate)
  F2 samples/.github/workflows/lint.yml:7: default timeout is 360 minutes
    savings 22.32 USD (stuck_run)
  F3 samples/.github/workflows/lint.yml:2: pull_request without concurrency cancel-in-progress
    savings 0.00 USD (overlap)
example-org/sample-app .github/workflows/ci.yml        44.23   44.23      0.00     7372  15.56%
  actions_linux                                        44.23   44.23      0.00     7372        
  measured 0.00  invoice 44.23  difference 44.23
  test  samples/.github/workflows/ci.yml:9 = 44.23
example-org/sample-app .github/workflows/release.yml    7.72    7.72      0.00      772   2.72%
  actions_windows                                       7.72    7.72      0.00      772        
  measured 0.00  invoice 7.72  difference 7.72
  build-windows  samples/.github/workflows/release.yml:7 = 7.72
example-org/sample-app .github/workflows/docs.yml       2.11    2.11      0.00      352   0.74%
  actions_linux                                         2.11    2.11      0.00      352        
  measured 0.00  invoice 2.11  difference 2.11
  docs  samples/.github/workflows/docs.yml:5 = 2.11
  F2 samples/.github/workflows/docs.yml:4: default timeout is 360 minutes
    savings 2.16 USD (stuck_run)
example-org/sample-app (no workflow)                    0.00    0.01      0.01        0   0.00%
  actions_storage                                       0.00    0.01      0.01        0        
  measured 0.00  invoice 0.00  difference 0.00
```

The sample data above is invented; see `samples/README.md`.

## Known limitations

- **F4** counts jobs, not cost. A large matrix of cheap Linux jobs is
  flagged the same way as an expensive one.
- **F5** cannot reliably tell an intentional retry from a wasteful re-run;
  the saved API data does not say why a job ran again.
- Only usage rows with product `actions` are ranked. Rows for other
  products are added to one "Other products" total and are not broken down
  per repository or workflow.
- On a case-insensitive filesystem (the default on macOS and Windows),
  `fetch` stores two called workflows whose paths differ only in letter
  case (for example `lint.yml@v1` and `Lint.yml@V1`) in the same cache
  file, so the later download replaces the earlier one.

## License

MIT OR Apache-2.0. Maintained by PEPAI (pep_ai@icloud.com).
