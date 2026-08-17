# Design: an agent's read path to the cluster

**Status**: approved — every question answered in packet `f2f09077`; carried to a file 2026-08-17.

**Origin.** David, 2026-08-16: *"I think it makes sense then to have
kubectl on GCP as a backup?"* — asked after an investigation into
"why can't I see my own feedback" stalled because no host an agent
can reach could read the cluster's employee roster.

## The gap, measured

There is no automated path to the cluster. Not a firewall problem and
not a routing problem — a credentials problem, which is smaller and
fixable today.

| probe | result |
|---|---|
| `10.20.0.10:6443` (k8s API, VIP) | **answers** — clean `401 Unauthorized` |
| `10.20.0.34:7900` (jobs API) | open; this is how every packet read tonight was done |
| `10.20.0.34` ports 4443 / 8080 / 443 / 80 | closed |
| VIP 443 / 4443 | closed |
| kubeconfig on boss-gcp (`~/.kube`, `~/.talos`, `/etc/kubernetes`) | none |
| `kubectl` on boss-gcp or the forge host | not installed |

So the cluster is listening and simply does not know the caller.
boss-gcp already reaches the LAN over WireGuard.

**What it cost.** An agent investigating packet `e287bb62` read the
employee roster from boss-gcp's *local* Postgres — the only one
reachable — found no `emp-david`, and published a root cause that had
to be retracted. The right database was unreadable and the wrong one
was not.

**What it still blocks.** Whether an `emp-david` employee row exists
on the cluster. `classifyProbe` resolves a session by looking up
`employee_id` in the roster; a miss with a non-guest role yields
`unrecognized`, which MePage renders as a bare line with no
watchlist and no queues. Until the cluster roster is readable, nobody
can say whether David's board renders.

## The exposure this would touch

boss-gcp is internet-facing: public IP `34.45.110.40`, host firewall
`INPUT policy ACCEPT`, and services bound to `*:443`, `*:4222`
(NATS), `*:8222`. Measured from outside, only port 22 is reachable —
the GCP firewall is the real perimeter, and the host's open policy is
being carried by it.

`SECURITY.md` §Deployment trust model describes one trust boundary,
`boss-gateway`, with backend services trusting the `x-boss-user`
header verbatim and per-service auth still unlanded. It says nothing
about cluster credentials. Putting one on this box is a change to
that model and should be written into it rather than left implicit.

## The shape of an answer

A **read-only, namespace-scoped ServiceAccount**, not the admin
kubeconfig. `get` / `list` / `watch` on the `boss` namespace answers
every question this investigation needed. The token lives on
boss-gcp, which is the box that already runs the conductor and
already reaches the LAN.

"Backup" undersells it. If David's laptop holds the only kubeconfig,
this is not redundancy — it is the first automated path, and the
absence of one is why cluster questions currently route through him.

## Open questions

## Decision history

Reviewed as packet `f2f09077` on 2026-08-16; the packet carried the
prose and the questions, so this file is the residue rather than the
precondition. Answers verbatim:

**Q1 — Read-only ServiceAccount, or the admin kubeconfig?**

A read-only, namespace-scoped ServiceAccount. get/list/watch on the
`boss` namespace answers every question tonight's investigation
needed, and boss-gcp is internet-facing with SSH open — it is about to
hold access to the newly-authoritative system of record. The admin
kubeconfig would make a compromise of that box a compromise of the
cluster. Against the proposal: read-only cannot restart a wedged pod,
so a genuine incident still routes through David's laptop. That is the
right trade for now — the gap being closed is a READ gap.

**Q2 — Does it get `pods/portforward`?**

Yes, and it should be decided rather than inherited, because it is the
verb that turns 'read the cluster' into 'reach anything in it'. The
concrete need is real: the people API is not exposed outside the
cluster, so reading the roster means port-forwarding to it. The honest
alternative is exposing the people service on the LAN the way 7900
already is — narrower in privilege, wider in attack surface, and it
needs a manifest change rather than an RBAC line. Recommend port-
forward, and note that `create` on pods/portforward is not a read verb
however it is framed.

**Q3 — Where does the token live, and what rotates it?**

A file on boss-gcp readable only by the service user, referenced by
KUBECONFIG in the units that need it — the same posture as /etc/boss-
train/forge.token, which already works and which operators already
know. Rotation is the part with no answer today: the forge token has
no rotation story either, and adding a second unrotated credential
makes that gap worth naming rather than doubling silently. Bounded
ServiceAccount tokens expire by default in modern Kubernetes, which
turns rotation from a discipline into a deadline — worth taking.

**Q4 — Does SECURITY.md's trust model change, or just gain a paragraph?**

Gains a paragraph, and the paragraph matters. The current model names
ONE boundary, boss-gateway, and describes backends trusting x-boss-
user verbatim. A cluster credential on boss-gcp is a second kind of
authority on the same box that the document does not currently
acknowledge exists. Not a rewrite: the gateway is still the boundary
for BOSS's own traffic. But 'this host also holds a scoped cluster
credential, and here is its blast radius' belongs in the document that
a person reads before exposing BOSS.

