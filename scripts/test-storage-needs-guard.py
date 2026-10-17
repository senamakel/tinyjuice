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
PACKAGE_ID = f"{SOURCE}#tinystoragedrivers-bus@1.5.0"
ROOT_ID = "path+file:///proof#tinyjuice-storage-needs-consumer@0.1.0"


def metadata_fixture(*, source=SOURCE, dependency_source=None, kind=None,
                     features=None, include_package=True):
    bus_package = {
        "id": PACKAGE_ID,
        "name": "tinystoragedrivers-bus",
        "version": "1.5.0",
        "source": source,
        "dependencies": [],
    }
    root = {
        "id": ROOT_ID,
        "name": "tinyjuice-storage-needs-consumer",
        "version": "0.1.0",
        "dependencies": [{
            "name": "tinystoragedrivers-bus",
            "source": dependency_source or SOURCE,
            "req": "=1.5.0",
            "kind": kind,
            "target": None,
            "optional": False,
            "uses_default_features": False,
            "features": [],
        }],
    }
    bus_node = {"id": PACKAGE_ID, "features": features or [], "deps": []}
    root_node = {
        "id": ROOT_ID,
        "deps": [{
            "pkg": PACKAGE_ID,
            "dep_kinds": [{"kind": kind, "target": None}],
        }],
    }
    packages = [root] + ([bus_package] if include_package else [])
    nodes = [root_node] + ([bus_node] if include_package else [])
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
            metadata_path = temp / "metadata.json"
            metadata_path.write_text(json.dumps(metadata), encoding="utf-8")
            normal_path = temp / "normal.txt"
            normal_path.write_text(normal_graph if normal_graph is not None else
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
        result = self.run_guard(metadata_fixture(include_package=False), normal_graph="")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("exactly one storage bus package", result.stderr)

    def test_wrong_source_revision_or_tag_fails_closed(self):
        for source in (
            SOURCE.replace(REV, "08caa16d1d4e3c6b8d649e4f26fe1a426ee1d168"),
            SOURCE.replace(f"?rev={REV}", "?tag=v1.5.0"),
        ):
            with self.subTest(source=source):
                result = self.run_guard(metadata_fixture(source=source, dependency_source=source))
                self.assertNotEqual(result.returncode, 0)

    def test_client_feature_is_rejected_in_metadata_or_feature_graph(self):
        result = self.run_guard(metadata_fixture(features=["client"]))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("optional storage TinyBus client", result.stderr)
        result = self.run_guard(
            metadata_fixture(),
            feature_graph='tinystoragedrivers-bus v1.5.0\ntinystoragedrivers-bus feature "client"\n',
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("optional storage TinyBus client", result.stderr)

    def test_dev_only_dependency_does_not_satisfy_normal_graph_proof(self):
        result = self.run_guard(metadata_fixture(kind="dev"), normal_graph="")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("normal consumer", result.stderr)

    def test_forbidden_build_dependency_is_rejected(self):
        result = self.run_guard(
            metadata_fixture(),
            normal_graph="tinystoragedrivers-bus v1.5.0\ntinybus v0.1.7\n",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpectedly depends on tinybus", result.stderr)


if __name__ == "__main__":
    unittest.main()
