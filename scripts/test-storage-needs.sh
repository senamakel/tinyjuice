#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
repo_root="$(pwd -P)"
manifest=tests/storage-needs-consumer/Cargo.toml
target_dir="$repo_root/target/storage-needs-proof"

cargo test --locked --manifest-path "$manifest" --target-dir "$target_dir"

metadata="$(cargo metadata --locked --manifest-path "$manifest" --format-version 1)"
python3 scripts/check-storage-needs-metadata.py <<<"$metadata"

normal_graph="$(cargo tree --locked --manifest-path "$manifest" --edges normal,build --prefix none)"
if ! grep -Eq '^tinystoragedrivers-bus v1\.5\.0([[:space:]]|$)' <<<"$normal_graph"; then
  echo "TinyJuice bus-only consumer is missing tinystoragedrivers-bus 1.5.0 from its normal/build graph" >&2
  exit 1
fi
while IFS= read -r entry; do
  package="${entry%% *}"
  case "$package" in
    tinybus|tinybus-*|tokio|tokio-*|async-std|async-std-*|smol|smol-*|\
      reqwest|ureq|isahc|curl|hyper|hyper-*|http|http-body|http-body-util|\
      tower|tower-*|axum|actix-web|sqlx|diesel|sea-orm|sea-orm-*|\
      rusqlite|libsqlite3-sys|mongodb|bson|mysql|postgres|redis|sled|redb|\
      rocksdb|librocksdb-sys|tinystoragedrivers|tinystoragedrivers-core|\
      tinystoragedrivers-client|tinystoragedrivers-busproxy|\
      tinystoragedrivers-secrets|tinystoragedrivers-tinybus|\
      tinystoragedrivers-sqlite|tinystoragedrivers-file|\
      tinystoragedrivers-mongodb|tinystoragedrivers-s3|\
      tinyjuice|tinyjuice-module|tinyjuice-drivers)
      echo "TinyJuice bus-only consumer unexpectedly depends on $package" >&2
      exit 1
      ;;
  esac
done <<<"$normal_graph"

feature_graph="$(cargo tree --locked --manifest-path "$manifest" --edges features --invert tinystoragedrivers-bus)"
if ! grep -Fq 'tinystoragedrivers-bus v1.5.0' <<<"$feature_graph"; then
  echo "TinyJuice bus-only consumer feature graph is missing tinystoragedrivers-bus 1.5.0" >&2
  exit 1
fi
if grep -Fq 'tinystoragedrivers-bus feature "client"' <<<"$feature_graph"; then
  echo "TinyJuice bus-only consumer enabled the optional storage TinyBus client" >&2
  exit 1
fi
