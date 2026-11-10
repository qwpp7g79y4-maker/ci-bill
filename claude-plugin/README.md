# CI Bill

This plugin helps Claude use the `ci-bill` command-line tool. The tool ranks GitHub Actions cost per workflow from a detailed usage report CSV, and can point to likely causes in local workflow files.

## Use it

Install the tool with Rust 1.85 or newer: `cargo install --locked ci-bill`. Give Claude a usage CSV from GitHub billing, and optionally a workflows directory. Ask which workflow costs the most, or where savings might be. `report` and `prices` run offline. Only `fetch` talks to the network, and only when the user already has a GitHub token in the environment.

## Data

`report` and `prices` send nothing to the network and need no token. They read the CSV and files you pass. `fetch` calls the GitHub API using `GH_TOKEN` or `GITHUB_TOKEN` from the environment, and writes run and job JSON to a local directory. This plugin does not store data itself. Never print a token in the conversation.
