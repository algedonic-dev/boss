#!/usr/bin/env python3
"""
Generate the Algedonic Ales employee roster for 2025-04-01 (the
starting state for the 1yr=1hr sim loop).

Deterministic: a fixed RNG seed means re-running produces identical
output. The output overwrites examples/brewery/seeds/employees.json.

Edit role headcounts, salary bands, or name pools below, then re-run:

    python3 examples/brewery/scripts/gen_employees.py

Headcount target: industrial regional brewer (~140 employees), not a
12-person craft shop. Department mix matches what a real growing
brewer would look like — production-heavy, packaging + warehouse
significant, finance/HR/IT lean but present.

Roles + departments must exist in examples/brewery/seeds/classes.toml.
The script does not validate against the registry; the loader will
fail loudly if a role/department code is unknown.
"""

import json
import random
from datetime import date, timedelta
from pathlib import Path

SEED = 42
TARGET_DATE = date(2025, 4, 1)  # roster snapshot date — the starting state
OUT_PATH = Path(__file__).parent.parent / "seeds" / "employees.json"

# Headcount multiplier applied to every non-singular role in
# ROSTER_PLAN. Singular roles (executives, single-head depts) stay at
# 1 because there's only one CEO / COO / head-brewer / etc. Bumping
# this scales the production/packaging/warehouse/sales/finance ranks
# proportionally so the org chart shape is preserved.
#
# 3 × ~121 IC roles + 20 singular ≈ 395 employees. Reduced from 6
# (#96): at SCALE=6 (789 employees) the brewery's annual payroll
# ($48M) exceeded its gross margin ($29M) and drove cash negative
# over the sim cycle. SCALE=3 cuts annual payroll to ~$24M while
# preserving the org-chart shape. Re-running with a different scale
# produces deterministic but different output thanks to the fixed
# RNG seed.
SCALE = 3

# Demo-mode anonymization. The Algedonic Ales seed is shipped as a
# public OSS playground; real-looking people names create the
# misleading impression that this is a real-company HR record. Every
# employee's display name is now `<role-slug>-<NNN>` (e.g.
# `brewer-007`) with an email of the same shape at example.brewery.
# All other fields (role, department, salary band, hire date, manager
# chain) keep their realism — the workforce shape is the demo, the
# identities are not.

