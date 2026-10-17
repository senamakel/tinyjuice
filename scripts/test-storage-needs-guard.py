"""Exercise the storage-needs CI guard with deterministic fake Cargo output."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parent.parent
REV = "c9ea6ff1615b77b55d7269403af5fd11bedc9ce8"
SOURCE = (
    "git+https://github.com/tinyhumansai/tinystoragedrivers"
    f"?rev={REV}#{REV}"
)
# Cargo package IDs use the opaque identity after the source's single '#'; the
# resolved commit belongs in package.source, not as another package-ID fragment.
PACKAGE_ID = (
    "git+https://github.com/tinyhumansai/tinystoragedrivers"
    f"?rev={REV}#tinystoragedrivers-bus@1.5.0"
)
ROOT_ID = "path+file:///proof#tinyjuice-storage-needs-consumer@0.1.0"
TINYJUICE_BUS_ID = "path+file:///tinyjuice/crates/tinyjuice-bus#tinyjuice-bus@0.10.6"


def package_id_for(source):
    source_spec = source.partition("#")[0]
    return f"{source_spec}#tinystoragedrivers-bus@1.5.0"


def storage_bus_dependency(*, source=SOURCE, kind=None, default_features=False):
    return {
        "name": "tinystoragedrivers-bus",
        "source": source,
        "req": "=1.5.0",
        "kind": kind,
        "target": None,
        "optional": False,
        "uses_default_features": default_features,
        "features": [],
    }


def metadata_fixture(*, source=SOURCE, dependency_source=None, kind=None,
                     features=None, default_features=False, include_package=True,
                     package_id=PACKAGE_ID):
    bus_package = {
        "id": package_id,
        "name": "tinystoragedrivers-bus",
        "version": "1.5.0",
        "source": source,
        "dependencies": [],
    }
    tinyjuice_bus = {
        "id": TINYJUICE_BUS_ID,
        "name": "tinyjuice-bus",
        "version": "0.10.6",
        "source": None,
        "dependencies": [storage_bus_dependency()],
    }
    root = {
        "id": ROOT_ID,
        "name": "tinyjuice-storage-needs-consumer",
        "version": "0.1.0",
        "dependencies": [
            {
                "name": "tinyjuice-bus",
                "source": None,
                "req": "*",
                "kind": None,
                "target": None,
                "optional": False,
                "uses_default_features": True,
                "features": [],
            },
            storage_bus_dependency(source=dependency_source or SOURCE, kind=kind,
                                   default_features=default_features),
        ],
    }
    bus_node = {"id": package_id, "features": features or [], "deps": []}
    tinyjuice_bus_node = {
        "id": TINYJUICE_BUS_ID,
        "deps": [{"pkg": PACKAGE_ID, "dep_kinds": [{"kind": None, "target": None}]}],
    }
    root_node = {
        "id": ROOT_ID,
        "deps": [
            {"pkg": TINYJUICE_BUS_ID, "dep_kinds": [{"kind": None, "target": None}]},
            {"pkg": package_id, "dep_kinds": [{"kind": kind, "target": None}]},
        ],
    }
    packages = [root, tinyjuice_bus] + ([bus_package] if include_package else [])
    nodes = [root_node, tinyjuice_bus_node] + ([bus_node] if include_package else [])
    return {"packages": packages, "resolve": {"root": ROOT_ID, "nodes": nodes}}


class StorageNeedsGuardTests(unittest.TestCase):
    def test_release_manifests_pin_one_data_only_storage_schema_and_sdk(self):
        root_manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
        consumer_manifest = tomllib.loads(
            (ROOT / "tests/storage-needs-consumer/Cargo.toml").read_text()
        )
        root_pin = root_manifest["workspace"]["dependencies"]["tinystoragedrivers-bus"]
        consumer_pin = consumer_manifest["dependencies"]["tinystoragedrivers-bus"]
        self.assertEqual(root_pin, consumer_pin)
        self.assertEqual(root_pin, {
            "version": "=1.5.0",
            "git": "https://github.com/tinyhumansai/tinystoragedrivers",
            "rev": REV,
            "default-features": False,
        })

        self.assertEqual(root_manifest["package"]["version"], "0.10.6")
        self.assertEqual(root_manifest["dependencies"]["tinyjuice-bus"]["version"], "0.10.6")
        for relative in ("crates/tinyjuice-bus/Cargo.toml", "crates/tinyjuice-module/Cargo.toml"):
            manifest = tomllib.loads((ROOT / relative).read_text())
            self.assertEqual(manifest["package"]["version"], "0.10.6")

        module_manifest = tomllib.loads((ROOT / "crates/tinyjuice-module/Cargo.toml").read_text())
        self.assertEqual(module_manifest["dependencies"]["tinybus"]["version"], "=0.1.7")
        self.assertEqual(module_manifest["dependencies"]["tinybus-module"], "=0.1.7")
        self.assertEqual(module_manifest["dev-dependencies"]["tinybus"]["version"], "=0.1.7")
        self.assertEqual(root_manifest["patch"]["https://github.com/tinyhumansai/tinybus"]["tinybus"]["path"],
                         "vendor/tinybus/crates/tinybus")
        self.assertEqual(root_manifest["patch"]["crates-io"]["tinybus"]["path"],
                         "vendor/tinybus/crates/tinybus")
        self.assertEqual(root_manifest["patch"]["crates-io"]["tinybus-module"]["path"],
                         "vendor/tinybus/crates/tinybus-module")

    def run_guard(self, metadata, normal_graph=None, feature_graph=None):
        with tempfile.TemporaryDirectory() as directory:
            temp = Path(directory)
            cargo = temp / "cargo"
            cargo.write_text("""#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
