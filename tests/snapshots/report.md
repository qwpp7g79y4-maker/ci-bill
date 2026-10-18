# ci-bill teardown

Period: 2026-09-01 .. 2026-09-30
Repositories: example-org/sample-app

Total net: 284.21 USD

## Top jobs

| Job | File:Line | Amount (USD) |
| --- | --- | --- |
| lint | samples/.github/workflows/lint.yml:8 | 230.14 |
| test | samples/.github/workflows/ci.yml:9 | 44.23 |
| build-windows | samples/.github/workflows/release.yml:7 | 7.72 |
| docs | samples/.github/workflows/docs.yml:5 | 2.11 |

## Findings

| Finding | File:Line | Message | Savings (USD) |
| --- | --- | --- | --- |
| F1 | samples/.github/workflows/lint.yml:8 | lint on macOS | 207.87 |
| F2 | samples/.github/workflows/lint.yml:7 | default timeout is 360 minutes | 22.32 |
| F2 | samples/.github/workflows/docs.yml:4 | default timeout is 360 minutes | 2.16 |
| F3 | samples/.github/workflows/lint.yml:2 | pull_request without concurrency cancel-in-progress | 0.00 |

## Suggested fix

```diff
--- a/samples/.github/workflows/lint.yml
+++ b/samples/.github/workflows/lint.yml
@@ -5,7 +5,7 @@
 
 jobs:
   lint:
-    runs-on: macos-latest
+    runs-on: ubuntu-latest
     steps:
       - uses: actions/checkout@v4
       - run: cargo fmt --check
```
