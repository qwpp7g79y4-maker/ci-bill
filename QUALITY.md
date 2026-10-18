# Quality notes

Short, honest status of what is tested in this repository and how. Written
by PEPAI (pep_ai@icloud.com).

## Tests

`cargo test` (debug, stable 1.99 and MSRV 1.85, both green):

- 302 unit tests (`src/lib.rs`, `#[cfg(test)]` modules across the crate).
- 167 integration tests across 23 files under `tests/`, driving the real
  `ci-bill` binary as a subprocess (CSV/YAML/JSON fixtures on disk, checked
  exit codes, stdout/stderr, and JSON snapshots).
- 469 tests total, 0 failing.

`cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` are both
clean, for the main crate and for `fuzz/`.

## Fuzzing

`fuzz/` is a standalone cargo-fuzz crate (its own one-member workspace, not
part of the main crate's package or workspace), with one libFuzzer target
per untrusted-input stage:

| target | stage | smoke-run (60s, `-jobs=1 -rss_limit_mb=2048`) |
|---|---|---|
| `usage_csv` | usage CSV → `rank::rank` | 93,155 runs, 0 crashes |
| `workflow_yaml` | workflow YAML, anchors/aliases → `findings::findings` | 1,396,603 runs, 0 crashes |
| `api_json` | saved `--runs` API JSON pages | 4,945,933 runs, 0 crashes |
| `price_files` | price-table JSON + runner-map CSV | 644,300 runs, 0 crashes |
| `json_render` | full CSV → `report::render_json` | 74,190 runs, 0 crashes |
| `fix_diff` | `--fix-diff` diff applied with real `git apply`, re-parsed | 667,608 runs, 0 crashes |

Each target asserts no panic. `json_render` additionally asserts the
rendered text parses as JSON and has a string `total_net` field. `fix_diff`
additionally asserts that when `git apply --check` accepts the diff, the
patched file still parses as valid workflow YAML (the exact property CB-3
violated).

These are smoke runs, not a long-running fuzzing campaign: they found no
new crashes in the 60 seconds each was given, which is evidence the known
findings are fixed and the obvious shallow cases are covered, not proof the
code is crash-free under sustained fuzzing.

## Review findings CB-1 .. CB-6

All six findings from the prior review are fixed and committed, each with
its own regression test:

- **CB-1** (YAML anchor/alias expansion could exhaust memory): fixed by a
  node budget in `src/workflow.rs`. Tests:
  `billion_laughs_sequence_aliases_are_a_parse_error`,
  `billion_laughs_merge_key_aliases_are_a_parse_error`,
  `repeated_non_multiplying_alias_reuse_stays_within_budget`. Also exercised
  directly by the `workflow_yaml` fuzz target.
- **CB-2** (CSV money-sum overflow panicked): fixed with checked addition
  and a clean `UsageError` in `src/rank.rs`. Tests:
  `net_amount_overflow_in_one_workflow_is_a_clean_error`,
  `gross_amount_overflow_in_one_workflow_is_a_clean_error`,
  `discount_amount_overflow_in_one_workflow_is_a_clean_error`,
  `quantity_overflow_in_one_workflow_is_a_clean_error`,
  `grand_total_net_overflow_across_workflows_is_a_clean_error`,
  `grand_total_quantity_overflow_across_workflows_is_a_clean_error`,
  `other_products_net_overflow_is_a_clean_error`,
  `negative_net_overflow_in_one_workflow_is_a_clean_error`, plus
  `tests/cli.rs::amount_overflow_exits_2_for_text_json_and_md`. A broader
  audit of every other `+`/`+=`/`sum()` on `Micros` reachable from CSV,
  YAML, or API JSON input found three more unchecked sums (in
  `prices::quote_sku`, `report::omitted_net`, `overlap::findings`) and fixed
  each; see the "Extra audit" section below.
- **CB-3** (`--fix-diff` corrupted flow-style jobs while `git apply --check`
  stayed green): fixed by making the flow-style patch logic aware of the
  brace structure in `src/fix_diff.rs`. Tests: eight new unit tests there
  (`flow_lint_on_runner_edits_only_the_value`,
  `flow_missing_timeout_inserts_a_flow_entry_after_the_brace`, and others),
  plus five integration tests in `tests/fix_diff.rs`
  (`fix_diff_pure_flow_lint_on_runner_edits_only_the_value`,
  `fix_diff_flow_job_inside_block_jobs_fixes_both_findings`, and others)
  that run the real `git apply --check` and `git apply`. Also exercised
  directly by the `fix_diff` fuzz target, which does the same real-`git`
  check on fuzzer-generated workflows.
- **CB-4** (`Retry-After` was ignored once `x-ratelimit-remaining: 0`):
  fixed by checking `Retry-After` first in `src/fetch.rs::decide`. Tests:
  `retry_after_wins_over_primary_limit_remaining_zero`,
  `retry_after_wins_over_secondary_status_429_with_remaining_zero`.
- **CB-5** (a second YAML document was silently dropped): fixed by
  rejecting more than one mapping document as a parse error in
  `src/workflow.rs`. Tests:
  `a_single_leading_document_marker_or_trailing_end_marker_still_works`,
  `a_second_yaml_document_is_a_parse_error_instead_of_a_silent_drop`.
- **CB-6** (control characters and ANSI escapes in CSV fields could forge
  terminal lines): fixed by escaping control characters before they reach
  text/Markdown output in `src/report.rs`/`src/md.rs`. Tests:
  `control_characters_and_bidi_overrides_become_visible`,
  `control_characters_and_pipes_in_file_paths_are_escaped`,
  `control_characters_in_input_fields_are_escaped_not_raw`.

### Extra audit: remaining unchecked `Micros` sums

Beyond CB-2's own fix site, every other `+`, `+=`, and `.sum()` on `Micros`
reachable from untrusted input was checked by hand and, where needed, with
a crafted reproducer:

- `prices::quote_sku` summed CSV `net`/`quantity` per repository with the
  panicking `Add` impl. Two large CSV rows for the same workflow panicked
  the `prices` command. Fixed with `checked_add` and a clean, line-numbered
  `PriceFail::Input`. Regression test:
  `tests/prices.rs::csv_net_amount_sum_overflow_is_a_clean_error_not_a_panic`
  (confirmed to fail with `Some(101)` before the fix, `Some(2)` after).
- `report::omitted_net` (the "N more workflows (X USD)" summary) summed a
  *subset* of workflow totals with `.sum()`, separate from the grand total
  CB-2 already checks. A CSV with one large positive and two large negative
  workflow totals passes the (checked) grand total but overflowed this
  subset sum. Fixed by saturating instead, consistent with the
  derived/estimate sums elsewhere (`job_cost`, `matrix_cost`, `savings`,
  `reconcile`). Regression test:
  `tests/cli.rs::omitted_workflows_net_overflow_does_not_panic` (confirmed
  to fail with `Some(101)` before the fix, `Some(0)` after).
- `overlap::findings` summed cancel-in-progress USD (`usd +=`,
  `bucket.usd +=`) with the panicking `Add` impl, driven by a CSV-derived
  per-minute rate from a crafted `--runs` API JSON directory. Two
  overlapping run pairs on one workflow, each near `i64::MAX`, panicked.
  Fixed by saturating. Regression test:
  `tests/overlap.rs::two_huge_overlap_amounts_on_one_workflow_do_not_panic`
  (confirmed to fail with `Some(101)` before the fix, `Some(0)` after).
- `rerun::findings` also summed re-run cost with `.sum()`. Changed to the
  same saturating fold for consistency, but no overflow could be
  reproduced: `reconcile::allocate` sums job shares back to the bucket's
  own (already-bounded) total exactly, and the list-price fallback path
  cannot reach anywhere near `i64::MAX` within a parseable timestamp range.
  No regression test is claimed for this one since no crash was shown.

## Open points

- The fuzz targets are smoke-tested for 60 seconds each, not run as a
  continuous campaign. Longer runs, especially on `usage_csv` and
  `json_render` with a richer seed corpus, would likely find more of the
  input space.
- `fix_diff`'s fuzz target shells out to the real `git` binary per
  iteration, which caps its throughput well below the other targets; a
  pure in-memory unified-diff applier would let it run far more iterations
  per second at the cost of fidelity to the real `git apply` behavior.
- The prior review's two "not reproduced" items (a fetch write interrupted
  between delete and rename, and two concurrent `fetch` processes to the
  same `--out` directory) are still open; this pass did not attempt them.
