# Changelog

## 0.1.1 - 2026-10-21

### Security

- Update rustls-webpki to 0.103.16 (RUSTSEC-2026-0382: certificate verification may accept distrusted intermediates).
- Update webpki-roots to 1.0.10 (RUSTSEC-2026-0381: bundled root certificates allow trust in distrusted emSign intermediates).

These crates are only used by `ci-bill fetch`; `report` and `prices` make no network connection.

### Packaging

- Release archives include THIRD-PARTY-LICENSES.html with the licenses of the third-party crates in the binary.
- CI checks dependency licenses with cargo-deny.

## 0.1.0

First release. See the [v0.1.0 release](https://github.com/qwpp7g79y4-maker/ci-bill/releases/tag/v0.1.0).
