#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

engine_pattern='^(rusqlite|libsqlite3-sys|mongodb|bson|aws-sdk-s3|lapin|amq-protocol|sqlx|redb|sled|rocksdb|tinystoragedrivers-(document|kv|blob|queue|secrets|connect)|tinystoragedrivers)$'

strip_carriage_return() {
  printf '%s' "${1%$'\r'}"
}

tree_names() {
  printf '%s\n' "$1" | tr -d '\r' | awk '{print $1}'
}

# Cargo metadata may be emitted with CRLF on Windows. Keep this parser
# self-test Bash 3 compatible, as used by the macOS runners.
crlf_packages=$'tinyjuice\r\ntinyjuice-module\r\n'
normalized_packages=''
while IFS= read -r package; do
  [[ -n "$package" ]] || continue
  package="$(strip_carriage_return "$package")"
  normalized_packages="${normalized_packages:+${normalized_packages}$'\n'}${package}"
done <<< "$crlf_packages"
if [[ "$normalized_packages" != $'tinyjuice\ntinyjuice-module' ]]; then
  echo "engine gate failed its CRLF package-list self-test" >&2
  exit 1
fi
if [[ "$(tree_names $'tinyjuice\r\nrusqlite\r\n')" != $'tinyjuice\nrusqlite' ]]; then
  echo "engine gate failed its CRLF cargo-tree self-test" >&2
  exit 1
fi

# Exact matching must detect backend engines, without rejecting the public
# ports or names that merely contain a forbidden package name.
for name in rusqlite libsqlite3-sys mongodb bson aws-sdk-s3 lapin amq-protocol sqlx redb sled rocksdb tinystoragedrivers-document tinystoragedrivers-kv tinystoragedrivers-blob tinystoragedrivers-queue tinystoragedrivers-secrets tinystoragedrivers-connect tinystoragedrivers; do
  if ! printf '%s\n' "$name" | grep -xE "$engine_pattern" >/dev/null; then
    echo "engine matcher missed package: $name" >&2
    exit 1
  fi
done
for name in tinyjuice tinyjuice-bus tinyjuice-drivers tinyjuice-module tinystoragedrivers-api tinystoragedrivers-busproxy tinystoragedrivers-testkit tinystoragedrivers-types not-tinystoragedrivers-document tinystoragedrivers-types-extra; do
  if printf '%s\n' "$name" | grep -xE "$engine_pattern" >/dev/null; then
    echo "engine matcher rejected allowed package name: $name" >&2
    exit 1
  fi
done

libraries="$(cargo metadata --locked --format-version 1 --no-deps | python3 -c '
import json, sys
metadata = json.load(sys.stdin)
members = set(metadata["workspace_members"])
library_kinds = {"lib", "rlib", "cdylib", "dylib", "staticlib", "proc-macro"}
names = sorted({
    package["name"]
    for package in metadata["packages"]
    if package["id"] in members
    and any(library_kinds.intersection(target["kind"]) for target in package["targets"])
})
print("\n".join(names))
')"
if [[ -z "$libraries" ]]; then
  echo "Cargo metadata found no workspace library targets" >&2
  exit 1
fi

while IFS= read -r package; do
  package="$(strip_carriage_return "$package")"
  [[ -n "$package" ]] || continue
  for features in default all; do
    args=(tree --locked -p "$package" -e normal,build --prefix none)
    if [[ "$features" == all ]]; then
      args+=(--all-features)
    fi
    tree="$(cargo "${args[@]}")"
    names="$(tree_names "$tree")"
    if ! printf '%s\n' "$names" | grep -Fx "$package" >/dev/null; then
      echo "cargo tree for $package ($features) had no root package entry" >&2
      exit 1
    fi
    forbidden="$(printf '%s\n' "$names" | grep -xE "$engine_pattern" || true)"
    if [[ -n "$forbidden" ]]; then
      printf 'engine dependencies found in %s (%s features):\n%s\n' "$package" "$features" "$forbidden" >&2
      exit 1
    fi
  done
done <<< "$libraries"
