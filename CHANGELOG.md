# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `CONTRIBUTING.md`.

### Changed

- `SECURITY.md` names 1.0.x as the supported version.

## [1.0.0] - 2026-11-08

The previous version is 0.1.2.

### Added

- Document the binary: manual page `man/ci-bill.1`, an invented usage fixture under `examples/`, and `tests/handbook_help.rs`, which checks `--help` against that page.

- Add `README.nl.md` (Dutch), an Options section in both READMEs, two more invented inputs under `examples/` with their expected output, and golden tests for them (`tests/examples_golden.rs`).
- Add a test that every option in any `--help` appears in both READMEs.

### Changed

- Input errors in `report` now add a `what to do:` line; the unknown-runner-label, missing-token, rate-limit, unauthorized, reversed-date and `GITHUB_API_URL` errors name a next step. Exit codes are unchanged (`tests/error_hints.rs`).
- State the measured test count (518 passing, 0 failing, measured on commit a6f3b9c) in README.md, README.nl.md and QUALITY.md.
- README: `report` requires `--usage`; name `help`, `--help`, and `--version`; point at `man/ci-bill.1`; align exit codes 2 and 3 with `fetch` and an unknown SKU.

## [0.1.2] - 2026-10-28

### Changed

- Read stdin when a path is `-`, and add a self-contained quick start.
- Generate THIRD-PARTY-LICENSES.html once on Linux, with no cargo-about binary for Intel macOS.
- Tests tolerate a closed stdin when the binary rejects early.

### Fixed

- Reject deeply nested workflow YAML (for example 20,000 levels, or lines separated only by CR) before the recursive parser sees it, so it gives an `invalid YAML` error instead of a stack overflow. The pre-scan handles quotes after an anchor or tag in flow collections, verbatim `!<...>` tags, and `\r\n`, `\r` and `\n` line endings.
- Stop two stdout panics in the text report and CLI.
- Exit 3 and refuse to read both `--usage` and `--runner-map` from stdin.
- Floor F1 savings at zero when macOS or Windows minutes already cost less than the same minutes at the undiscounted Linux list rate.
- Skip a malformed Link-header parameter during pagination instead of dropping a later page of runs or jobs.
- Track flow-bracket depth when a fix diff finds where a key's value ends, so an F1 or F3 diff keeps a multi-line flow map or sequence valid YAML.
- Escape a literal backslash in a Markdown table cell before escaping `|`, so a cell that contains `\|` does not split the row.

## [0.1.1] - 2026-10-21

### Security

- Update rustls-webpki to 0.103.16 (RUSTSEC-2026-0382: certificate verification may accept distrusted intermediates).
- Update webpki-roots to 1.0.10 (RUSTSEC-2026-0381: bundled root certificates allow trust in distrusted emSign intermediates).

These crates are only used by `ci-bill fetch`; `report` and `prices` make no network connection.

### Packaging

- Release archives include THIRD-PARTY-LICENSES.html with the licenses of the third-party crates in the binary.
- CI checks dependency licenses with cargo-deny.

## [0.1.0] - 2026-10-19

First release. See the [v0.1.0 release](https://github.com/qwpp7g79y4-maker/ci-bill/releases/tag/v0.1.0).

[Unreleased]: https://github.com/qwpp7g79y4-maker/ci-bill/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/qwpp7g79y4-maker/ci-bill/compare/v0.1.2...v1.0.0
[0.1.2]: https://github.com/qwpp7g79y4-maker/ci-bill/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/qwpp7g79y4-maker/ci-bill/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/qwpp7g79y4-maker/ci-bill/releases/tag/v0.1.0
