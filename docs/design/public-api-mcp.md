# Design: the public API as a surface for other people's agents

**Status**: approved — in-review — open questions tracked at `/system/design` (2026-08-14).
**Source**: feedback `78207000` — "publish the 'public' API somewhere so
that people can let their individual agents code up tools, while our API
protects them. That API should probably be MCP compliant given mindshare,
even though we want a robust API to ensure guardrails of the system model
are adhered to."
**Related**: [human-powered-state-machine.md](./human-powered-state-machine.md) ·
[schema-migrations.md](./schema-migrations.md)

---

## The tension, named

The ask contains its own tension: MCP compliance is about *reach* —
any agent framework can consume an MCP server — while the guardrails
are about *constraint* — every write passes policy, workflow
validation, and the audit log. The resolution is the reading frame the
repo already holds: **agents are additional CPUs in the same machine,
not a separate system.** An external agent doesn't get a side door;
it gets an actor identity, the same alphabet of legal transitions, and
the same provenance obligations as a human operator or the sim
workforce. MCP is a transport for that, not an exemption from it.

The existence proof is already running: the brewery sim operates the
entire company through the public API as the workforce, enforced by
`infra/lint/api-path-bypass-smell.sh` (no direct-DB end-arounds), and
the OS map showed 3,633 simulated vs 63 real hand-offs over one
11-hour wall-clock run. The public API is demonstrably sufficient to
*run the company*. What it lacks is a way for an outside agent to
discover it, authenticate to it, and hold a legible contract with it.

## What exists today, measured

- **423 route registrations** across the workspace; **82** on the
  gateway itself; ~24 services proxied behind the gateway by path
  prefix. Every proxied route requires a valid session
  (`proxy.rs::has_valid_session`).
- **No machine-readable API description anywhere.** No OpenAPI, no
  utoipa, no schemars in any Cargo.toml. The de facto contracts are
  the `*-client` crates and `apps/web/src/{domain}/types.ts` —
  compiled, not published.
- **No machine authentication at the edge.** The gateway is the
  browser edge: session cookies in, inbound `x-boss-*` identity
  headers stripped. Terminal tooling (`boss-step.sh`,
  `feedback-queue.sh`) goes *around* the gateway to service ports on
  loopback precisely because no token path exists. An external agent
  cannot authenticate to BOSS today at all.
- **The model is already self-describing as data.** 31 Workflows, 21
  subject kinds, 225 classes, 501 policy rules, and the StepType
  registry are all queryable through the API. An agent that can read
  the registries knows what work exists, what transitions are legal,
  and what fields a step requires at completion — no OpenAPI needed
  for the *model* layer.

That last point is the load-bearing one. BOSS's registries-over-code
principle (CLAUDE.md §9) means most of what an agent must learn is
data it can fetch, not prose someone must maintain. Only the thin
generic layer — how to open a Job, claim and complete a Step (PATCH
semantics, metadata merge!), read a Subject, search, send a message —
needs documenting by hand, and that layer is a dozen route families,
not 423 routes.

## Proposed shape

Three layers, buildable in order, each useful without the next:

