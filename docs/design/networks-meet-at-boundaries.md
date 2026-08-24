# Design: networks meet at boundaries — retiring the second tenant

**Status**: in-review — the direction is decided; the questions are about scope and order.

**Origin**: David, 2026-08-23, on `/ux/refurb`: *"Refurb should be
removed. We aren't supporting multi-tenancy anymore. It isn't really
in the network spirit. Instead, tenants will connect networks at the
boundary through agreed protocols."*

**Related**: [the-three-layers](the-three-layers.md) ·
[job-packet-network](job-packet-network.md) ·
[platform-vs-tenant-jobkinds](platform-vs-tenant-jobkinds.md)

---

## What the sentence changes

"Multi-tenancy" in this repo has meant: one deployment can model more
than one company, and the second company is proof the first isn't
special. That produced a real discipline — every time a brewery noun
tried to reach Tier 1, the used-device-shop was the reason it
couldn't. The invariant `no-tenant-nouns-in-the-platform-roster`
exists because of it.

The new claim is not "generality was a mistake." It is that **the
unit of the system is one organization's network**, and two
organizations relate the way two networks relate: at a boundary,
through a protocol both sides agreed to. Not by sharing a database
with a tenant column. Under the three-layer reading this is the
substrate arguing for itself — a packet's provenance, the one
admission edge, and the pinned protocol set are all statements about
*a* network. A second tenant inside the same network was always a
tenant-shaped hole in that story.

So the deletion is not a retreat from generality. It moves the
generality from *rows in one deployment* to *protocols between
deployments* — which is where BOSS already keeps its meaning.

## What is actually there

Measured 2026-08-23:

| thing | size | verdict |
|---|---|---|
| `crates/tenants/boss-used-device-shop-engine` | 3 files, 535 lines | goes |
| `examples/used-device-shop/` (DOMAIN.md, seeds, data) | 7 files, 212 KB | goes |
| `apps/web` refurb/device surfaces | 23 files reference them; `/refurb` + `/refurb/{id}` routes | goes, mostly |
| `boss-commerce` (5,180), `boss-inventory` (11,885), `boss-shipping` (3,548), `boss-catalog` (4,567), `boss-assets` (7,869) | ~33,000 lines | **stays** — the brewery uses all five (parts, products, vendors, shipments) |
| core references (`registry.rs`, `port.rs`, `defaults.rs`) | comments + a test denylist | rewrite, don't delete |
| `docs/invariants/no-tenant-nouns-in-the-platform-roster.toml` | the guard | **stays, re-argued** |

The engine and its seeds are small. The surfaces are the visible
part. The module crates — the thing that *looks* like it was built
for a device shop — are load-bearing for the brewery and not in
scope. That is the whole cost picture: **under a thousand lines of
tenant code plus a page, against a claim repeated in six documents.**

## The part worth being careful about

Deleting the second tenant removes the pressure that kept Tier 1
honest. The invariant survives the tenant that motivated it — its
denylist can keep naming device-shop nouns that no longer exist,
because the point is the *shape* of the mistake, not those specific
words. But the doc that explains it must stop saying "because the
used-device-shop would break" and start saying "because a network
that hardcodes its own vocabulary cannot meet another network at a
boundary." Same guard, honest reason.

Two smaller consequences, both good:

- Packet `b3480531` ("used-device-shop has no install story") is
  **moot** — the honest answer is that the tenant is going, not that
  the install path was owed.
- `README.md`'s "adding a third tenant takes two TOML files and a
  JSON seed" becomes false and should be replaced by the boundary
  claim, which is the one we actually intend to keep.

## Proposed order

1. **This doc lands.** The principle is written before anything is
   cut, so the deletions cite it rather than a chat message.
2. **The surfaces** — `/refurb`, its routes, nav entries, and the
   device-detail page that only that tenant reaches. Visible, safe,
   answers the original report.
3. **The tenant** — engine crate, `examples/used-device-shop/`,
   workspace membership, and the seeds' bootstrap branch
   (`bootstrap-vm.sh`'s `TENANT=device-shop` arm is already a no-op).
4. **The claims** — CLAUDE.md §Project Overview and §10, README,
   `architecture-decisions.md`, `platform-vs-tenant-jobkinds.md`, and
   the invariant's rationale. This is the step that must not be
   skipped: a repo that deletes a tenant but keeps advertising it has
   simply moved the lie.
5. **Close `b3480531`** as moot, citing this doc.

Steps 2–4 are separately reviewable cars. Nothing here needs the
boundary protocol to exist first — that is the next design, not a
prerequisite.

## Open questions

### Q1: Does anything about the device-shop domain deserve to survive as a platform example?

The Equipment KB (`boss-catalog`, `boss-assets`) was built with
device refurb in mind and is genuinely generic — the brewery uses it
for vessels and mechanics use it for failure modes. Should some of
`examples/used-device-shop/DOMAIN.md` be rewritten as a *platform*
document about equipment-bearing work, or does it all go and the
brewery becomes the only worked example?

### Q2: What replaces the second tenant as the guard against Tier-1 leakage?

Today the answer is a denylist of two tenants' nouns plus a count
assertion. With one tenant, the denylist half keeps working but the
"a second tenant would break" argument disappears. Options: keep the
denylist on its own merits, add a shape rule (no proper nouns in
`platform_workflows()` at all), or let the platform bundle finish the
job — the FORM fix the invariant's own note already points at.

### Q3: Is the used-device-shop worth keeping as a *separate network* rather than deleting?

The doctrine says organizations run their own networks and meet at
boundaries. A device shop is exactly the kind of counterparty a
brewery has. Keeping it as a second *deployment* — its own database,
its own packets, connected to the brewery's network only through an
agreed protocol — would be the strongest possible demonstration of
the new claim, and the strongest possible test of it. That is
materially more work than deleting, and it is a different program
(it needs the boundary protocol first). Delete now and rebuild later
as a peer, or keep the code parked until that program starts?
