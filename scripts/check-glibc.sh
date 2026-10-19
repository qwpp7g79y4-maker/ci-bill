#!/usr/bin/env bash
# Fail when a binary needs a glibc symbol newer than the given version.
# Usage: check-glibc.sh BINARY MAX_VERSION
set -euo pipefail
bin="$1"
max="$2"
highest=$(objdump -T "$bin" | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' | sed 's/^GLIBC_//' | sort -V | tail -n 1)
if [ -z "$highest" ]; then
  echo "no GLIBC symbol versions found in $bin" >&2
  exit 1
fi
echo "highest GLIBC symbol: $highest (allowed: $max)"
if [ "$(printf '%s\n%s\n' "$highest" "$max" | sort -V | tail -n 1)" != "$max" ]; then
  echo "$bin needs GLIBC_$highest, newer than $max" >&2
  exit 1
fi
