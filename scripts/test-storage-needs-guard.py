"""Exercise the storage-needs CI guard with deterministic fake Cargo output."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parent.parent
CONTRACT_VERSION = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
TSD_BUS_VERSION = "1.9.0"
TSD_REV = "a04e3a174fa8f28d5229a816ca6b39c72a9efd7b"
TINYBUS_VERSION = "0.3.11"
REV = TSD_REV
SOURCE = (
    "git+https://github.com/tinyhumansai/tinystoragedrivers"
    f"?rev={REV}#{REV}"
)
DEPENDENCY_SOURCE = (
    "git+https://github.com/tinyhumansai/tinystoragedrivers"
    f"?rev={REV}"
)
# Cargo package IDs use the opaque identity after the source's single '#'; the
# resolved commit belongs in package.source, not as another package-ID fragment.
PACKAGE_ID = (
    "git+https://github.com/tinyhumansai/tinystoragedrivers"
    f"?rev={REV}#tinystoragedrivers-bus@{TSD_BUS_VERSION}"
)
ROOT_ID = "path+file:///proof#tinyjuice-storage-needs-consumer@0.1.0"
TINYJUICE_BUS_ID = f"path+file:///tinyjuice/crates/tinyjuice-bus#tinyjuice-bus@{CONTRACT_VERSION}"
TINYBUS_SDK_REV = "9e14f65666b11782ec1054aba576114026d180f8"
SERDE_ID = "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.228"


def package_id_for(source):
    source_spec = source.partition("#")[0]
    return f"{source_spec}#tinystoragedrivers-bus@{TSD_BUS_VERSION}"


def storage_bus_dependency(*, source=DEPENDENCY_SOURCE, kind=None, default_features=False):
    return {
        "name": "tinystoragedrivers-bus",
        "source": source,
        "req": f"={TSD_BUS_VERSION}",
        "kind": kind,
        "target": None,
        "optional": False,
        "uses_default_features": default_features,
        "features": [],
    }


def serde_dependency():
    return {
        "name": "serde",
        "source": "registry+https://github.com/rust-lang/crates.io-index",
        "req": "^1.0",
        "kind": None,
        "target": None,
        "optional": False,
        "uses_default_features": True,
        "features": ["derive"],
    }


def metadata_fixture(*, source=SOURCE, dependency_source=None, kind=None,
                     features=None, default_features=False, include_package=True,
                     package_id=PACKAGE_ID):
    bus_package = {
        "id": package_id,
        "name": "tinystoragedrivers-bus",
        "version": TSD_BUS_VERSION,
        "source": source,
        "dependencies": [serde_dependency()],
    }
    tinyjuice_bus = {
        "id": TINYJUICE_BUS_ID,
        "name": "tinyjuice-bus",
        "version": CONTRACT_VERSION,
        "source": None,
        "dependencies": [storage_bus_dependency(), serde_dependency()],
    }
    serde_package = {
        "id": SERDE_ID,
        "name": "serde",
        "version": "1.0.228",
        "source": "registry+https://github.com/rust-lang/crates.io-index",
        "dependencies": [],
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
            storage_bus_dependency(source=dependency_source or DEPENDENCY_SOURCE, kind=kind,
                                   default_features=default_features),
        ],
    }
    bus_node = {
        "id": package_id,
        "features": features or [],
        "deps": [{"pkg": SERDE_ID, "dep_kinds": [{"kind": None, "target": None}]}],
    }
    tinyjuice_bus_node = {
        "id": TINYJUICE_BUS_ID,
        "deps": [
            {"pkg": package_id, "dep_kinds": [{"kind": None, "target": None}]},
            {"pkg": SERDE_ID, "dep_kinds": [{"kind": None, "target": None}]},
        ],
    }
    serde_node = {"id": SERDE_ID, "deps": []}
    root_node = {
        "id": ROOT_ID,
        "deps": [
            {"pkg": TINYJUICE_BUS_ID, "dep_kinds": [{"kind": None, "target": None}]},
            {"pkg": package_id, "dep_kinds": [{"kind": kind, "target": None}]},
        ],
    }
    packages = [root, tinyjuice_bus, serde_package] + ([bus_package] if include_package else [])
    nodes = [root_node, tinyjuice_bus_node, serde_node] + ([bus_node] if include_package else [])
    return {"packages": packages, "resolve": {"root": ROOT_ID, "nodes": nodes}}


class StorageNeedsGuardTests(unittest.TestCase):
    def test_executable_schema_and_migration_are_owned_by_tinyjuice_drivers(self):
        bus_storage = ROOT / "crates/tinyjuice-bus/src/storage.rs"
        bus_source = bus_storage.read_text()
        self.assertNotIn("storage_schema", bus_source)
        self.assertNotIn("Step::blobs", bus_source)
        self.assertFalse((ROOT / "crates/tinyjuice-bus/src/storage_tests.rs").exists())
        self.assertIn("Schema::new", (ROOT / "crates/tinyjuice-drivers/src/storage.rs").read_text())
        self.assertTrue((ROOT / "crates/tinyjuice-drivers/src/storage_tests.rs").exists())
        driver = tomllib.loads((ROOT / "crates/tinyjuice-drivers/Cargo.toml").read_text())
        self.assertIn("tinystoragedrivers-core", driver["dependencies"])

    def test_bus_has_no_tsd_core_dependency_under_any_feature(self):
        manifest = tomllib.loads((ROOT / "crates/tinyjuice-bus/Cargo.toml").read_text())
        self.assertNotIn("tinystoragedrivers-core", manifest.get("dependencies", {}))
        self.assertNotIn("tinystoragedrivers-core", manifest.get("dev-dependencies", {}))
        self.assertEqual(manifest.get("features", {}).get("storage"), [])
        self.assertIn("pub mod storage;", (ROOT / "crates/tinyjuice-bus/src/lib.rs").read_text())

    def test_ci_guard_job_checks_out_recursive_submodules_without_credentials(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        checkout = re.search(
            r"(?ms)^      - uses: actions/checkout@[^\n]+\n        with:\n((?:          [^\n]*\n)+)",
            workflow,
        )
        self.assertIsNotNone(checkout, "CI guard checkout step was not found")
        options = checkout.group(1)
        self.assertRegex(options, r"(?m)^          submodules: recursive$")
        self.assertRegex(options, r"(?m)^          persist-credentials: false$")

    def test_release_manifests_pin_one_data_only_storage_schema_and_sdk(self):
        root_manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
        consumer_manifest = tomllib.loads(
            (ROOT / "tests/storage-needs-consumer/Cargo.toml").read_text()
        )
        root_pin = root_manifest["workspace"]["dependencies"]["tinystoragedrivers-bus"]
        consumer_pin = consumer_manifest["dependencies"]["tinystoragedrivers-bus"]
        self.assertEqual(root_pin, consumer_pin)
        self.assertEqual(root_pin, {
            "version": f"={TSD_BUS_VERSION}",
            "git": "https://github.com/tinyhumansai/tinystoragedrivers",
            "rev": TSD_REV,
            "default-features": False,
        })

        self.assertEqual(root_manifest["package"]["version"], "0.11.0")
        self.assertEqual(root_manifest["dependencies"]["tinyjuice-bus"]["version"], CONTRACT_VERSION)
        self.assertEqual(consumer_manifest["dependencies"]["tinyjuice-bus"]["version"], CONTRACT_VERSION)
        examples_manifest = tomllib.loads((ROOT / "crates/tinyjuice-examples/Cargo.toml").read_text())
        self.assertEqual(examples_manifest["dependencies"]["tinyjuice"]["version"], CONTRACT_VERSION)
        for relative in ("crates/tinyjuice-bus/Cargo.toml", "crates/tinyjuice-module/Cargo.toml"):
            manifest = tomllib.loads((ROOT / relative).read_text())
            self.assertEqual(manifest["package"]["version"], CONTRACT_VERSION)

        module_manifest = tomllib.loads((ROOT / "crates/tinyjuice-module/Cargo.toml").read_text())
        self.assertEqual(module_manifest["dependencies"]["tinyjuice"]["version"], CONTRACT_VERSION)
        self.assertEqual(module_manifest["dependencies"]["tinyjuice-bus"]["version"], CONTRACT_VERSION)
        self.assertEqual(module_manifest["dependencies"]["tinybus"]["version"], f"={TINYBUS_VERSION}")
        self.assertEqual(module_manifest["dependencies"]["tinybus-module"]["version"], f"={TINYBUS_VERSION}")
        self.assertEqual(module_manifest["dev-dependencies"]["tinybus"]["version"], f"={TINYBUS_VERSION}")
        driver_manifest = tomllib.loads((ROOT / "crates/tinyjuice-drivers/Cargo.toml").read_text())
        self.assertNotIn("version", driver_manifest["dependencies"]["tinyjuice"])
        self.assertNotIn("version", driver_manifest["dependencies"]["tinyjuice-bus"])
        runtime_pin = {
            "version": f"={TSD_BUS_VERSION}",
            "git": "https://github.com/tinyhumansai/tinystoragedrivers",
            "rev": REV,
        }
        driver_core = driver_manifest["dependencies"]["tinystoragedrivers-core"]
        self.assertEqual({key: driver_core[key] for key in runtime_pin}, runtime_pin)
        self.assertEqual(driver_core["features"], ["blocking"])
        for section, name in (
            ("dependencies", "tinystoragedrivers-core"),
            ("dependencies", "tinystoragedrivers-busproxy"),
            ("dependencies", "tinystoragedrivers-client"),
        ):
            actual = module_manifest[section][name]
            self.assertEqual({key: actual[key] for key in runtime_pin}, runtime_pin)
        sdk_gitlink = subprocess.run(
            ["git", "-C", str(ROOT), "ls-files", "--stage", "--", "vendor/tinybus"],
            check=True,
            text=True,
            capture_output=True,
        ).stdout.strip()
        self.assertEqual(
            sdk_gitlink,
            f"160000 {TINYBUS_SDK_REV} 0\tvendor/tinybus",
            "TinyBus SDK gitlink must pin the verified 0.3.11 source, including in a nonrecursive checkout",
        )
        self.assertEqual(root_manifest["patch"]["https://github.com/tinyhumansai/tinybus"]["tinybus"]["path"],
                         "vendor/tinybus/crates/tinybus")
        self.assertEqual(
            root_manifest["patch"]["https://github.com/tinyhumansai/tinybus"]["tinybus-module"]["path"],
            "vendor/tinybus/crates/tinybus-module",
        )
        self.assertEqual(root_manifest["patch"]["crates-io"]["tinybus"]["path"],
                         "vendor/tinybus/crates/tinybus")
        self.assertNotIn("tinybus-module", root_manifest["patch"]["crates-io"])

    def test_stale_tinyjuice_bus_contract_version_fails_closed(self):
        metadata = metadata_fixture()
        bus = next(package for package in metadata["packages"] if package["id"] == TINYJUICE_BUS_ID)
        bus["version"] = "0.10.10"
        result = self.run_guard(metadata)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("resolved TinyJuice bus contract version", result.stderr)

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
                                   f"tinyjuice-bus v{CONTRACT_VERSION} (/tinyjuice/crates/tinyjuice-bus)\n"
                                   "serde v1.0.228 (registry)\n"
                                   f"tinystoragedrivers-bus v{TSD_BUS_VERSION} (canonical source)\n",
                                   encoding="utf-8")
            feature_path = temp / "features.txt"
            feature_path.write_text(feature_graph if feature_graph is not None else
                                    f"tinystoragedrivers-bus v{TSD_BUS_VERSION} (canonical source)\n",
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

    def test_historical_1_8_storage_bus_graph_fails_closed(self):
        result = self.run_guard(
            metadata_fixture(),
            normal_graph=(
                "tinyjuice-storage-needs-consumer v0.1.0\n"
                f"tinyjuice-bus v{CONTRACT_VERSION} (/tinyjuice/crates/tinyjuice-bus)\n"
                "serde v1.0.228 (registry)\n"
                "tinystoragedrivers-bus v1.8.0 (historical source)\n"
            ),
            feature_graph="tinystoragedrivers-bus v1.8.0 (historical source)\n",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"missing tinystoragedrivers-bus {TSD_BUS_VERSION}", result.stderr)

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

        missing_transitive_node = metadata_fixture()
        missing_transitive_node["resolve"]["nodes"] = [
            node for node in missing_transitive_node["resolve"]["nodes"]
            if node["id"] != SERDE_ID
        ]
        cases.append(("transitive node", missing_transitive_node, None))

        missing_edge = metadata_fixture()
        missing_edge["resolve"]["nodes"][0]["deps"] = []
        cases.append(("direct edge", missing_edge, None))

        missing_edge_kind = metadata_fixture()
        missing_edge_kind["resolve"]["nodes"][0]["deps"][0]["dep_kinds"] = []
        cases.append(("edge kind", missing_edge_kind, None))

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
            f"?tag=v1.8.0#{REV}"
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
            feature_graph=(
                f'tinystoragedrivers-bus v{TSD_BUS_VERSION}\n'
                'tinystoragedrivers-bus feature "client"\n'
            ),
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
                f"tinyjuice-bus v{CONTRACT_VERSION} (/tinyjuice/crates/tinyjuice-bus)\n"
                "serde v1.0.228 (registry)\n"
                f"tinystoragedrivers-bus v{TSD_BUS_VERSION}\ntinybus v{TINYBUS_VERSION}\n"
            ),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpectedly depends on tinybus", result.stderr)


if __name__ == "__main__":
    unittest.main()
