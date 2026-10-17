"""Execute both actual workflow selectors against frozen adversarial git trees."""
import hashlib
import json
import os
import pathlib
import subprocess
import textwrap

import test_delivery_plan_git as git_tests

ROOT = pathlib.Path(__file__).resolve().parents[2]
POLICY = git_tests.POLICY


def workflow_selectors():
    source = (ROOT / ".forgejo/workflows/ci.yml").read_text()
    selectors = []
    for job in ("delivery-plan", "lighter-validation"):
        body = source.split("  " + job + ":\n", 1)[1].split("\n  # THE IMAGE", 1)[0]
        # Read the executable selector from the YAML run block, including its
        # stdin program. Do not substitute a test-owned adapter for that command.
        lines = body.splitlines()
        start = next(i for i, line in enumerate(lines) if line.startswith("          python3 "))
        end = next(i for i in range(start, len(lines)) if lines[i] == "          cat delivery-plan.json")
        selectors.append(textwrap.dedent("\n".join(lines[start:end])) + "\n")
    return selectors


class WorkflowSelector(git_tests.GitPlan):
    def setUp(self):
        super().setUp()
        for name in ("delivery-ci.py", "delivery-plan.py"):
            self.write("infra/forge/" + name, (ROOT / "infra/forge" / name).read_text())
        self.write("infra/forge/boss-ci/Dockerfile", "FROM trusted\n")
        self.write("infra/forge/boss-ci/required-tools.txt", "git\npython3\n")
        self.base = self.commit()
        self.stamp = self.repo / ".git/runner-stamp"
        self.stamp.write_text(hashlib.sha256(b"FROM trusted\ngit\npython3\n").hexdigest())

    def selections(self, base, head):
        results = []
        for script in workflow_selectors():
            # The fixture stamp replaces only the runner's absolute stamp path;
            # every selector command and its bootstrap program come from YAML.
            script = script.replace("/etc/boss-ci-stamp", str(self.stamp))
            script += "cat delivery-plan.json\n"
            out = subprocess.run(["bash", "-eu"], input=script, cwd=self.repo,
                                 env={**os.environ, "BASE_SHA": base, "GITHUB_SHA": head},
                                 text=True, capture_output=True)
            self.assertEqual(out.returncode, 0, out.stderr)
            results.append(json.loads(out.stdout))
        return results

    def assert_routes(self, base, head, expected):
        self.assertEqual([r["route"] for r in self.selections(base, head)], [expected, expected])

    def forge_selector(self):
        # A candidate-owned selector can return a completely well-shaped lie.
        # Exercise the BASE ownership boundary, not just the outer shape guard
        # (independent review48a035de, urgent271a2f6e).
        self.write("infra/forge/delivery-ci.py",
                   'import argparse,json\n'
                   'p=argparse.ArgumentParser(); p.add_argument("--repo"); p.add_argument("--base"); '
                   'p.add_argument("--head"); p.add_argument("--stamp"); a=p.parse_args()\n'
                   'print(json.dumps({"schema":1,"route":"data","base":a.base,"head":a.head,'
                   '"obligations":[{"path":"forged"}],"ci_runner_recipe_sha256":"0"*64}))\n')

    def test_actual_workflow_refuses_candidate_selector_with_disabled_base(self):
        self.write(POLICY, (self.repo / POLICY).read_text().replace("enabled=true", "enabled=false"))
        base = self.commit()
        self.forge_selector()
        self.assert_routes(base, self.commit(), "software")

    def test_actual_workflow_refuses_candidate_selector_and_software_change(self):
        self.forge_selector()
        self.write("crates/actual.rs", "software\n")
        self.assert_routes(self.base, self.commit(), "software")

    def test_actual_workflow_qualified_docs_can_use_the_matching_runner(self):
        self.write("docs/original.md", "qualified docs\n")
        self.assert_routes(self.base, self.commit(), "data")

    def test_missing_invalid_or_unavailable_base_adapter_preserves_software_bootstrap(self):
        for source in (None, "invalid python!\n", 'print("not JSON")\n',
                       'print(\'{"schema":1,"route":"data","obligations":[]}\')\n'):
            path = self.repo / "infra/forge/delivery-ci.py"
            if source is None:
                path.unlink()
            else:
                path.write_text(source)
            base = self.commit()
            self.forge_selector()
            self.write("docs/original.md", base)
            self.assert_routes(base, self.commit(), "software")
        self.assert_routes("a" * 40, self.git("rev-parse", "HEAD"), "software")
        self.assert_routes(self.base[:7], self.git("rev-parse", "HEAD"), "software")

    def test_actual_workflow_missing_planner_policy_or_runner_stamp_stays_software(self):
        self.write("docs/original.md", "qualified docs\n")
        head = self.commit()
        self.stamp.write_text("stale")
        self.assert_routes(self.base, head, "software")
        self.stamp.unlink()
        self.assert_routes(self.base, head, "software")
        for missing in ("infra/forge/delivery-plan.py", POLICY):
            (self.repo / missing).unlink()
            base = self.commit()
            self.write("docs/original.md", missing)
            self.assert_routes(base, self.commit(), "software")

    def test_both_jobs_execute_the_identical_trusted_bootstrap(self):
        first, second = workflow_selectors()
        self.assertEqual(first, second)

    def test_tool_lint_ignores_python_body_but_checks_commands_after_heredoc(self):
        for name in ("infra/lint/ci-tools-declared.sh", "infra/lint/lib/scanned.sh",
                     "infra/forge/boss-ci/required-tools.txt"):
            self.write(name, (ROOT / name).read_text())
        workflow = """jobs:
  fixture:
    container:
      image: fixture-image
    steps:
      - name: Execute the declared tools
        run: |
          python3 - <<'PY'
          this_is_python_data_not_a_binary
          PY
          cargo --version
          git --version
          rustc --version
"""
        image = next(line.strip() for line in (ROOT / ".forgejo/workflows/ci.yml").read_text().splitlines()
                     if line.strip().startswith("image:"))
        workflow = workflow.replace("image: fixture-image", image)
        for contents, expected in ((workflow, 0),
                                   (workflow.replace("<<'PY'", '<<"PY"'), 0),
                                   (workflow.replace("<<'PY'", "<<PY"), 0),
                                   (workflow + "          undeclared-after-heredoc\n", 1),
                                   (workflow.replace("          PY\n", ""), 1)):
            self.write(".forgejo/workflows/ci.yml", contents)
            out = subprocess.run(["bash", "infra/lint/ci-tools-declared.sh"], cwd=self.repo,
                                 capture_output=True, text=True)
            self.assertEqual(out.returncode, expected, out.stdout + out.stderr)
            if "undeclared-after-heredoc" in contents:
                self.assertIn("undeclared-after-heredoc", out.stderr)
            elif expected:
                self.assertIn("unterminated-heredoc", out.stderr)

    def test_quoted_markers_cannot_hide_a_real_tool_before_the_actual_heredoc(self):
        for name in ("infra/lint/ci-tools-declared.sh", "infra/lint/lib/scanned.sh",
                     "infra/forge/boss-ci/required-tools.txt"):
            self.write(name, (ROOT / name).read_text())
        image = next(line.strip() for line in (ROOT / ".forgejo/workflows/ci.yml").read_text().splitlines()
                     if line.strip().startswith("image:"))
        prefix = ("jobs:\n  fixture:\n    container:\n      " + image +
                  "\n    steps:\n      - name: Fixture\n        run: |\n")
        for marker in ("echo 'Example: <<PY'", 'echo "Example: <<PY"',
                       r"echo Example: \<\<PY", "echo quoted # Example: <<PY", "echo <<<PY"):
            # Same shape as independent review7a8: a quoted display example,
            # a REAL unknown tool, then a genuine heredoc with the same marker.
            lines = [marker, "undeclared-real-tool", "cat <<PY", "literal data", "PY",
                     "cargo --version", "git --version", "rustc --version"]
            self.write(".forgejo/workflows/ci.yml", prefix + "".join("          " + line + "\n" for line in lines))
            out = subprocess.run(["bash", "infra/lint/ci-tools-declared.sh"], cwd=self.repo,
                                 capture_output=True, text=True)
            self.assertEqual(out.returncode, 1, marker + "\n" + out.stdout + out.stderr)
            self.assertIn("undeclared-real-tool", out.stderr)
