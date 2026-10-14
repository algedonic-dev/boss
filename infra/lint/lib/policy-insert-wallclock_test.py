"""Pin moved operational SQL tuples without exempting their source file."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[3]
lint = subprocess.run(["bash", str(root / "infra/lint/no-wallclock.sh")], cwd=root, capture_output=True, text=True)
assert lint.returncode == 0, lint.stdout + lint.stderr

logical = "crates/core/boss-policy-client/src/postgres.rs"
lines = (root / logical).read_text().splitlines()

def actual_statement(prefix):
    start = next(i for i, line in enumerate(lines) if prefix in line)
    return "\n".join(lines[start:start + 3])

rules = actual_statement('"INSERT INTO policy_rules ')
overrides = actual_statement('"INSERT INTO policy_user_overrides ')
helper = root / "infra/lint/lib/policy-insert-wallclock.sh"

def classified(text, named=logical):
    with tempfile.NamedTemporaryFile(mode="w") as source:
        source.write(text)
        source.flush()
        return subprocess.run(["bash", "-c", 'source "$1"; policy_operational_insert_allowed "$2" "$3" "$4"',
            "control", str(helper), named, source.name, str(len(text.splitlines()))], capture_output=True).returncode

assert classified(rules) == 0
assert classified(overrides) == 0
assert classified(rules, "crates/core/boss-policy-client/src/unrelated.rs") != 0
assert classified(rules.replace("created_at, created_by", "happened_on, created_by")) != 0
assert classified(rules.replace("$5, $6", "NOW(), $6")) != 0
assert classified(overrides.replace("$7, NOW()", "NOW(), NOW()")) != 0
assert classified(rules.replace("policy_rules", "invoices")) != 0
assert classified('fn business() { let query = "SELECT CURRENT_DATE"; }') != 0
assert classified(rules + '\nfn business() { let query = "SELECT CURRENT_DATE"; }') != 0
print("policy-insert-wallclock: two exact moved operational tuples; seven same-source/foreign-source business negatives refused")