# (department, role, headcount, annual_salary_band_cents).
# Salaries reflect 2024-ish US ranges for a regional brewery.
# Hourly roles are stored as annual gross at 2080h baseline.
ROSTER_PLAN = [
    # Executive — small but real
    ("executive",   "ceo",                1, (180_000_00, 180_000_00)),
    ("executive",   "coo",                1, (155_000_00, 155_000_00)),
    ("executive",   "cfo",                1, (160_000_00, 160_000_00)),
    ("executive",   "cto",                1, (150_000_00, 150_000_00)),
    ("executive",   "head-of-sales",      1, (145_000_00, 145_000_00)),
    ("executive",   "head-of-marketing",  1, (135_000_00, 135_000_00)),
    ("executive",   "head-of-distribution",1,(130_000_00, 130_000_00)),
    ("executive",   "head-of-people",     1, (130_000_00, 130_000_00)),

    # Production — heart of a brewery.
    # 3-tier: head-brewer → shift-lead (24) → {senior-brewer, brewer,
    # cellar-tech} (144). Each shift-lead manages ~6 ICs. senior-
    # brewer is a senior IC role, not a management layer.
    ("production",  "head-brewer",        1, (115_000_00, 125_000_00)),
    ("production",  "shift-lead",         4, (62_000_00,  72_000_00)),
    ("production",  "senior-brewer",      4, (75_000_00,  90_000_00)),
    ("production",  "brewer",            12, (55_000_00,  68_000_00)),
    ("production",  "cellar-tech",        8, (48_000_00,  58_000_00)),

    # QA / Lab — small enough that 2 tiers work
    ("qa",          "qa-supervisor",      1, (78_000_00,  88_000_00)),
    ("qa",          "lab-tech",           3, (52_000_00,  62_000_00)),
    ("qa",          "qa-tech",            4, (46_000_00,  55_000_00)),

    # Packaging — separate dept from production at industrial scale.
    # 3-tier: packaging-mgr → packaging-lead → ICs. At SCALE=6 we get
    # 12 leads supervising ~11 ICs each; the manager sees only the
    # 12 leads instead of the 84+30 line staff.
    ("packaging",   "packaging-mgr",      1, (82_000_00,  92_000_00)),
    ("packaging",   "packaging-lead",     2, (58_000_00,  68_000_00)),
    ("packaging",   "packaging-tech",    14, (42_000_00,  54_000_00)),
    ("packaging",   "palletizer",         5, (38_000_00,  46_000_00)),

    # Warehouse & logistics — 3-tier. warehouse-lead bridges the
    # mgr to the floor; clerks roll up under leads as well to keep
    # the mgr's span ~12 instead of 48.
    ("warehouse",   "warehouse-mgr",      1, (75_000_00,  85_000_00)),
    ("warehouse",   "warehouse-lead",     2, (54_000_00,  64_000_00)),
    ("warehouse",   "forklift-operator",  8, (40_000_00,  50_000_00)),
    ("warehouse",   "inventory-clerk",    3, (44_000_00,  52_000_00)),
    ("warehouse",   "shipping-clerk",     3, (42_000_00,  50_000_00)),

    # Maintenance
    ("maintenance", "maintenance-mgr",    1, (88_000_00,  98_000_00)),
    ("maintenance", "mechanic",           4, (58_000_00,  72_000_00)),
    ("maintenance", "electrician",        2, (68_000_00,  82_000_00)),

    # Distribution (drivers + dispatch) — 3-tier with leads on the
    # floor (12 leads × ~4 drivers each).
    ("distribution","distribution-lead",  2, (62_000_00,  72_000_00)),
    ("distribution","distribution-driver",8, (52_000_00,  64_000_00)),

    # Sales — 3-tier with leads supervising reps and AMs. 12 leads
    # × ~5 staff each at SCALE=6.
    ("sales",       "sales-lead",         2, (88_000_00, 105_000_00)),
    ("sales",       "sales-rep",          6, (62_000_00,  82_000_00)),
    ("sales",       "account-manager",    4, (75_000_00,  95_000_00)),

    # Marketing
    ("marketing",   "brand-manager",      2, (78_000_00,  92_000_00)),
    ("marketing",   "social-media-coord", 1, (52_000_00,  62_000_00)),
    ("marketing",   "events-coord",       2, (48_000_00,  58_000_00)),

    # Taproom — public-facing storefront. taproom-shift-lead is
    # renamed from the generic `shift-lead` to disambiguate from the
    # production shift-lead (different reporting line, different work).
    ("taproom",     "taproom-shift-lead", 2, (52_000_00,  62_000_00)),
    ("taproom",     "bartender",          4, (40_000_00,  48_000_00)),
    ("taproom",     "taproom-server",     6, (36_000_00,  44_000_00)),

    # Finance
    ("finance",     "controller",         1, (115_000_00, 125_000_00)),
    ("finance",     "ap-clerk",           2, (52_000_00,  60_000_00)),
    ("finance",     "ar-clerk",           2, (52_000_00,  60_000_00)),
    ("finance",     "payroll-mgr",        1, (78_000_00,  88_000_00)),
    ("finance",     "fp-analyst",         2, (78_000_00,  92_000_00)),
    ("finance",     "bookkeeper",         1, (58_000_00,  68_000_00)),

    # People (HR)
    ("people",      "hr-generalist",      2, (65_000_00,  78_000_00)),
    ("people",      "recruiter",          1, (68_000_00,  82_000_00)),
    ("people",      "benefits-coord",     1, (56_000_00,  66_000_00)),

    # IT
    ("it",          "it-director",        1, (125_000_00, 135_000_00)),
    ("it",          "sysadmin",           2, (78_000_00,  92_000_00)),
    ("it",          "helpdesk",           2, (52_000_00,  62_000_00)),

    # Admin (front office)
    ("admin",       "bookkeeper",         1, (52_000_00,  62_000_00)),
]

# location_id is currently a single value; refine later with a real
# locations table. For now everyone's at the brewery campus.
DEFAULT_LOCATION = "loc-brewery-brewhouse"

# Hire date distribution: weight toward 2018-2024 (recent growth) with
# a long tail back to 2010 (founders + early hires).
def gen_hire_date(rng, role):
    # Executives + owners trend earlier (founder cohort).
    if role in {"ceo", "coo", "cfo", "cto"}:
        # 2010-2018 founder window
        days = rng.randint(0, (date(2018, 12, 31) - date(2010, 6, 1)).days)
        return date(2010, 6, 1) + timedelta(days=days)
    # Heads + managers: mostly 2015-2022
    if role.startswith("head-") or role.endswith("-mgr") or role in {"controller", "it-director", "qa-supervisor"}:
        days = rng.randint(0, (date(2022, 12, 31) - date(2015, 1, 1)).days)
        return date(2015, 1, 1) + timedelta(days=days)
    # ICs: weighted toward 2020-2025-03-31 with a tail to 2014
    if rng.random() < 0.15:
        # Long tail veterans
        days = rng.randint(0, (date(2019, 12, 31) - date(2014, 1, 1)).days)
        return date(2014, 1, 1) + timedelta(days=days)
    days = rng.randint(0, (date(2025, 3, 31) - date(2020, 1, 1)).days)
    return date(2020, 1, 1) + timedelta(days=days)


