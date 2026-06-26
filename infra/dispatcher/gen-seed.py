#!/usr/bin/env python3
"""Generate infra/postgres/schema/41-dispatcher.sql from rules.toml.

The dispatcher rules moved to a DB registry (the `dispatcher_rules` table);
rules.toml stays as the human-authored source, and this script regenerates
the schema seed from it so the two can't be hand-edited apart. Run from the
repo root:  python3 infra/dispatcher/gen-seed.py

The `dispatcher_rules_seed_matches_toml` test (boss-dispatcher) guards the
committed seed against rules.toml drift.
"""
import json
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RULES_TOML = ROOT / "infra" / "dispatcher" / "rules.toml"
OUT = ROOT / "infra" / "postgres" / "schema" / "41-dispatcher.sql"


def sql_str(s: str | None) -> str:
    return "NULL" if s is None else "'" + s.replace("'", "''") + "'"


def sql_jsonb(obj) -> str:
    j = json.dumps(obj, separators=(",", ":"))
    return "'" + j.replace("'", "''") + "'::jsonb"


def main() -> None:
    with open(RULES_TOML, "rb") as f:
        doc = tomllib.load(f)
    rules = doc.get("rule", [])

    rows = []
    for r in rules:
        do = [{"handler": s["handler"], "args": s.get("args", {})} for s in r.get("do", [])]
        # A rule is triggered by EXACTLY ONE of `on_event` or `[rule.schedule]`.
        # The schedule columns are NULL for an event rule and vice versa.
        sched = r.get("schedule")
        if sched is not None:
            cadence = sql_str(sched["cadence"])
            anchor = sql_str(sched["anchor_date"])
            calendar = sql_str(sched.get("business_calendar"))
        else:
            cadence = anchor = calendar = "NULL"
        rows.append(
            f"  ({sql_str(r['name'])}, {r.get('version', 1)}, 'active', "
            f"{sql_str(r.get('on_event'))}, {sql_str(r.get('when'))}, "
            f"{sql_jsonb(do)}, {sql_str(r.get('delay'))}, "
            f"{cadence}, {anchor}, {calendar})"
        )

    header = """\
-- 41-dispatcher.sql — dispatcher rule registry (the reactive side-effect layer).
--
-- GENERATED from infra/dispatcher/rules.toml by infra/dispatcher/gen-seed.py —
-- do NOT hand-edit the seed rows; edit rules.toml and regenerate. The
-- dispatcher loads the ACTIVE rows from this table at startup (it no longer
-- reads rules.toml at runtime) and /api/dispatcher/rules serves them. The
-- `dispatcher_rules_seed_matches_toml` test guards this seed against drift.
--
-- Append-only + versioned like step_plugins: a new version of a rule supersedes
-- the prior active row (retire it, insert the new one); the partial unique index
-- keeps exactly one 'active' row per rule name.

CREATE TABLE IF NOT EXISTS dispatcher_rules (
    name        TEXT NOT NULL,
    version     INT  NOT NULL,
    status      TEXT NOT NULL CHECK (status IN ('draft', 'active', 'retired')),
    -- A rule is triggered by EXACTLY ONE of `on_event` (NATS event) or a
    -- `schedule_*` group (clock-driven). on_event is nullable now so a
    -- scheduled rule can omit it; the application enforces the XOR.
    on_event    TEXT,
    when_expr   TEXT,                 -- the rule's `when` predicate (NULL = always)
    do_steps    JSONB NOT NULL,       -- [{"handler": "...", "args": {...}}, ...]
    delay       TEXT,                 -- optional delay spec
    -- Clock trigger (all-or-nothing): cadence + anchor are both set for a
    -- scheduled rule, both NULL for an event rule. The dispatcher fires the
    -- rule on each sim-DAY the cadence selects (postponed onto a business
    -- day when schedule_calendar names one).
    schedule_cadence  TEXT,          -- daily|weekly|biweekly|monthly|quarterly|annually
    schedule_anchor   DATE,          -- cadence anchor date
    schedule_calendar TEXT,          -- optional business-calendar code (us-banking, …)
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (name, version)
);

CREATE UNIQUE INDEX IF NOT EXISTS dispatcher_rules_one_active_per_name
    ON dispatcher_rules (name) WHERE status = 'active';

-- Single-row cursor for the clock-driven schedule runner: the last sim-DAY
-- whose schedule rules have already been fired. The runner reads it at
-- startup, fires `(cursor, today]` (capped), and persists the advance —
-- so a restart resumes where it left off instead of replaying or skipping.
-- The CHECK pins the table to one row (id = 1).
CREATE TABLE IF NOT EXISTS dispatcher_clock_cursor (
    id           INT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    last_sim_day DATE
);

INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
"""
    OUT.write_text(header + ",\n".join(rows) + ";\n")
    print(f"wrote {OUT.relative_to(ROOT)} with {len(rows)} rules")


if __name__ == "__main__":
    main()
