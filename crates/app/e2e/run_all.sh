#!/usr/bin/env bash
# Run every E2E scenario against the release binary; exit 1 if any failed.
# cargo build --release -p easycad first. Screenshots land in e2e/shots/.
cd "$(dirname "$0")" || exit 1
fail=0
for s in [a-z]_*.sh; do
    bash "$s" || { echo "== $s FAILED"; fail=1; }
done
exit $fail
