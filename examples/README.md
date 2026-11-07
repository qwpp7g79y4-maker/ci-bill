# Examples

Invented fixtures for `ci-bill`. Not a real account, organization, or bill:
no customer data, keys, or tokens. Run every command from the repository root.

## Minimal

```sh
ci-bill report --usage examples/usage.csv
```

Expected exit code: 0

## Webshop: one repository, expensive lint on macOS

```sh
ci-bill report --usage examples/webshop-usage.csv
```

Expected output: `examples/webshop-usage.expected.txt`

Expected exit code: 0

## API gateway: usage CSV plus workflow files

```sh
ci-bill report --usage examples/api-gateway-usage.csv --workflows examples/api-gateway-workflows
```

Expected output: `examples/api-gateway-usage.expected.txt`

Expected exit code: 0

`tests/examples_golden.rs` runs both commands and compares the output.
