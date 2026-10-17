"""Exercise routing and input guards in the GitHub release workflow."""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github/workflows/release.yml"


def resolve_script(workflow):
    match = re.search(r"(?ms)^      - id: tag\n.*?^        run: \|\n((?:^          .*\n|^\n)+)", workflow)
    if not match:
        raise AssertionError("release resolve script was not found")
    return "\n".join(line[10:] if line.startswith("          ") else "" for line in match[1].splitlines())


def job_block(workflow, name):
    match = re.search(rf"(?ms)^  {re.escape(name)}:\n(.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)", workflow)
    if not match:
        raise AssertionError(f"release job {name} was not found")
    return match[1]


def job_permissions(block):
    match = re.search(r"(?ms)^    permissions:\n((?:^      [a-z-]+: [a-z]+\n)+)", block)
    if not match:
        raise AssertionError("job permissions were not found")
    return dict(re.findall(r"(?m)^      ([a-z-]+): ([a-z]+)$", match[1]))


class ReleaseWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = WORKFLOW.read_text()
        cls.script = resolve_script(cls.workflow)

    def run_resolve(self, mode="main", reviewed_pr=""):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            env = dict(os.environ, TAG="v1.2.3", RELEASE_MODE=mode,
                       REVIEWED_PR=reviewed_pr, GITHUB_OUTPUT=str(output))
            result = subprocess.run(["bash", "-c", self.script], env=env,
                                    text=True, capture_output=True)
            return result, output.read_text() if output.exists() else ""

    def test_manual_default_and_explicit_main_route_to_existing_release(self):
        self.assertIn("default: main", self.workflow)
        result, output = self.run_resolve("main")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("release_mode=main", output)
        self.assertIn("reviewed_pr=0", output)
        self.assertIn("github.event_name == 'push' && 'main' || inputs.release_mode", self.workflow)
        self.assertIn("github.event_name == 'push' && github.ref_name || inputs.tag", self.workflow)

    def test_reviewed_epic_requires_and_forwards_positive_integer_pr(self):
        result, output = self.run_resolve("reviewed-epic", "91")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("reviewed_pr=91", output)

    def test_reviewed_epic_rejects_missing_zero_and_non_integer_prs(self):
        for value in ("", "0", "-1", "1.5", "abc"):
            with self.subTest(value=value):
                result, _ = self.run_resolve("reviewed-epic", value)
                self.assertEqual(result.returncode, 2)
                self.assertIn("reviewed_pr must be a positive integer", result.stderr)

    def test_ordinary_path_rejects_reviewed_pr_and_uses_fixed_profiles(self):
        result, _ = self.run_resolve("main", "91")
        self.assertEqual(result.returncode, 2)
        self.assertIn("reviewed_pr is only valid", result.stderr)
        self.assertIn("module-crate: tinyjuice-module", self.workflow)
        self.assertIn("reviewed-pr: ${{ fromJSON(needs.resolve.outputs.reviewed_pr) }}", self.workflow)
        self.assertNotIn("inputs.package", self.workflow)
        self.assertNotIn("inputs.repo", self.workflow)

    def test_every_shared_workflow_uses_a_full_commit_pin(self):
        refs = re.findall(
            r"(?m)^\s+uses: tinyhumansai/\.github/\.github/workflows/[^@\s]+@([0-9a-f]+)\s*$",
            self.workflow,
        )
        self.assertGreaterEqual(len(refs), 2)
        self.assertTrue(all(len(ref) == 40 for ref in refs), refs)
        reviewed = job_block(self.workflow, "module-reviewed-epic-release")
        self.assertRegex(
            reviewed,
            r"(?m)^    uses: tinyhumansai/\.github/\.github/workflows/module-reviewed-epic-release\.yml@837d0424c7f38e4293f41c795f12ed5eb5dd7280$",
        )

    def test_release_routes_are_exclusive_and_permissions_are_minimal(self):
        ordinary = job_block(self.workflow, "module-release")
        reviewed = job_block(self.workflow, "module-reviewed-epic-release")
        self.assertRegex(ordinary, r"(?m)^    if: needs\.resolve\.outputs\.release_mode == 'main'$")
        self.assertRegex(reviewed, r"(?m)^    if: needs\.resolve\.outputs\.release_mode == 'reviewed-epic'$")
        self.assertEqual(job_permissions(ordinary), {"contents": "write"})
        self.assertEqual(job_permissions(reviewed), {
            "contents": "write",
            "pull-requests": "read",
            "checks": "read",
            "actions": "read",
        })


if __name__ == "__main__":
    unittest.main()
