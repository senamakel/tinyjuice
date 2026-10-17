#!/usr/bin/env python3
"""Fail closed unless the isolated consumer resolves the exact data-only bus."""
import json
import sys


SOURCE = (
    "git+https://github.com/tinyhumansai/tinystoragedrivers"
    "?rev=c9ea6ff1615b77b55d7269403af5fd11bedc9ce8"
    "#c9ea6ff1615b77b55d7269403af5fd11bedc9ce8"
)
REV = "c9ea6ff1615b77b55d7269403af5fd11bedc9ce8"


def fail(message: str) -> None:
    raise SystemExit(f"TinyJuice storage-needs metadata check failed: {message}")


def main() -> None:
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
    root = by_id.get(root_id)
    if root is None or root.get("name") != "tinyjuice-storage-needs-consumer":
        fail("the isolated consumer root package is missing")

    direct = [dependency for dependency in root.get("dependencies", [])
              if dependency.get("name") == "tinystoragedrivers-bus"]
    if len(direct) != 1:
        fail("the isolated consumer must directly declare exactly one storage bus dependency")
    dependency = direct[0]
    source = dependency.get("source") or ""
    if not source.startswith(f"git+https://github.com/tinyhumansai/tinystoragedrivers?rev={REV}"):
        fail("the direct dependency does not pin the canonical source by the expected revision")
    fragment = source.partition("#")[2]
    if fragment and fragment != REV:
        fail("the direct dependency source resolved to a different commit")
    if dependency.get("req") != "=1.5.0":
        fail("the direct dependency must require exactly tinystoragedrivers-bus 1.5.0")
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
    package_id = f"{SOURCE}#tinystoragedrivers-bus@1.5.0"
    if (package.get("version") != "1.5.0" or package.get("source") != SOURCE
            or package.get("id") != package_id):
        fail("the resolved storage bus package did not match the canonical 1.5.0 source SHA")

    nodes = resolve.get("nodes")
    if not isinstance(nodes, list):
        fail("Cargo metadata omitted the resolved dependency graph")
    nodes_by_id = {node.get("id"): node for node in nodes if isinstance(node, dict)}
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


if __name__ == "__main__":
    main()
