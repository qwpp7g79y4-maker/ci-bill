# Quality notes

Short, honest status of what is tested in this repository and how. Written
by PEPAI (pep_ai@icloud.com).

## Tests

`cargo test` (debug and release, stable 1.99 and MSRV 1.85, all green):

- 312 unit tests (`src/lib.rs`, `#[cfg(test)]` modules across the crate).
- 167 integration tests across 22 files under `tests/`, driving the real
  `ci-bill` binary as a subprocess (CSV/YAML/JSON fixtures on disk, checked
  exit codes, stdout/stderr, and JSON snapshots).
- 479 tests total, 0 failing.

`cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` are both
clean, for the main crate and for `fuzz/`.

## CI

`.github/workflows/ci.yml` runs on every push and pull request (and is
callable from a release workflow later):

- `test`: stable toolchain on `ubuntu-latest`, `windows-latest`,
  `macos-latest`, and `macos-15-intel` — `cargo fmt --check`, clippy with
  `-D warnings`, `cargo test --locked`, `cargo test --release --locked`.
- `msrv`: the same clippy and test steps on `ubuntu-latest` with Rust 1.85.
- `audit`: `cargo audit` against the advisory database.
- `fuzz-build`: `cargo +nightly fuzz build` for every fuzz target, to catch
  fuzz-target bitrot without running a full campaign in CI.

Every job's commands were run locally first with `-j 2`/`--test-threads=2`
to bound memory and CPU use. The first runs on GitHub (19 October 2026)
failed on Windows and macOS; the fixes are listed under "Cross-OS fixes"
below. CI run 39177348023 on commit `2c1614a` passed all seven jobs.

## Cross-OS fixes from the first GitHub CI runs

- `--fix-diff` wrote the `--- a/`/`+++ b/` headers with the OS path, so on
  Windows they held `\` and `git apply` rejected the diff. Headers now use
  `/`. Test: `fix_diff::tests::diff_headers_use_forward_slashes`, plus the
  `tests/fix_diff.rs` tests that apply the diff with `git apply`.
- Reports showed a workflow path built from `--workflows` and a file name
  with mixed separators on Windows (`samples/.github/workflows\lint.yml`).
  The text, Markdown and JSON reports now show paths with `/` on every OS.
  Test: `display::tests::display_path_uses_forward_slashes`.
- Several tests assumed a case-sensitive filesystem or `/` in printed
  paths; they were adjusted, not the behaviour they check.
- Not fixed: on a case-insensitive filesystem (the default on macOS and
  Windows), `fetch` stores two called workflows whose paths differ only in
  letter case in one cache file, so the later download replaces the
  earlier one. The README lists this under known limitations.

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
- The panicking `impl Add for Micros` (and the `AddAssign`/`Sum` impls
  built on it) had no callers left after the fixes above, so it was removed.
  Every money sum now has to use `checked_add` or an explicit saturating
  form, which makes an overflow panic on `Micros` impossible to reintroduce
  by accident. Test: `money::tests::checked_add_reports_overflow_instead_of_panicking`.

## EC15: atomic fetch cache writes

`fetch`'s cache writers (`write_merged_runs`, `write_job_batch`) used to
delete the old cache files for a chunk before renaming the new temp files
over them. An interruption between the delete and the rename (a crash, a
killed process) could leave that chunk missing entirely. Fixed in
`src/fetch.rs` by writing and `fsync`-ing every new file to a temp path
first, renaming each temp over its destination (a `rename` replaces the
destination atomically on every supported OS, including Windows), and only
removing whichever old files the new set no longer needs once every rename
has landed. Tests:
`write_merged_runs_leaves_old_cache_intact_if_a_temp_write_fails`,
`write_job_batch_leaves_old_cache_intact_if_a_temp_write_fails`,
`write_merged_runs_drops_stale_files_only_after_new_ones_land`,
`write_merged_runs_keeps_every_old_file_if_a_rename_fails`.

## Overflow error wording

The "sum of all rows" overflow error (the grand total across an entire
report) printed a `line 0` that named no real row. Fixed in `src/usage.rs`
(`UsageError`'s `Display` impl drops the line number for this case) and
`src/rank.rs` (a comment at the one call site that can reach it). Tests:
`no_overflow_error_message_prints_line_zero`,
`amount_overflow_with_a_known_line_names_it`,
`amount_overflow_with_no_known_line_drops_it`, and the updated
`tests/cli.rs::amount_overflow_exits_2_for_text_json_and_md`.

## Open points

- The fuzz targets are smoke-tested for 60 seconds each, not run as a
  continuous campaign. Longer runs, especially on `usage_csv` and
  `json_render` with a richer seed corpus, would likely find more of the
  input space.
- `fix_diff`'s fuzz target shells out to the real `git` binary per
  iteration, which caps its throughput well below the other targets; a
  pure in-memory unified-diff applier would let it run far more iterations
  per second at the cost of fidelity to the real `git apply` behavior.
- The prior review's other "not reproduced" item (two concurrent `fetch`
  processes writing to the same `--out` directory) is still open; this
  pass did not attempt it. EC15 above fixes the single-process interrupted
  write, not concurrent-writer races.
- The `fetch` cache case collision on case-insensitive filesystems (see
  "Cross-OS fixes" above).
