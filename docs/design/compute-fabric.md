# Design: the compute fabric

**Status**: draft — worked up from packets `a59de54d` (the directive)
and `6796ee5f` (IT operations are substrate, not protocol), 2026-08-18.

**Origin**: David, 2026-08-18, verbatim: *"Can we make boss-gcp
effectively part of the 'cluster' while not necessarily running Talos,
etc?"* and, extending it: *"I think our compute should all be part of
the same fabric."*

**Related**: [the-three-layers](./the-three-layers.md) (the frame this
doc applies to machines) · [dev-node-checkout](./dev-node-checkout.md)
(the first fabric-native compute experience, approved) ·
[stations](./stations.md) (the queue/capability layer machines plug
into) · protocol-cadence Decision history Q1/Q4 (the cadence fold this
doc leans on) · invariants register
`instance-registry-asymmetry-is-declared` (the declared role split
between the two instances)

---

## The compute today, and its four membership stories

| machine | runs | supervised by | how the system sees it |
|---|---|---|---|
| Talos cluster (cp-1/2/3) | the system of record: BOSS pod, Postgres, Kanidm, Longhorn, dev pods | kubelet / Talos | fully — it IS the SoR |
| boss-gcp | train conductor, public demo instance, Caddy/Cloudflare edge | systemd | not at all — its role lives in env drop-ins and memory files |
| minipc (`david-asus-minipc`) | Forgejo forge, CI runner, cluster-deploy-runner | systemd + docker | not at all — not even reachable from boss-gcp (Mac jump only) |
| David's Mac | agent sessions, gates | nothing | not at all |

Four machines, and the only one the system itself can see is the
cluster. Everything known about the other three — where the jobs API
lives, how to cancel a train, which remote a car pushes to, why the
runner must not host compose jobs — lives in agent memory files and
one person's head. Packet `6796ee5f` names that plainly: *a protocol
is how an organization remembers; a memory file is how an individual
copes.*

## Two readings of "same fabric", at different layers

**(a) The Kubernetes layer** — join boss-gcp (and eventually the
minipc) to the Talos cluster as worker nodes. Possible without
running Talos: the control plane is standard Kubernetes, so a
kubeadm-joined kubelet over the existing WireGuard link would
register, and taints keep Longhorn and stateful pods off it. But it
buys a permanent snowflake — Talos manages machine config, PKI
rotation and upgrades for its own nodes only, so the non-Talos node
drifts by hand forever; the WAN link makes it a flaky worker with a
VPN as a single point of failure; and the CNI now spans the WAN.
This defines membership by *process supervisor*.

**(b) The BOSS layer** — the fabric is the packet network itself:
one system of record, stations, routes, the log. A machine is "in
the fabric" when the system can *see* it (it is a Subject), *route
to* it (a station holds work only it can do), and *audit* it (its
operations are protocols leaving events). Whether systemd or kubelet
supervises its processes is an implementation detail of the machine,
exactly as it is for a human actor's laptop.

The three-layers frame decides this: the network is the substrate,
and nodes are actors on it. Making kubelet the membership test puts
the fabric at the wrong layer — and when the two frames disagree,
the network framing wins.

## How much of (b) already exists or is already decided

- **Job writes converged 2026-08-13**: every pipeline write from
  boss-gcp goes to the cluster SoR (`BOSS_JOBS_URL=10.20.0.34:7900`)
  since the split-brain incident `c4b4a6b0`.
- **The cadence fold is decided** (protocol-cadence Q1/Q4,
  2026-08-15): cadence rows fold into the dispatcher rules registry
  and window packets become ordinary packets claimed through the
  step CAS. This retires the conductor's private `cadence_rules`
  table on boss-gcp — the last genuine split-brain — and turns the
  conductor into a station consumer whose *location stops
  mattering*.
- **The instance role split is declared** (invariants register,
  2026-08-18): the demo instance runs a declared subset of the
  cluster's protocols, checked daily by the conformance sweep.
- **The first fabric-native compute experience is approved**:
  [dev-node-checkout](./dev-node-checkout.md) — a dev pod checked
  out from the website, presence-gated. That is what "part of the
  fabric" feels like when it works; this doc generalises it from
  dev pods to all compute.
- **Role-claimable steps are at the dock** (car `de6aa212`,
  `feat/steps-claimable-by-role`): a protocol can put a step in a
  queue for a capability instead of nominating an actor — the
  mechanism a machine-capability station needs.

## What is missing

1. **Machines are not Subjects.** No packet can be *about* boss-gcp;
   no sweep can target it by identity; its capabilities (can run
   gates, can reach the forge LAN, hosts the edge) are not data.
2. **IT operations are not protocols.** Six bespoke paths in one
   session (`6796ee5f`): cancel a train, find the jobs API,
   authenticate to it, read a CI failure, repair a broken pipeline,
   reach the runner host. Zero packets, zero events, zero
   discoverability.
3. **Nothing measures the drain.** The agent memory directory is the
   symptom store; nothing tracks which entries a landed protocol
   made obsolete.

## Open questions

### Q1: What is the membership test for "in the fabric"?

Is a machine in the fabric when it is a Subject with a station and
its operations are protocols — or only when its workloads are also
cluster-scheduled?

Proposed: the former. Subject + station + protocol-visible
operations is the whole test; the scheduling substrate (kubelet,
systemd, launchd) stays per-machine, replaceable without touching
the fabric. Option (a) is not foreclosed — a machine that later
joins Kubernetes changes its supervisor, not its identity.

### Q2: How does a machine register, and what keeps its record honest?

A `node` Subject kind (per `bossnet-physical-topology` Q1, already
accepted for dev nodes) with capabilities as Class rows — but what
creates the rows for boss-gcp, the minipc, the Mac, and what notices
when the record drifts from the machine?

Proposed: seed the four as data (they change rarely), and extend the
conformance-sweep pattern: a per-node daily sweep whose inspect step
verifies the declared capabilities against the machine (can it reach
the forge? does the conductor unit exist? disk headroom), closing
the loop the same way `instance-registry-asymmetry-is-declared`
closes it for registries.

### Q3: Where does the conductor run after the cadence fold?

Once window packets are claimed through the CAS, the conductor is a
station consumer that happens to hold a git clone and a forge token.

Proposed: it stays on boss-gcp under systemd, unmoved — the fold
makes its location irrelevant, which is the point. Moving it
in-cluster becomes a one-line choice later, not a prerequisite now.

### Q4: Does the public demo stay a separate BOSS instance?

The demo on boss-gcp is a second BOSS instance with its own
Postgres, which is why a conformance sweep and a declared baseline
exist at all. Folding it into the SoR as a tenant would delete that
whole class of drift — at the cost of putting public traffic on the
system of record.

Proposed: keep it separate. Blast-radius isolation for the public
door is worth one declared, sweep-checked asymmetry. Revisit only
alongside real multi-tenancy work.

### Q5: What retires a memory file?

`6796ee5f`'s thesis needs an acceptance test, or the memory
directory keeps growing beside whatever protocols land.

Proposed: every IT protocol that ships names the memory entries it
supersedes (the way registries are expected to retire the code they
replace), and the count of superseded-but-still-cited entries is the
measure of whether this doc's direction is actually working.
