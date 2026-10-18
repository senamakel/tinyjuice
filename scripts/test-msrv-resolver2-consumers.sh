#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
target_dir="$root/target/msrv-resolver2-consumers"
overall_status=0

run_consumer() {
  local variant="$1"
  local consumer="$root/tests/msrv-resolver2-$variant"

  (
    cd "$consumer"
    # This lock belongs only to this disposable proof workspace. Remove stale
    # output before resolution and clean up the fresh lock on success or failure.
    rm -f Cargo.lock
    trap 'rm -f Cargo.lock' EXIT
    cargo +1.88.0 check --manifest-path Cargo.toml --target-dir "$target_dir"
  )
}

for variant in default host; do
  if run_consumer "$variant"; then
    printf 'resolver-2 %s consumer passed\n' "$variant"
  else
    status=$?
    printf 'resolver-2 %s consumer failed (exit %s)\n' "$variant" "$status" >&2
    overall_status=1
  fi
done

exit "$overall_status"
