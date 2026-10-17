#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
manifest=tests/storage-needs-consumer/Cargo.toml

cargo test --locked --manifest-path "$manifest"

normal_graph="$(cargo tree --locked --manifest-path "$manifest" --edges normal,build --prefix none)"
if grep -Eq '^tinybus v' <<<"$normal_graph"; then
  echo "TinyJuice bus-only consumer unexpectedly depends on TinyBus" >&2
  exit 1
fi
if ! grep -Eq '^tinystoragedrivers-bus v1\.3\.0([[:space:]]|$)' <<<"$normal_graph"; then
  echo "TinyJuice bus-only consumer is missing tinystoragedrivers-bus 1.3.0" >&2
  exit 1
fi

feature_graph="$(cargo tree --locked --manifest-path "$manifest" --edges features --invert tinystoragedrivers-bus)"
if grep -Fq 'tinystoragedrivers-bus feature "client"' <<<"$feature_graph"; then
  echo "TinyJuice bus-only consumer enabled the optional storage TinyBus client" >&2
  exit 1
fi