if args and args[0] == "metadata":
    print(open(os.environ["FAKE_METADATA"], encoding="utf-8").read())
elif args and args[0] == "tree":
    print(open(os.environ["FAKE_FEATURE_GRAPH" if "features" in args else "FAKE_NORMAL_GRAPH"], encoding="utf-8").read())
elif args and args[0] == "test":
    pass
else:
    raise SystemExit("unexpected fake cargo invocation: " + " ".join(args))
""")
            cargo.chmod(0o755)
            rustc = temp / "rustc"
            rustc.write_text("#!/usr/bin/env bash\nprintf 'host: x86_64-unknown-linux-gnu\\n'\n")
            rustc.chmod(0o755)
            metadata_path = temp / "metadata.json"
            metadata_path.write_text(json.dumps(metadata), encoding="utf-8")
            normal_path = temp / "normal.txt"
            normal_path.write_text(normal_graph if normal_graph is not None else
                                   "tinyjuice-storage-needs-consumer v0.1.0\n"
                                   "tinyjuice-bus v0.10.6 (/tinyjuice/crates/tinyjuice-bus)\n"
                                   "tinystoragedrivers-bus v1.5.0 (canonical source)\n",
                                   encoding="utf-8")
            feature_path = temp / "features.txt"
            feature_path.write_text(feature_graph if feature_graph is not None else
                                    "tinystoragedrivers-bus v1.5.0 (canonical source)\n",
                                    encoding="utf-8")
            env = dict(os.environ)
            env.update({
                "PATH": f"{temp}:{env['PATH']}",
                "FAKE_METADATA": str(metadata_path),
                "FAKE_NORMAL_GRAPH": str(normal_path),
                "FAKE_FEATURE_GRAPH": str(feature_path),
            })
            return subprocess.run(["bash", str(ROOT / "scripts/test-storage-needs.sh")],
                                  cwd=temp, env=env, text=True, capture_output=True)

    def test_exact_canonical_direct_normal_dependency_passes(self):
        result = self.run_guard(metadata_fixture())
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_resolved_package_fails_closed(self):
        result = self.run_guard(metadata_fixture(include_package=False))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("exactly one storage bus package", result.stderr)

    def test_missing_root_node_bus_node_edge_or_tree_evidence_fails_closed(self):
        cases = []
        missing_root = metadata_fixture()
        missing_root["packages"] = [package for package in missing_root["packages"]
                                     if package["id"] != ROOT_ID]
        cases.append(("root package", missing_root, None))

        missing_root_node = metadata_fixture()
        missing_root_node["resolve"]["nodes"] = [
            node for node in missing_root_node["resolve"]["nodes"] if node["id"] != ROOT_ID
        ]
        cases.append(("root node", missing_root_node, None))

        missing_bus_node = metadata_fixture()
        missing_bus_node["resolve"]["nodes"] = [
            node for node in missing_bus_node["resolve"]["nodes"] if node["id"] != PACKAGE_ID
        ]
        cases.append(("storage bus node", missing_bus_node, None))

        missing_contract_node = metadata_fixture()
        missing_contract_node["resolve"]["nodes"] = [
            node for node in missing_contract_node["resolve"]["nodes"]
            if node["id"] != TINYJUICE_BUS_ID
        ]
        cases.append(("TinyJuice bus node", missing_contract_node, None))

        missing_edge = metadata_fixture()
        missing_edge["resolve"]["nodes"][0]["deps"] = []
        cases.append(("direct edge", missing_edge, None))

        for label, metadata, normal_graph in cases:
            with self.subTest(label=label):
                result = self.run_guard(metadata, normal_graph=normal_graph)
                self.assertNotEqual(result.returncode, 0)
        result = self.run_guard(metadata_fixture(), normal_graph="")
        self.assertNotEqual(result.returncode, 0)

    def test_wrong_source_revision_or_tag_fails_closed(self):
        wrong_revision = "08caa16d1d4e3c6b8d649e4f26fe1a426ee1d168"
        wrong_revision_source = (
            "git+https://github.com/tinyhumansai/tinystoragedrivers"
            f"?rev={wrong_revision}#{wrong_revision}"
        )
        tag_source = (
            "git+https://github.com/tinyhumansai/tinystoragedrivers"
            f"?tag=v1.5.0#{REV}"
        )
        for dependency_source, resolved_source in (
            (wrong_revision_source, wrong_revision_source),
            (tag_source, tag_source),
            (SOURCE, wrong_revision_source),
        ):
            with self.subTest(source=dependency_source, resolved=resolved_source):
                result = self.run_guard(metadata_fixture(
                    source=resolved_source,
                    dependency_source=dependency_source,
                    package_id=package_id_for(resolved_source),
                ))
                self.assertNotEqual(result.returncode, 0)

    def test_client_feature_is_rejected_in_metadata_or_feature_graph(self):
        result = self.run_guard(metadata_fixture(features=["client"]))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpected feature", result.stderr)
        result = self.run_guard(
            metadata_fixture(),
            feature_graph='tinystoragedrivers-bus v1.5.0\ntinystoragedrivers-bus feature "client"\n',
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("optional storage TinyBus client", result.stderr)

    def test_dev_only_dependency_does_not_satisfy_normal_graph_proof(self):
        result = self.run_guard(metadata_fixture(kind="dev"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("normal consumer", result.stderr)

    def test_default_features_cannot_silently_enable_the_optional_client(self):
        result = self.run_guard(metadata_fixture(default_features=True))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("disable default features", result.stderr)

    def test_forbidden_build_dependency_is_rejected(self):
        result = self.run_guard(
            metadata_fixture(),
            normal_graph=(
                "tinyjuice-storage-needs-consumer v0.1.0\n"
                "tinyjuice-bus v0.10.6 (/tinyjuice/crates/tinyjuice-bus)\n"
                "tinystoragedrivers-bus v1.5.0\ntinybus v0.1.7\n"
            ),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpectedly depends on tinybus", result.stderr)


if __name__ == "__main__":
    unittest.main()
