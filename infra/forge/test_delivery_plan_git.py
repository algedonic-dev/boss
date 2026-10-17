"""Frozen real-git input, including deletes, renames and floating branches."""
import json
import hashlib
import pathlib
import subprocess
import tempfile
import unittest

DRIVER = pathlib.Path(__file__).with_name("delivery-plan-git.py")
POLICY = "infra/platform/delivery-policy/train-conductor.toml"


class GitPlan(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="delivery-plan-test-")
        self.addCleanup(self.directory.cleanup)
        self.repo = pathlib.Path(self.directory.name)
        self.git("init", "-q")
        self.git("config", "user.email", "delivery-test@example.invalid")
        self.git("config", "user.name", "delivery test")
        self.write(POLICY, "[[delivery_policy]]\nname='train-conductor'\nversion=8\n[delivery_policy.channels]\nschema=1\nenabled=true\n[[delivery_policy.channels.rules]]\nprefix='docs/'\nsuffix='.md'\nevidence='forge_blob'\n")
        self.write("docs/original.md", "original\n")
        self.base = self.commit()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), *args], text=True).strip()

    def write(self, path, text):
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def commit(self):
        self.git("add", "-A")
        self.git("commit", "-qm", "test frozen facts")
        return self.git("rev-parse", "HEAD")

    def plan(self, head, base=None):
        result = subprocess.run(["python3", str(DRIVER), "--repo", str(self.repo),
                                 "--base", base or self.base, "--head", head],
                                text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_exact_original_frozen_refs_survive_branch_movement(self):
        self.write("docs/original.md", "published\n")
        frozen = self.commit()
        first = self.plan(frozen)
        self.assertEqual(first["route"], "data")
        self.assertEqual(first["head"], frozen)
        self.write("crates/actual.rs", "software\n")
        moved = self.commit()
        self.assertEqual(self.plan(frozen), first)
        self.assertEqual(self.plan(moved)["route"], "software")

    def test_deleted_and_renamed_files_are_all_conserved(self):
        (self.repo / "docs/original.md").rename(self.repo / "docs/renamed.md")
        result = self.plan(self.commit())
        self.assertEqual(result["route"], "data")
        obligations = {r["path"]: r for r in result["obligations"]}
        self.assertEqual(set(obligations), {"docs/original.md", "docs/renamed.md"})
        self.assertEqual(obligations["docs/original.md"]["new_mode"], "000000")
        self.assertEqual(obligations["docs/renamed.md"]["old_mode"], "000000")

    def test_missing_branch_unknown_ancestor_and_mutated_policy_are_heavy(self):
        self.assertEqual(self.plan("not-present")["route"], "software")
        self.write(POLICY, "[[delivery_policy]]\nname='train-conductor'\nversion=999\n")
        self.assertEqual(self.plan(self.commit())["route"], "software")
        self.assertEqual(self.plan(self.base, base="a" * 40)["route"], "software")

    def test_registry_behavioral_data_requires_real_publisher_equality_not_an_image(self):
        policy = (self.repo / POLICY).read_text()
        policy += "\n[[delivery_policy.channels.rules]]\nprefix='infra/platform/workflows/'\nsuffix='.toml'\nevidence='workflow_equality'\nprotected_names_from='infra/platform/workflow-holds/'\nprotected_keys=['authority']\n"
        self.write(POLICY, policy)
        workflow = "infra/platform/workflows/learning.toml"
        original = "[[workflow]]\nkind='learning'\ndescription='before'\n[[workflow.step]]\ntitle='read'\nready_when='true'\n[workflow.authority]\nrole='reader'\n"
        self.write(workflow, original)
        self.base = self.commit()
        self.write(workflow, original.replace("before", "after"))
        self.assertEqual(self.plan(self.commit())["route"], "data")
        self.write(workflow, original.replace("reader", "admin"))
        self.assertEqual(self.plan(self.commit())["route"], "software")
        self.write(workflow, original.replace("ready_when='true'", "ready_when='steps.review.done'"))
        self.assertEqual(self.plan(self.commit())["route"], "data")
        self.write("infra/platform/workflows/new.toml", original.split("[workflow.authority]")[0])
        self.assertEqual(self.plan(self.commit())["route"], "data")
        self.write("infra/platform/workflow-holds/learning.toml", "drift_publish='held'\n")
        self.base = self.commit()
        self.write(workflow, original.replace("before", "after protected"))
        self.assertEqual(self.plan(self.commit())["route"], "software")

    def test_existing_protected_rule_effects_and_unknown_handlers_stay_heavy(self):
        policy = (self.repo / POLICY).read_text()
        policy += "\n[[delivery_policy.channels.rules]]\nprefix='infra/dispatcher/rules/'\nsuffix='.toml'\nevidence='rule_equality'\nallowed_handlers=['jobs.spawn']\n"
        self.write(POLICY, policy)
        rule = "infra/dispatcher/rules/example.toml"
        original = "[[rule]]\nname='example'\non_event='step.done.read'\n[[rule.do]]\nhandler='jobs.spawn'\nargs={title='before'}\n"
        self.write(rule, original)
        self.base = self.commit()
        self.write(rule, original.replace("before", "after"))
        self.assertEqual(self.plan(self.commit())["route"], "data")
        for handler in ["credential.rotate.self-issued", "unrecognized.operation"]:
            self.write(rule, original.replace("jobs.spawn", handler))
            self.base = self.commit()
            self.write(rule, original.replace("jobs.spawn", handler).replace("before", "after"))
            self.assertEqual(self.plan(self.commit())["route"], "software")

    def test_repository_docs_exclude_image_assets_and_pin_the_image_input_layout(self):
        recipe = "COPY crates ./crates\nCOPY docs/architecture ./docs/architecture\n"
        self.write("infra/image/Dockerfile", recipe)
        policy = (self.repo / POLICY).read_text().replace(
            "evidence='forge_blob'", "evidence='forge_blob'\nexcluded_prefixes=['docs/architecture/']")
        policy += ("\n[[delivery_policy.channels.guards]]\npath='infra/image/Dockerfile'\nsha256='"
                   + hashlib.sha256(recipe.encode()).hexdigest() + "'\n")
        self.write(POLICY, policy)
        self.base = self.commit()
        self.write("docs/original.md", "repository publication\n")
        self.assertEqual(self.plan(self.commit())["route"], "data")
        self.write("docs/architecture/embedded.md", "image input\n")
        self.assertEqual(self.plan(self.commit())["route"], "software")
        self.write("infra/image/Dockerfile", recipe + "COPY docs ./docs\n")
        self.base = self.commit()
        self.write("docs/original.md", "now copied into image\n")
        self.assertEqual(self.plan(self.commit())["route"], "software")


if __name__ == "__main__":
    unittest.main()