def main():
    rng = random.Random(SEED)
    employees = []

    # Track ids by role so we can wire manager_id after the fact.
    by_role = {}
    # Track per-role counters so each role gets its own 001, 002, …
    # display sequence regardless of when it appears in the plan.
    role_counter = {}

    counter = 1
    for dept, role, headcount, (lo_cents, hi_cents) in ROSTER_PLAN:
        # Apply SCALE only to non-singular roles. Executives + single
        # heads stay singular; everyone else multiplies.
        effective_headcount = headcount if headcount == 1 else headcount * SCALE
        for _ in range(effective_headcount):
            role_counter[role] = role_counter.get(role, 0) + 1
            seq = role_counter[role]
            # Demo-mode display: role-slug-NNN. Singular execs stay
            # un-numbered (`ceo`, `cfo`) since there's only one.
            if headcount == 1:
                name = role
                email_local = role
            else:
                name = f"{role}-{seq:03d}"
                email_local = f"{role}-{seq:03d}"
            email = f"{email_local}@example.brewery"

            salary = rng.randint(lo_cents, hi_cents) if hi_cents > lo_cents else lo_cents
            hire_date = gen_hire_date(rng, role)

            emp_id = f"emp-aa-{counter:03d}"
            counter += 1
            emp = {
                "id": emp_id,
                "name": name,
                "email": email,
                "github_username": None,
                "role": role,
                "department": dept,
                "skill_level": None,
                "hire_date": hire_date.isoformat(),
                "location": DEFAULT_LOCATION,
                "manager_id": None,  # filled below
                "employment_type": "full-time",
                "status": "active",
                "skills": [],
                "certifications": [],
                "annual_salary_cents": salary,
            }
            employees.append(emp)
            by_role.setdefault(role, []).append(emp_id)

    # Wire manager_id: each role declares the role of its direct
    # manager. The reporting line walks up the tree until it lands on
    # the CEO. Where multiple candidates exist (e.g. 24 shift-leads
    # for 120 line staff), assignment is round-robin from the
    # seeded RNG so the line staff get distributed evenly across
    # leads instead of all reporting to candidates[0]. Span-of-
    # control thresholds are ~5-15 across the tree, vs. the previous
    # implementation where head-brewer / packaging-mgr / warehouse-
    # mgr each held 100+ direct reports.
    ceo_id = by_role.get("ceo", [None])[0]

    # (department, role) → role of the direct manager. Department
    # is part of the key because `shift-lead` exists in both
    # production and taproom with different reporting lines, and
    # `bookkeeper` exists in both finance and admin.
    manager_role_for = {
        # Executives — all roll up to the CEO directly.
        ("executive", "coo"):                "ceo",
        ("executive", "cfo"):                "ceo",
        ("executive", "cto"):                "ceo",
        ("executive", "head-of-sales"):      "ceo",
        ("executive", "head-of-marketing"):  "ceo",
        ("executive", "head-of-distribution"):"ceo",
        ("executive", "head-of-people"):     "ceo",

        # Production — 3-tier: head-brewer → shift-lead → ICs.
        # senior-brewer is a senior IC role under a shift-lead, not
        # a management layer (folds the previous 4-tier so the
        # head-brewer's reports drop from 28 to 24).
        ("production", "head-brewer"):       "ceo",
        ("production", "shift-lead"):        "head-brewer",
        ("production", "senior-brewer"):     "shift-lead",
        ("production", "brewer"):            "shift-lead",
        ("production", "cellar-tech"):       "shift-lead",

        # QA — 2-tier.
        ("qa", "qa-supervisor"):             "ceo",
        ("qa", "lab-tech"):                  "qa-supervisor",
        ("qa", "qa-tech"):                   "qa-supervisor",

        # Packaging — 3-tier with packaging-lead between mgr and ICs.
        ("packaging", "packaging-mgr"):      "coo",
        ("packaging", "packaging-lead"):     "packaging-mgr",
        ("packaging", "packaging-tech"):     "packaging-lead",
        ("packaging", "palletizer"):         "packaging-lead",

        # Warehouse — 3-tier across the board. Clerks roll up
        # under leads (not mgr) so the mgr's span stays ~12 instead
        # of 12 leads + 18 inventory + 18 shipping = 48.
        ("warehouse", "warehouse-mgr"):      "coo",
        ("warehouse", "warehouse-lead"):     "warehouse-mgr",
        ("warehouse", "forklift-operator"):  "warehouse-lead",
        ("warehouse", "inventory-clerk"):    "warehouse-lead",
        ("warehouse", "shipping-clerk"):     "warehouse-lead",

        # Maintenance — 2-tier.
        ("maintenance", "maintenance-mgr"):  "coo",
        ("maintenance", "mechanic"):         "maintenance-mgr",
        ("maintenance", "electrician"):      "maintenance-mgr",

        # Distribution — 3-tier with distribution-lead.
        ("distribution", "distribution-lead"):  "head-of-distribution",
        ("distribution", "distribution-driver"):"distribution-lead",

        # Sales — 3-tier with sales-lead.
        ("sales", "sales-lead"):             "head-of-sales",
        ("sales", "sales-rep"):              "sales-lead",
        ("sales", "account-manager"):        "sales-lead",

        # Marketing — 2-tier.
        ("marketing", "brand-manager"):      "head-of-marketing",
        ("marketing", "social-media-coord"): "head-of-marketing",
        ("marketing", "events-coord"):       "head-of-marketing",

        # Taproom — 3-tier with taproom-shift-lead. Rolls up under
        # operations (coo), separate reporting line from production.
        ("taproom", "taproom-shift-lead"):   "coo",
        ("taproom", "bartender"):            "taproom-shift-lead",
        ("taproom", "taproom-server"):       "taproom-shift-lead",

        # Finance — 2-tier. Controller is the head of finance.
        ("finance", "controller"):           "cfo",
        ("finance", "ap-clerk"):             "controller",
        ("finance", "ar-clerk"):             "controller",
        ("finance", "payroll-mgr"):          "controller",
        ("finance", "fp-analyst"):           "controller",
        ("finance", "bookkeeper"):           "controller",

        # People (HR) — 2-tier.
        ("people", "hr-generalist"):         "head-of-people",
        ("people", "recruiter"):             "head-of-people",
        ("people", "benefits-coord"):        "head-of-people",

        # IT — 2-tier.
        ("it", "it-director"):               "cto",
        ("it", "sysadmin"):                  "it-director",
        ("it", "helpdesk"):                  "it-director",

        # Admin (front office) — small, rolls up under operations.
        ("admin", "bookkeeper"):             "coo",
    }

    # Round-robin counter per (dept, manager-role) so the seeded
    # RNG drives even distribution across managers in the same
    # bucket (e.g. 120 line staff spread across 24 shift-leads).
    # Pre-shuffle each bucket once with the seeded RNG so the
    # specific candidate order is deterministic but not just
    # registration order.
    shuffled_by_role = {}
    for role_name, ids in by_role.items():
        shuf = list(ids)
        rng.shuffle(shuf)
        shuffled_by_role[role_name] = shuf
    bucket_cursor = {}  # (dept, manager_role) -> next index

    def pick_manager_in_dept(dept, manager_role, emp_id):
        """Pick the next manager-role employee in dept, round-robin."""
        # Manager pool: every employee with role=manager_role.
        # The roster doesn't index by (dept, role), but for the
        # roles we declare here, manager-roles are unique per
        # department (head-brewer only in production, etc.), so
        # filtering by role alone suffices. Exception: roles like
        # coo that manage employees from multiple departments —
        # those have a single instance, so distribution doesn't
        # matter.
        pool = [c for c in shuffled_by_role.get(manager_role, []) if c != emp_id]
        if not pool:
            return None
        key = (dept, manager_role)
        idx = bucket_cursor.get(key, 0)
        chosen = pool[idx % len(pool)]
        bucket_cursor[key] = idx + 1
        return chosen

    for emp in employees:
        role = emp["role"]
        dept = emp["department"]
        if role == "ceo":
            continue
        manager_role = manager_role_for.get((dept, role))
        if manager_role is None:
            # Unknown (dept, role) combo — fall back to CEO so the
            # tree is at least connected. Add to manager_role_for
            # when a new role lands in ROSTER_PLAN.
            emp["manager_id"] = ceo_id
            continue
        manager_id = pick_manager_in_dept(dept, manager_role, emp["id"])
        emp["manager_id"] = manager_id if manager_id else ceo_id

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    OUT_PATH.write_text(json.dumps(employees, indent=2) + "\n")

    # Summary to stdout
    by_dept = {}
    for e in employees:
        by_dept[e["department"]] = by_dept.get(e["department"], 0) + 1
    print(f"Wrote {len(employees)} employees to {OUT_PATH}")
    print("Department counts:")
    for dept, count in sorted(by_dept.items(), key=lambda kv: -kv[1]):
        print(f"  {dept:12s}  {count}")


if __name__ == "__main__":
    main()
