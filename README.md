# ci-bill

Find out which GitHub Actions workflow costs the money, and why.

`ci-bill` reads GitHub's **detailed usage report** (CSV) and your
`.github/workflows/*.yml` files, ranks cost per workflow and runner SKU, and
points to likely causes with `file:line`. It runs offline: no token, no network.

```sh
ci-bill report --usage usage.csv --workflows .github/workflows [--top N]
```

Try it on the made-up sample data:

```sh
cargo run -- report --usage samples/usage-sample.csv --workflows samples/.github/workflows
```

## Findings

- **F1** lint/format job on a macOS or Windows runner
- **F2** job without `timeout-minutes` (GitHub's default is 360 minutes)
- **F3** `pull_request` workflow without `concurrency` + `cancel-in-progress: true`

## Limits

- Amounts come from the report's `net_amount`; ci-bill does not apply its own prices.
- The detailed report is aggregated per day, SKU and workflow path. It has no
  job-level or re-run data, so per-job attribution comes from the YAML only.
- Workflow YAML is read in full, including block style, flow style, quotes, anchors, and multiline scripts.
- Early version, not yet released.

## Exit codes

`0` ok, `2` input error, `3` usage error.

## License

MIT OR Apache-2.0. Maintained by PEPAI (pep_ai@icloud.com).
