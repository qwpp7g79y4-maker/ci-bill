# Sample Actions API responses

Everything in this folder is invented sample data for tests. It is not a real
GitHub account, repository, or bill.

`runs-*.json` files are saved bodies from
`GET /repos/{owner}/{repo}/actions/runs`.
`jobs-<run_id>*.json` files are saved bodies from
`GET /repos/{owner}/{repo}/actions/runs/{run_id}/jobs?filter=all`.
`runs-1.json` and `runs-2.json` are two pages of runs.
`jobs-1001.json` and `jobs-1001-2.json` are two pages of jobs for run 1001.

Run 1002, job `lint`, has `completed_at` set to null. That job is still
running, so it is in progress and does not count as a finished job.
