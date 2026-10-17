"""The frozen plan shared by CI, deploy and the conductor.

These tests exercise the actual planner, with no native publication or apply.
"""
import copy
import importlib.util
import pathlib
import unittest
import tomllib
import hashlib

PATH = pathlib.Path(__file__).with_name("delivery-plan.py")
spec = importlib.util.spec_from_file_location("delivery_plan", PATH)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class FrozenDeliveryPlan(unittest.TestCase):
    def test_authored_image_input_guards_equal_the_actual_reviewed_files(self):
        authored = tomllib.loads((PATH.parents[1] / "platform/delivery-policy/train-conductor.toml").read_text())["delivery_policy"][0]
        guards = authored["channels"]["guards"]
        self.assertTrue(guards)
        for guard in guards:
            self.assertEqual(guard["sha256"],hashlib.sha256((PATH.parents[2] / guard["path"]).read_bytes()).hexdigest(),guard["path"])

    def test_unknown_channel_controls_and_malformed_protection_are_not_lighter_authority(self):
        unknown = copy.deepcopy(self.input)
        unknown["policy"]["channels"]["unread_future_control"] = True
        self.assertEqual(module.plan(unknown)["route"],"software")
        protected = copy.deepcopy(self.input)
        protected["changes"][0]["path"] = "infra/platform/workflows/example.toml"
        protected["policy"]["channels"]["rules"][1]["protected_keys"] = ["authority"]
        for value in [None,[],False,"unread"]:
            protected["protection"] = value
            self.assertEqual(module.plan(protected)["route"],"software")

    def test_authored_workflow_policy_preserves_body_edits_but_new_roster_kinds_need_an_image(self):
        authored = tomllib.loads((PATH.parents[1] / "platform/delivery-policy/train-conductor.toml").read_text())["delivery_policy"][0]
        self.input["policy"] = copy.deepcopy(authored)
        channels = self.input["policy"]["channels"]
        channels["enabled"] = True
        self.input["guard_observations"] = [{"path":g["path"],"base":g["sha256"],"head":g["sha256"],"expected":g["sha256"]} for g in channels["guards"]]
        self.input["guards_match"] = True
        change = self.input["changes"][0]
        change["path"] = "infra/platform/workflows/example.toml"
        self.input["protection"] = {change["path"]:False}
        self.assertEqual(module.plan(self.input)["route"],"data")
        change["old_mode"] = "000000"
        change["old"] = "0" * 40
        self.assertEqual(module.plan(self.input)["route"],"software")

    def setUp(self):
        self.input = {
            "base": "a" * 40,
            "head": "b" * 40,
            "policy": {
                "name": "train-conductor",
                "version": 8,
                "channels": {
                    "schema": 1,
                    "enabled": True,
                    "rules": [
                        {"prefix": "docs/", "suffix": ".md", "evidence": "forge_blob"},
                        {"prefix": "infra/platform/workflows/", "suffix": ".toml", "evidence": "workflow_equality"},
                        {"prefix": "infra/dispatcher/rules/", "suffix": ".toml", "evidence": "rule_equality"},
                    ],
                },
            },
            "changes": [{"path": "docs/architecture-decisions.md", "old": "c" * 40,
                         "new": "d" * 40, "old_mode": "100644", "new_mode": "100644"}],
        }

    def test_docs_are_bound_to_exact_publication_not_a_channel_stamp(self):
        result = module.plan(self.input)
        self.assertEqual(result["route"], "data")
        self.assertEqual(result["base"], self.input["base"])
        self.assertEqual(result["head"], self.input["head"])
        self.assertEqual(result["obligations"], [{**self.input["changes"][0], "evidence": "forge_blob"}])
        self.assertFalse(module.satisfied(result, []))
        receipt = {**result["obligations"][0], "head": result["head"], "observed": True}
        self.assertTrue(module.satisfied(result, [receipt]))
        for key in ["head", "new", "path", "evidence"]:
            wrong = {**receipt, key: "wrong"}
            self.assertFalse(module.satisfied(result, [wrong]), key)

    def test_registry_requires_actual_equality_for_every_file(self):
        for prefix, evidence in [("infra/platform/workflows/", "workflow_equality"),
                                 ("infra/dispatcher/rules/", "rule_equality")]:
            with self.subTest(evidence=evidence):
                self.input["changes"][0]["path"] = prefix + "example.toml"
                p = module.plan(self.input)
                self.assertEqual(p["route"], "data")
                r = {**p["obligations"][0], "head": p["head"], "observed": True}
                self.assertFalse(module.satisfied(p, [r]))
                self.assertTrue(module.satisfied(p, [{**r, "equal": True}]))
                self.assertFalse(module.satisfied(p, [{**r, "equal": False}]))

    def test_mixed_migration_config_security_unknown_and_symlinks_stay_heavy(self):
        for path in ["crates/core/boss-jobs/src/lib.rs", "infra/postgres/schema/20261016000000.sql",
                     "infra/cluster/manifests/boss.yaml", "infra/platform/policy.toml", "unknown.txt"]:
            with self.subTest(path=path):
                candidate = copy.deepcopy(self.input)
                candidate["changes"].append({**candidate["changes"][0], "path": path})
                self.assertEqual(module.plan(candidate)["route"], "software")
        for mode in ["120000", "160000", "100755", "unknown"]:
            candidate = copy.deepcopy(self.input)
            candidate["changes"][0]["new_mode"] = mode
            self.assertEqual(module.plan(candidate)["route"], "software")

    def test_missing_malformed_or_unfrozen_facts_never_grant_a_light_route(self):
        for key in ["base", "head", "policy", "changes"]:
            candidate = copy.deepcopy(self.input)
            del candidate[key]
            self.assertEqual(module.plan(candidate)["route"], "software", key)
        for value in [[], None, "docs", [{}]]:
            candidate = {**self.input, "changes": value}
            self.assertEqual(module.plan(candidate)["route"], "software")
        for key in ["base", "head"]:
            self.assertEqual(module.plan({**self.input, key: "main"})["route"], "software")

    def test_complete_population_no_mutation_no_duplicate_or_extra_receipt(self):
        original = copy.deepcopy(self.input)
        p = module.plan(self.input)
        self.assertEqual(self.input, original)
        r = {**p["obligations"][0], "head": p["head"], "observed": True}
        self.assertFalse(module.satisfied(p, [r, r]))
        candidate = copy.deepcopy(self.input)
        candidate["changes"].append(copy.deepcopy(candidate["changes"][0]))
        self.assertEqual(module.plan(candidate)["route"], "software")

    def test_bad_policy_or_path_cannot_manufacture_eligibility(self):
        for path in ["docs/../infra/secret.md", "/docs/x.md", "docs//x.md", "docs/x.md\ncrates/x"]:
            candidate = copy.deepcopy(self.input)
            candidate["changes"][0]["path"] = path
            self.assertEqual(module.plan(candidate)["route"], "software")

    def test_all_malformed_policy_fields_refuse_without_throwing(self):
        for key, values in [("prefix", [None, 7, [], {}, "../"]),
                            ("suffix", [None, 7, [], {}, ""]),
                            ("evidence", [None, 7, [], {}, "apply"] )]:
            for value in values:
                candidate = copy.deepcopy(self.input)
                candidate["policy"]["channels"]["rules"][0][key] = value
                self.assertEqual(module.plan(candidate)["route"], "software", (key, value))
        for schema in [True, "1", None, 2]:
            candidate = copy.deepcopy(self.input)
            candidate["policy"]["channels"]["schema"] = schema
            self.assertEqual(module.plan(candidate)["route"], "software")

    def test_only_explicit_registry_enablement_can_select_lighter_delivery(self):
        for value in [False, None, 0, 1, "true", [], {}]:
            candidate = copy.deepcopy(self.input)
            candidate["policy"]["channels"]["enabled"] = value
            self.assertEqual(module.plan(candidate)["route"], "software", value)
        candidate = copy.deepcopy(self.input)
        del candidate["policy"]["channels"]["enabled"]
        self.assertEqual(module.plan(candidate)["route"], "software")
        for version in [None, 0, -1, True, "8"]:
            candidate = copy.deepcopy(self.input)
            candidate["policy"]["version"] = version
            self.assertEqual(module.plan(candidate)["route"], "software")


if __name__ == "__main__":
    unittest.main()
