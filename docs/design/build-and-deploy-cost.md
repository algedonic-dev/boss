# Design: the workspace is built twice

**Status**: in-review
**Origin**: measured, not suspected — the protocol-retro's first
instance (job 23fa024b) named CI as the long pole, and the
2026-08-13 morning run measured the second one.
**Related**: [deployment-as-network.md](./deployment-as-network.md) ·
[protocol-experiments.md](./protocol-experiments.md) ·
[internal-forge.md](./internal-forge.md)

---

## The measurements

| leg | cost | source |
|---|---|---|
| CI `test` job | 20–40 min per consist | forge action logs, trains 1–17 |
| CI `fast` job (fmt+clippy) | ~1 min | same |
| deploy after a **leaf-crate** change | 164 s | train 14, 11:20→11:22Z |
| deploy after a **core-crate** change | 30+ min | train 12, 09:50→10:34Z (boss-core touched) |
| cluster converge | ≤10 min tick, image built on the forge host | runner stamp |

Measured again at the end of the day, across 14 trains: **median
board→merge 35 min** (dominated by the CI `test` job) and **median
merge→deploy 3 min** (n=5 trains with full timestamps). That refines
the claim above rather than contradicting it: the deploy is cheap in
the common case and expensive only when a core crate changed, which
was 1 train in 14 today. The duplicated build is real but it is not
what an operator waits on — CI is.

The deploy figure is the interesting one: the same workspace that CI
just compiled is compiled **again** on the playground host, from
scratch when a core crate changed, while the operator watches. A
change to `boss-core` invalidates every downstream crate, so the
cost is structural rather than incidental.

## The asymmetry already in the system

The **cluster** does not rebuild: the forge host builds an image
once, pushes it to the registry, and the converge runner rolls the
deployment onto that image. The **playground** rebuilds from source
on every deploy. Two deploy paths for one artifact, and only one of
them pays the compile twice.

That asymmetry is the finding. Whatever we choose, the goal is one
build per commit, consumed by every environment that runs it.

## Open questions

### Q1: Where should the one build happen?

Candidates: (a) CI builds release binaries and publishes them as
forge artifacts, the deploy fetches; (b) the forge host's existing
image build becomes the single artifact and the playground runs the
container too; (c) neither — keep building on the playground but
make it cheap with a shared cache (`sccache`) or a warm target dir.

(a) and (b) achieve build-once; (c) only reduces the constant. (b)
unifies the two deploy paths but changes what the playground *is*
(a host running systemd services becomes a host running a
container), which touches the generation store, the confirm unit,
and every operational habit built around them. (a) keeps the
generation store exactly as it is and swaps only where `bin/` comes
from.

Proposed: **(a)**, because the generation store is young, working,
and already keyed by sha — a fetched artifact drops into
`releases/<sha>/bin/` with no other change — and because it makes
the deploy's provenance explicit: the binaries that deploy are the
ones CI tested, not a recompilation that happens to match.

### Q2: What proves an artifact is the tested one?

If the deploy fetches instead of building, the fingerprint pre-flight
("release binaries were built from different sources than the tree")
loses its meaning and must be replaced by something stronger: the
artifact carries the sha it was built from, the deploy refuses an
artifact whose sha ≠ the tree's HEAD, and the arrival report records
which artifact landed. Otherwise we trade a slow honest build for a
fast unverifiable one.

Proposed: publish artifacts keyed by commit sha, refuse any
mismatch, record it — the same equality posture the locomotive check
uses for the CI image stamp.

### Q3: Does CI's own cost move at all?

Build-once helps deploys, not the 20–40 minute CI leg, which is
dominated by the `test` job. The pre-designed experiment
(protocol-experiments Q4) is a second runner or off-window builds.
Worth noting: the two questions are independent, and Q1 is the
cheaper win.

### Q4: Is this worth doing before more protocol work?

The end-of-day medians sharpen this question considerably: at a
3-minute median deploy, build-once buys almost nothing on a typical
train and only pays on the rare core-crate day. The honest case
against: deploys are not currently blocking anyone —
the trains land, and 30 minutes of compile costs an operator
nothing when the loop no longer goes deaf during it (the
cadence-nonblocking car). The case for: it is the last place where
the system does the same work twice, and duplicated work is the
thing this design language keeps eliminating everywhere else.
