# ci-bill

Find out which GitHub Actions workflow costs the money, and why.

[![crates.io](https://img.shields.io/crates/v/ci-bill.svg)](https://crates.io/crates/ci-bill) [![downloads](https://img.shields.io/crates/d/ci-bill.svg)](https://crates.io/crates/ci-bill) [![CI](https://github.com/qwpp7g79y4-maker/ci-bill/actions/workflows/ci.yml/badge.svg)](https://github.com/qwpp7g79y4-maker/ci-bill/actions/workflows/ci.yml) [![PEPAI checked](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fqwpp7g79y4-maker%2Fci-bill%2Fmain%2F.github%2Fbadge.json)](QUALITY.md) [![MSRV 1.85](https://img.shields.io/badge/MSRV-1.85-blue.svg)](Cargo.toml) [![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

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

Download the `--usage` CSV as a **detailed usage report** export from your
organization's or repository's GitHub billing settings; see GitHub's docs,
"Billing reports reference", for where that export lives on your plan.
GitHub's own Billing page already totals that cost; ci-bill reads the same
data and breaks it down per repository, workflow, and job, with a
`file:line` cause and a savings estimate the Billing page does not give you.

`report` and `prices` are offline: no token, no network. Only `fetch` talks
to the network, using a `GH_TOKEN` or `GITHUB_TOKEN` from the environment.

## Installation

With Rust 1.85 or newer, install from [crates.io](https://crates.io/crates/ci-bill):

```sh
cargo install --locked ci-bill
ci-bill --help
```

Ready-made archives for Linux, Windows and macOS are attached to each
[GitHub release](https://github.com/qwpp7g79y4-maker/ci-bill/releases), each
with a sha256 checksum file. Download the archive, check the checksum, extract
it, and put the binary on your `PATH`:

```sh
curl -q -fsSL -o ci-bill.tar.gz "$ARCHIVE_URL"
curl -q -fsSL -o ci-bill.tar.gz.sha256 "$ARCHIVE_URL.sha256"
sha256sum ci-bill.tar.gz && cat ci-bill.tar.gz.sha256
tar -xzf ci-bill.tar.gz
install -m 755 ci-bill-*/ci-bill "$HOME/.local/bin/"
```

Supported systems for the archives:

- Linux x86_64 (`x86_64-unknown-linux-gnu`): glibc 2.34 or newer. For an
  older distribution or Alpine, use the statically linked
  `x86_64-unknown-linux-musl` archive, which needs no glibc.
- macOS: 10.12 or newer on Intel, 11.0 or newer on Apple Silicon.
- Windows: Windows 10 or newer, x86_64.

The tests run in CI on Linux, Windows, macOS on Apple Silicon and macOS on
Intel, and on Rust 1.85. Other systems may work with `cargo install`, but are
not tested.

For the latest `main`:
`cargo install --locked --git https://github.com/qwpp7g79y4-maker/ci-bill`.

## Quick start

Try it without cloning the repository: save a one-row usage CSV and run
`ci-bill report` on it.

```sh
cat > usage.csv <<'CSV'
date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path
2026-09-01,actions,actions_linux,120,minutes,0.008,0.960,0,0.960,ada,example-org,example-org/sample-app,.github/workflows/ci.yml
CSV
ci-bill report --usage usage.csv
```

```text
USD (from net_amount)
Period: 2026-09-01 .. 2026-09-01
Repositories: example-org/sample-app

Total net: 0.96
Total minutes: 120
Other products: 0.00

Workflow                                          Net  Gross  Discount  Minutes    Share
example-org/sample-app .github/workflows/ci.yml  0.96   0.96      0.00      120  100.00%
  actions_linux                                  0.96   0.96      0.00      120         
  measured 0.00  invoice 0.96  difference 0.96
```

The CSV above is invented. See the Example section below for a larger
walkthrough on the `samples/` data in this repository.

## Commands

```sh
ci-bill report --usage CSV [--workflows DIR] [--runs DIR] [--top N] \
    [--json] [--budget AMOUNT] [--fix-diff] [--md]
ci-bill prices [--json] [--runner-map CSV] [--label LABEL] [--sku SKU] \
    [--usage CSV --repository OWNER/REPO --workflow PATH]
ci-bill fetch --repo OWNER/REPO --out DIR --since DATE [--until DATE]
```

`report` ranks cost from the usage CSV and, with `--workflows`, scans the
workflow YAML for findings. `--usage -` reads the CSV from standard input
instead of a file. Add `--runs DIR` to bring in each job's billed
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
CSV rate over the table. `--usage` and `--runner-map` both take `-` to
read their CSV from standard input, but not at the same time; only one of
them may be `-` in a single run.

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
| 1 | `--budget` given and the Actions net total is over it, or stdout could not be written (e.g. disk full) |
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

Release archives include THIRD-PARTY-LICENSES.html with the licenses of the third-party crates in the binary.
