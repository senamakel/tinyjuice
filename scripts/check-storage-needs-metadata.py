#!/usr/bin/env python3
"""Fail closed unless the isolated consumer resolves the exact data-only bus."""
import json
from pathlib import Path
import sys
import tomllib


ROOT = Path(__file__).resolve().parent.parent
WORKSPACE_VERSION = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
CONSUMER_MANIFEST = tomllib.loads(
    (ROOT / "tests/storage-needs-consumer/Cargo.toml").read_text()
)


SOURCE = (
    "git+https://github.com/tinyhumansai/tinystoragedrivers"
    "?rev=595723e1b1cabeb3d36f0c5b6ff2e11b458e7558"
    "#595723e1b1cabeb3d36f0c5b6ff2e11b458e7558"
)
REV = "595723e1b1cabeb3d36f0c5b6ff2e11b458e7558"
DEPENDENCY_SOURCE = (
    "git+https://github.com/tinyhumansai/tinystoragedrivers"
    f"?rev={REV}"
)
PACKAGE_ID = f"{DEPENDENCY_SOURCE}#tinystoragedrivers-bus@1.8.0"


def fail(message: str) -> None:
    raise SystemExit(f"TinyJuice storage-needs metadata check failed: {message}")


def main() -> None:
    if CONSUMER_MANIFEST["dependencies"]["tinyjuice-bus"]["version"] != WORKSPACE_VERSION:
        fail("the isolated consumer must pin the current TinyJuice bus contract version")
    try:
        metadata = json.load(sys.stdin)
    except (json.JSONDecodeError, UnicodeDecodeError) as error:
        fail(f"Cargo metadata was not valid JSON: {error}")

    packages = metadata.get("packages")
    resolve = metadata.get("resolve")
    if not isinstance(packages, list) or not isinstance(resolve, dict):
        fail("Cargo metadata omitted package or resolve evidence")

    root_id = resolve.get("root")
    by_id = {package.get("id"): package for package in packages if isinstance(package, dict)}
    if len(by_id) != len(packages) or None in by_id:
        fail("Cargo metadata contains a package without a unique package ID")
    root = by_id.get(root_id)
    if root is None or root.get("name") != "tinyjuice-storage-needs-consumer":
        fail("the isolated consumer root package is missing")

    bus_packages = [package for package in packages if package.get("name") == "tinyjuice-bus"]
    if len(bus_packages) != 1 or bus_packages[0].get("version") != WORKSPACE_VERSION:
        fail("the resolved TinyJuice bus contract version does not match the workspace release")

    direct = [dependency for dependency in root.get("dependencies", [])
              if dependency.get("name") == "tinystoragedrivers-bus"]
    if len(direct) != 1:
        fail("the isolated consumer must directly declare exactly one storage bus dependency")
    dependency = direct[0]
    source = dependency.get("source") or ""
    if source not in (DEPENDENCY_SOURCE, SOURCE):
        fail("the direct dependency does not pin the canonical source by the expected revision")
    if dependency.get("req") != "=1.8.0":
        fail("the direct dependency must require exactly tinystoragedrivers-bus 1.8.0")
    if dependency.get("uses_default_features") is not False:
        fail("the direct dependency must disable default features")
    if dependency.get("features") != [] or dependency.get("optional") is not False:
        fail("the direct dependency must be non-optional and must request no features")
    if dependency.get("kind") not in (None, "normal") or dependency.get("target") is not None:
        fail("the direct dependency must be unconditional and available to a normal consumer")

    matches = [package for package in packages
               if package.get("name") == "tinystoragedrivers-bus"]
    if len(matches) != 1:
        fail("Cargo metadata must resolve exactly one storage bus package")
    package = matches[0]
    if (package.get("version") != "1.8.0" or package.get("source") != SOURCE
            or package.get("id") != PACKAGE_ID):
        fail("the resolved storage bus package did not match the pinned v1.8.0 source SHA")

    nodes = resolve.get("nodes")
    if not isinstance(nodes, list):
        fail("Cargo metadata omitted the resolved dependency graph")
    nodes_by_id = {node.get("id"): node for node in nodes if isinstance(node, dict)}
    if len(nodes_by_id) != len(nodes) or None in nodes_by_id:
        fail("Cargo metadata contains a node without a unique package ID")
    root_node = nodes_by_id.get(root_id)
    bus_node = nodes_by_id.get(package.get("id"))
    if root_node is None or bus_node is None:
        fail("the consumer root or canonical storage bus is missing from the resolved graph")
    edge = next((item for item in root_node.get("deps", [])
                 if item.get("pkg") == package.get("id")), None)
    if edge is None:
        fail("the consumer root has no resolved edge to the canonical storage bus")
    kinds = edge.get("dep_kinds", [])
    if not any(item.get("kind") in (None, "normal", "build")
               for item in kinds if isinstance(item, dict)):
        fail("the canonical storage bus is only present as a dev-only dependency")
    if bus_node.get("features") != []:
        fail("the storage bus resolved an unexpected feature, including its optional TinyBus client")

    reachable = set()
    pending = [root_id]
    while pending:
        package_id = pending.pop()
        if package_id in reachable:
            continue
        if package_id not in by_id or package_id not in nodes_by_id:
            fail("the normal/build dependency closure references a missing package or node")
        reachable.add(package_id)
        dependencies = nodes_by_id[package_id].get("deps")
        if not isinstance(dependencies, list):
            fail("a package node omitted its dependency-edge evidence")
        for item in dependencies:
            if not isinstance(item, dict) or not isinstance(item.get("pkg"), str):
                fail("Cargo metadata contains an incomplete dependency edge")
            dep_kinds = item.get("dep_kinds")
            if not isinstance(dep_kinds, list) or not dep_kinds:
                fail("Cargo metadata omitted dependency-kind evidence")
            for kind in dep_kinds:
                if (not isinstance(kind, dict) or "kind" not in kind or "target" not in kind
                        or kind["kind"] not in (None, "normal", "build", "dev")):
                    fail("Cargo metadata contains incomplete dependency-kind evidence")
            if any(kind.get("kind") in (None, "normal", "build")
                   for kind in dep_kinds if isinstance(kind, dict)):
                pending.append(item["pkg"])

    tree = sys.argv[1] if len(sys.argv) == 2 else ""
    tree_packages = set()
    for line in tree.splitlines():
        fields = line.strip().split()
        if len(fields) < 2 or not fields[1].startswith("v"):
            fail("the normal/build Cargo tree contains an incomplete package row")
        tree_packages.add((fields[0], fields[1][1:]))
    if not tree_packages:
        fail("the normal/build Cargo tree is missing")
    metadata_packages = {
        (by_id[package_id]["name"], by_id[package_id]["version"])
        for package_id in reachable
    }
    if tree_packages != metadata_packages:
        fail("the normal/build Cargo tree does not match the complete metadata closure")


if __name__ == "__main__":
    main()
