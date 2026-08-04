# Runbook: the playground baseline

**Status:** living — the contract for how model changes reach a running tenant.

## The contract

A BOSS demo tenant replays a **frozen baseline**. `sim_clock.epoch_baseline_audit_id`
holds `MAX(audit_log.id)` as of the moment the seed finished, and every
`restart-epoch` trims `audit_log` back to exactly that id before rebuilding
projections from what survives.

The consequence is the part that surprises people:

> **Nothing merged after the baseline was cut exists in the running tenant.**
> Not seed values, not JobKind specs, not Class rows. The demo will replay
> the same frozen snapshot every lap, indefinitely, no matter how far the
> source tree moves.

That is a deliberate pin — a demo you can hand someone should not silently
change shape under them — but it is only safe if the pin is *legible*.

## Three data paths, all from-empty-only

| what | authored in | reaches a tenant via |
|---|---|---|
| Class rows | `examples/<tenant>/seeds/classes.json`, `infra/postgres/schema/*.sql` | prepare step 1 (`POST /api/classes/batch`) |
| Seed values (reorder points, BOMs, products, accounts) | `examples/<tenant>/seeds/*.toml` | the baseline's audit events |
| JobKind specs | `examples/<tenant>/seeds/job_kinds.toml` | prepare's publish pass |

None of these reach a *running* tenant. Republishing a JobKind does not
either — the sim posts whole Jobs carrying `job_kind_version`, and the
create path honours the posted version.

## Checking the pin

```
GET /api/clock/baseline
{
  "baseline_audit_id": 4495,
  "cut_at": "2026-08-04T03:04:46.511999Z",
  "source_ref": "27e29fda",
  "age_days": 0
}
```

`age_days` is measured in **wall** time, not sim time — it answers "how long
has this pin been drifting from the source tree", which is a real-world
duration.

`source_ref` is the short git SHA the seed was cut from. `null` means the
seeding host had no git (docker images ship no `.git`) or predates the
column — read that as **unknown**, never as *current*.

## When to re-cut

Re-cut whenever you need the running tenant to reflect source, i.e. after
any change to the three paths above — and as a matter of course when
pinning the demo to a new release.

A useful habit: compare `source_ref` against what is deployed. If they have
diverged, the demo is showing you an older model than the code you are
reading. Diagnosing a "gap" off a stale demo is how a packaging fix that
merged on 2026-07-13 was re-reported as an open modeling defect three weeks
later — the demo was pinned to 2026-07-11.

## Re-cutting

```bash
# 1. binaries and seeds must come from the SAME tree: the reset runs
#    tools from $REPO_ROOT/target/release while the services run
#    /usr/local/bin.
./infra/build-release.sh
sudo ./infra/deploy-services.sh prod

# 2. drop + reseed + re-stamp + restart. ~1-2 min of downtime; the sim
#    then rebuilds the 12-month demo live from day 0.
sudo ./infra/postgres/reset-to-baseline.sh

# 3. confirm the pin moved
curl -s localhost:7060/api/clock/baseline
```

`BOSS_BASELINE_SOURCE_REF` overrides the recorded revision for callers that
know it without a working tree (container builds).

The reset aborts rather than stamping a baseline over a failed seed — a
baseline captured mid-failure has no published JobKinds, and the *next*
restart-epoch would trim to it and destroy the tenant model. If it aborts,
fix the reported cause and re-run; the script is idempotent.

## What re-cutting costs

The audit log is discarded down to the new seed, so the demo loses its
accumulated history and starts again at day 0. At the brewery's default
warp (1000) a full 12-month lap takes roughly 8-9 wall-hours to refill,
though the demo is usable from day 0 immediately.
