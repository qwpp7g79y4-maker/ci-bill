---
name: ci-bill
description: Find which GitHub Actions workflows cost money, from a billing usage CSV and local workflow files. Use when the user mentions GitHub Actions cost, a usage report CSV, CI billing, runner minutes, or ci-bill.
---

# ci-bill

Use the local `ci-bill` CLI (version 0.1.2). It ranks GitHub Actions cost per workflow from a detailed usage report CSV and can point to likely causes in workflow YAML.

## Install

If `ci-bill` is not on `PATH`, install it with Rust 1.85 or newer:

```sh
cargo install --locked ci-bill
ci-bill --help
```

Do not invent flags. If you are unsure, run `ci-bill --help` or `ci-bill report --help`.

## Commands (from `--help`)

```
ci-bill report --usage CSV [--workflows DIR] [--runs DIR] [--top N] [--json] [--budget AMOUNT] [--fix-diff] [--md]
ci-bill prices [--json] [--runner-map CSV] [--label LABEL] [--sku SKU] [--usage CSV --repository OWNER/REPO --workflow PATH]
ci-bill fetch --repo OWNER/REPO --out DIR --since DATE [--until DATE]
```

### `report`

Ranks cost from a detailed usage report. `--usage` is required. `-` reads the CSV from standard input.

- `--usage <CSV>` — Detailed usage report CSV
- `--workflows <DIR>` — Directory of workflow YAML files
- `--runs <DIR>` — Saved Actions API JSON (`runs-*.json` and `jobs-<run_id>*.json`) and cached remote workflows
- `--top <N>` — Show the N workflows with the highest net amount
- `--json` — One JSON object instead of the text report
- `--budget <AMOUNT>` — Net total budget in USD; exit 1 when the total is above this
- `--fix-diff` — Print a unified diff fixing the highest-savings finding instead of the report
- `--md` — Print a one-page Markdown teardown instead of the report

### `prices`

Shows the built-in Actions runner price table, or the USD/minute rate for one `--label` or `--sku`. `--usage` and `--runner-map` may be `-` for stdin, but not both at the same time.

### `fetch`

Downloads a repository's Actions runs, jobs, and referenced reusable workflows into `--out DIR`, in the layout `--runs` reads. `--since` and `--until` are `YYYY-MM-DD` in UTC. `--until` defaults to today.

`report` and `prices` are offline: no token, no network. Only `fetch` talks to the network, using a `GH_TOKEN` or `GITHUB_TOKEN` from the environment. Never print that token. Prefer `report` on a CSV the user already exported from GitHub billing unless they explicitly ask to fetch API data.

## Examples from the project README

Try it without cloning the repository:

```sh
cat > usage.csv <<'CSV'
date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path
2026-09-01,actions,actions_linux,120,minutes,0.008,0.960,0,0.960,ada,example-org,example-org/sample-app,.github/workflows/ci.yml
CSV
ci-bill report --usage usage.csv
```

On the sample data in a clone of the repository:

```sh
ci-bill report --usage samples/usage-sample.csv --workflows samples/.github/workflows
```

## Exit codes

- 0 — ok
- 1 — `--budget` given and the Actions net total is over it, or stdout could not be written
- 2 — an input file could not be read
- 3 — a usage error

The CSV `net_amount` stays authoritative for totals and `--budget`.