**1. Publish the contract.** A hand-curated reference for the generic
layer (the four primitives' routes + search + messages + registries),
pinned against the router by an equality test so it cannot drift
(CLAUDE.md §9a — a doc that restates the router gets a test that
fails naming the route when they disagree). The registries document
the rest of themselves; the reference's job is to say *that* and show
the reading order. Published in-repo and served at `/system/kb`.

**2. A `boss-mcp` server** — a new Tier-1 crate, generic over the
state machine, no tenant nouns. Speaks MCP's streamable-HTTP
transport at a gateway-served path (`/mcp`). Its tools are **the
primitives, not route wrappers**: on the order of a dozen tools
(`list_my_work`, `read_job`, `claim_step`, `complete_step`,
`read_subject`, `search`, `send_message`, `describe_workflow`,
`list_registries`, …) whose descriptions carry the when-to-use
guidance agents actually need. Each tool call is implemented as a
plain call through the public HTTP API *as the acting identity* — the
MCP server is an adapter in front of the gateway, so policy, workflow
validation, required-at-done metadata, and audit provenance apply
identically whether the caller is a browser, the sim, or someone's
agent. Guardrails stay server-side; nothing depends on which tools a
client chose to load.

A dozen primitive-shaped tools also serves every MCP client equally —
sophisticated clients that could handle hundreds of generated tools
via deferred loading gain little, and simple clients choke. The
state-machine alphabet is small; the tool set should be too.

**3. Agent identity.** MCP's ecosystem convention is OAuth-style
bearer tokens (hosted servers reject native API keys). BOSS's mapping:
a token minted for an agent, bound to a responsible human — the same
Q7 rule that every Job names a human owner. The gateway resolves
token → actor; the actor id (`agent:<name>` acting for
`emp-<who>`) lands in provenance; policy scopes evaluate against the
binding. This is the piece with real design decisions left — see Q2.

## What this deliberately is not

- Not OpenAPI generation for 423 routes. High effort, permanent
  drift surface, and it documents the wrong layer — the tenant
  modules' routes are reachable *through* the primitives (a Job about
  an invoice leads to the invoice), and an agent gripping the
  primitives doesn't need the long tail enumerated.
- Not a second write path. Every mutation stays behind the same
  policy gate and lands in the same audit log; the MCP server holds
  no privileged connection to anything.

## Open questions

All 5 open questions were resolved 2026-08-14 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q5: What compatibility does a published tool contract promise? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Publishing is a promise. Tool schemas follow the same
> expand/contract convention as the schema layer
> ([schema-migrations.md](./schema-migrations.md)): additive changes
> freely, destructive changes in two steps, roll-forward only. Whether
> the cluster's N-1 window (`ad2e28ab`) extends to "someone's agent
> built last month keeps working" — and for how long — is a policy
> decision that should be written down with the first published
> version.

Agreed


### Q3: Is the playground's MCP endpoint public? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> "Point your agent at the brewery" is the strongest possible demo of
> the human-powered-state-machine pitch, and the demo tenant's data is
> disposable by design. But it means anonymous or guest-tier agent
> traffic: rate limits, abuse handling, and a guest policy scope that
> keeps writes inside the demo tenant. Decide before the endpoint
> exists, not after.

Can we make it public but require getting a token via Guest access that we put behind a captcha to prevent automated abuse?


### Q1: Are tools the primitives only, or primitives plus per-Workflow specializations? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> The floor is the ~dozen generic tools. The registries could also
> *generate* per-Workflow tools (`open_expansion_purchase`, schema from
> `metadata_schema`) at MCP list-tools time — more legible to agents,
> zero hand-maintenance, but a bigger tool list and a moving one as
> Workflows are authored. The generic floor ships first either way.

Let's do primitives plus per-Workflow specializations


### Q2: Does an agent act as its human, or as itself? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Two models: a delegate token inherits the binding human's policy
> scope wholesale (simple; provenance says who to talk to; blast radius
> = that person's blast radius), or agents are first-class actors with
> their own roles in the Class registry (finer control; matches
> "agents are CPUs"; more machinery). Backlog item `afe54132` — an
> agent cannot be named as the initiator of an employee change — is
> already pressing on this from the audit side.

An agent acts as itself, a human acts only as the human. An agent can carry an authorization from the human but we want to know the agent was the actual actor.


### Q4: Where is the canonical published copy? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> In-repo reference + `/system/kb` page + the MCP server's own
> tool/resource descriptions are three copies of one fact. Proposal:
> the in-repo reference is canonical; the KB page renders it; the MCP
> descriptions are generated from it at build time; one equality test
> pins the set. Needs an owner and a mechanism, not a comment.

in-repo is canonical

## Decision history

_None yet._
