"""CI selects from frozen trusted-base code and a matching runner recipe."""
import hashlib
import json
import pathlib
import subprocess
import unittest
import ast
import shlex

import test_delivery_plan_git as git_tests

ADAPTER = pathlib.Path(__file__).with_name("delivery-ci.py")


class CIPlan(git_tests.GitPlan):
    def test_cached_runner_recipe_population_equals_the_image_stamp_inputs(self):
        tree = ast.parse(ADAPTER.read_text())
        definitions = [node.value for node in tree.body if isinstance(node,ast.Assign)
                       and any(isinstance(target,ast.Name) and target.id == "RECIPE_FILES" for target in node.targets)]
        self.assertEqual(len(definitions),1)
        recipe = ast.literal_eval(definitions[0])
        source = (ADAPTER.parent / "boss-ci/Dockerfile").read_text()
        copies = [line for line in source.splitlines() if line.startswith("COPY ") and line.endswith(" /tmp/stamp/")]
        self.assertEqual(len(copies),1)
        copied = shlex.split(copies[0])[1:-1]
        cats = [line for line in source.splitlines() if line.startswith("RUN cat /tmp/stamp/")]
        self.assertEqual(len(cats),1)
        hashed = [path.removeprefix("/tmp/stamp/") for path in shlex.split(cats[0].removesuffix("\\").strip())[2:]]
        self.assertTrue(recipe)
        self.assertEqual(list(recipe),copied,"cached-runner source inputs drifted from Docker COPY")
        self.assertEqual(list(recipe),hashed,"cached-runner recipe hash drifted from the actual image stamp")

    def setUp(self):
        super().setUp()
        self.write("infra/forge/delivery-plan.py", pathlib.Path(__file__).with_name("delivery-plan.py").read_text())
        self.write("infra/forge/boss-ci/Dockerfile", "FROM trusted\n")
        self.write("infra/forge/boss-ci/required-tools.txt", "git\npython3\n")
        self.base = self.commit()
        self.write("docs/original.md", "changed\n")
        self.head = self.commit()
        self.stamp = self.repo / ".git/test-runner-stamp"
        self.stamp.write_text(hashlib.sha256(b"FROM trusted\ngit\npython3\n").hexdigest() + "\n")

    def ci(self, base=None):
        out = subprocess.run(["python3", str(ADAPTER), "--repo", str(self.repo),
                              "--base", base or self.base, "--head", self.head,
                              "--stamp", str(self.stamp)], capture_output=True, text=True)
        self.assertEqual(out.returncode, 0, out.stderr)
        return json.loads(out.stdout)

    def test_matching_recipe_permits_frozen_docs_validation_without_image_build(self):
        self.assertEqual(self.ci()["route"], "data")

    def test_stale_or_missing_recipe_and_unreadable_base_keep_image_build(self):
        self.stamp.write_text("stale\n")
        stale = self.ci()
        self.assertEqual(stale["route"], "software")
        self.assertEqual(stale["candidate_plan"]["head"],self.head)
        self.assertTrue(stale["candidate_plan"]["source_facts"]["changes"])
        self.stamp.unlink()
        self.assertEqual(self.ci()["route"], "software")
        self.assertEqual(self.ci(base="missing")["route"], "software")

    def test_ci_requires_the_callers_full_frozen_identities_before_loading_trusted_code(self):
        self.assertEqual(self.ci(base=self.base[:7])["route"],"software")

    def test_changed_planner_or_recipe_cannot_grant_itself_a_skip(self):
        for path in ["infra/forge/delivery-plan.py", "infra/forge/boss-ci/Dockerfile"]:
            self.write(path, "untrusted replacement\n")
            self.head = self.commit()
            self.assertEqual(self.ci()["route"], "software")


if __name__ == "__main__":
    unittest.main()
