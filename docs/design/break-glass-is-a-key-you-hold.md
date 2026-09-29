# Break-glass is a key you hold

**Status**: in-review — Q1–Q6 ride the design packet "Break-glass is
a key you hold" (`e9703d8f`, filed 2026-09-03) with proposals
attached; the review step is assigned.

Break-glass authority moves from a secret on a disk to a hardware
security key in the operator's pocket. The emergency path into BOSS
stops being *something the system stores* and becomes *something only
a person can physically do* — a touch on an attested, device-bound
passkey — verified by whichever layer is still standing when the
emergency is real. (David, 2026-09-03: "I really want the break glass
protocol to be based on putting a passkey onto a security key.")

## Where break-glass authority lives today

Identity on this network is already passkey-only: passwords are not
an accepted factor, and the normal login is a passkey ceremony at the
IdP ([idm-kanidm.md](idm-kanidm.md)). The one deliberate exception is
break-glass — local auth via `credentials.toml`, reachable with curl
at `POST /api/auth/login`, deliberately a non-button so nobody drifts
back to it. It exists so an IdP outage cannot lock operators out, and
that reasoning is sound and stays.

What is wrong with it is its *substance*, not its existence:

- It is the last secret at rest in the auth story. idm-kanidm.md
  itself names the blast radius of losing that credential class.
- It lives on the `boss-auth` PVC — an RWO volume that is one of the
  two named blockers keeping the deployment on `strategy: Recreate`
  (the manifest's own comment), which priced every converge as an
  outage window on 2026-09-02.
- Possession of a string is the whole ceremony. Anything that can
  read a file can be the operator.

## What 2026-09-02 taught about verifiers

The boot-brick outages produced a working inventory of the emergency
levers and — the part that matters here — *which verifiers survive
which outages*:

| lever | verifier | survives |
|---|---|---|
| rollout undo | kube-apiserver | BOSS stack down, IdP down |
| emergency merge to forge main | Forgejo | BOSS stack down, IdP down |
| break-glass web session | boss-gateway | IdP down — but not stack down, and only while Cloudflare's edge, the Access mail and the tunnel are up (see "What this door does not cover") |

The design principle that falls out: **each lever gets
hardware-key-gated at its own verifier**, not at a central authority
that may be part of the outage. A break-glass that routes through the
thing that is down is a fiction, which is the same reasoning
idm-kanidm.md used to keep local auth local.

## The design

### The gateway becomes the break-glass verifier

boss-gateway grows a minimal WebAuthn relying party (the `webauthn-rs`
crate — the same author and idiom as Kanidm, so both halves of the
auth story stay in one dialect) with exactly one registered
credential: a **device-bound passkey on a hardware security key**,
enrolled by David. `POST /api/auth/login` with a password body is
replaced by a challenge/assertion ceremony.

The stored material is the credential's public key, sign counter, and
AAGUID. Nothing secret rests anywhere:

- A leaked break-glass store is a leaked *public key* — harmless by
  construction, retiring the credential-class blast radius
  idm-kanidm.md worries about.
- Public material needs no RWO volume. The credential record moves to
  a ConfigMap (in-tree manifest; the repo's public mirror can carry a
  public key without ceremony), and the `boss-auth` PVC retires —
  which removes RollingUpdate blocker #1. The agreed sequence
  (David, 2026-09-03) is: converge auto-rollback first (landed as
  `feat/converge-rolls-back-a-brick`), then this design, then
  RollingUpdate.

Device-bound is enforced, not assumed: registration requires
attestation, so a synced software passkey (iCloud/Google password
manager) cannot enroll. "A passkey on a security key" means the
private key is born on the hardware and cannot leave it; the
attestation statement is what proves that at enrollment time.

### Below the gateway: the same key, other applets

When the gateway itself is down — the 2026-09-02 case — WebAuthn has
no relying party to talk to. The levers below it are gated by the
same physical key through verifiers that were still standing that
night:

- **Rollout undo.** The break-glass kubeconfig's client certificate
  keeps its private key on the security key's PIV applet:
  non-exportable, PIN + touch per use, verified by kube-apiserver.
  A stolen kubeconfig file is inert without the hardware.
- **Emergency merge.** Forgejo already supports passkey login for
  the merge click. The *approval artifact* — the thing the
  post-mortem's break-glass protocol requires David to produce — is
  an SSH signature from an `sk-ed25519` key (FIDO2-backed, resident
  on the same hardware) over the gate-receipt sha: a
  hardware-touch-proven, permanently verifiable record that the
  operator authorized this exact tree state. The audit log gets a
  cryptographic fact instead of a chat transcript.

One key, three applets (FIDO2, PIV, sk-SSH), three verifiers, zero
shared secrets.

### What deliberately does not change

- The IdP remains the only normal door. Break-glass stays
  understated — reachable, documented, not a button.
- The break-glass session's *authority* is unchanged by this doc
  (see Q4 for whether it should be).
- The refusal posture: a break-glass attempt that fails verification
  fails loudly, like every other refusal in the system.

## What this door does not cover

Decided on design `c5ce1aeb` (David, 2026-09-29, all three questions
accepted as proposed; backlog `a15a1cd2`). Until then this document
said the web session survives "IdP down" and never named what else it
routes through, so a reader would have believed the key opens BOSS when
the edge or the mailbox is what broke — the fiction the principle above
refuses.

**The key reaches the gateway by exactly one road**, and every hop on
it must be standing. Each fact is read from the file that decides it:

| hop | why the door needs it | read from |
|---|---|---|
| Cloudflare's edge | the only certificate for boss.algedonic.dev is the edge's; the gateway's one LAN address is 10.20.0.30, port 80, plain http, and the Caddy TLS front with its DNS-01 certificate was deleted 2026-09-17 (`21c17ebc`) | `infra/cluster/manifests/boss.yaml` |
| the Access one-time code mailed to the operator | one Access application covers the WHOLE host with one allow policy; the only bypass in the file is playground's `/auth` | `infra/cluster/dns/access.toml` |
| the in-cluster tunnel connector | the edge reaches the gateway only through it | `infra/cluster/manifests/cloudflared-config.yaml` |
| the gateway | it is the relying party: one rp_id and one origin, https://boss.algedonic.dev | `break_glass_webauthn` in `crates/core/boss-gateway/src/break_glass.rs` |

A browser on http://10.20.0.30 is no way round: it is not a secure
context and will not run the ceremony, and its origin is not the one
both records are bound to. Nor is an Access bypass on `/break-glass`
alone: the ceremony POSTs to `/api/auth/break-glass/assert/*`, and the
session it opens is an HttpOnly cookie used on every other path, all
still behind Access — the key would be touched and the next page
refused. Only a bypass of the whole host removes the mailbox, and that
exposes every gateway route to the internet while keeping the edge and
the tunnel on the road; it was rejected.

**So this is the door for a lockout inside BOSS** — policy, the roster,
the IdP — while those four hops stand. Both enrolled keys were proven
through it on 2026-09-29 (two `auth.login.succeeded` events with
`method = break-glass`, 01:36Z and 01:37Z). It is **not** the door for
a dark edge, a dark mailbox or a dark cluster; those have roads that
owe nothing to Cloudflare:

| failure | road | read from |
|---|---|---|
| the edge or Access dark, or the one-time code not arriving | the dev workspace's ssh door on the LAN, `dev-ssh` at 10.20.0.35:22, which keeps a mounted `authorized_keys` as the fallback for a day the edge is down | `infra/estate/doors.toml`, `infra/cluster/manifests/boss-dev.yaml` |
| the same, from off the LAN | the WireGuard hub on boss-gcp (overlay 10.99.0.0/24), then the LAN road | `infra/cluster/wireguard/setup-hub.sh` |
| the gateway or the stack down | roll to the last known-good build at kube-apiserver, as operator (§Below the gateway) | `infra/cluster/manifests/boss-break-glass-operator.yaml` |
| a fix must land while the system of record is down | the emergency merge lane on the forge | `infra/platform/workflows/emergency-merge.toml` |

What that leaves, said plainly: a lockout inside BOSS at the same time
as a dark edge or mailbox has **no web road**; the repair comes from the
cluster side. And a road on this list is only as good as its last use —
the re-entry sheet's design (`125d405d`) derives this door's
"THIS ROAD NEEDS" line from the same three files, and prints NOT
EXERCISED ON RECORD for a road nobody has walked. The sheet owns that
line; this section is not restated there.

**The Apple Passkey Delegate** — the role that holds a phone passkey
for a one-person company (DR readiness `62dac114`, item 4), named as a
role and never as a person (David, 2026-09-29, backlog `41c5ddaf`) —
takes the ordinary edge road, never this one. A phone passkey is the
kind of authenticator this door's enrolment refuses (synced, or not
hardware-attested), so the Apple Passkey Delegate's credential is never
a break-glass record. And the edge road is not yet theirs either: the
Access policy admits only the operator's address, so the Apple Passkey
Delegate has no road in until item 4's design decides what they may do
and how it is revoked — including which of the roads above they may
walk. The re-entry sheet carries the role as its own road
(`apple-passkey-delegate` in `infra/recovery/re-entry.toml`), which
reads who the edge admits rather than stating it.

**A second road to this same door is decided and not built**: a LAN
TLS listener serving the gateway under the SAME origin,
https://boss.algedonic.dev, with its certificate from a local CA whose
root never enters the cluster or the forge. It needs the cluster and
the gateway pod, and not Cloudflare, the mailbox or the tunnel; the
enrolled keys work on it unchanged. The decision — where the root
lives, which devices trust it, and why it waits behind DR readiness
`62dac114` — is in
[docs/architecture-decisions.md](../architecture-decisions.md)
§Policy & auth, "The break-glass key is the door for a lockout inside
BOSS". Until that road is built and walked, the table above is the
whole list.

## Costs, said out loud

- The curl-able break-glass becomes a browser ceremony; WebAuthn
  does not fit a bare terminal. Q3 decides whether a terminal
  fallback (sk-SSH signature exchanged for a session ticket) is
  worth its surface.
- Break-glass gated on one losable physical object is a lockout
  waiting to happen. Two keys minimum — primary carried, backup in a
  safe — both enrolled (Q2). Enrollment and hardware are David's
  domain per the token-admin rule; the agent builds the ceremony and
  verifies by effect.
- `webauthn-rs` is a real new dependency in the gateway. It is the
  cost of not hand-rolling signature verification, which is not a
  place to be original.

## Open questions

### Q1: Attestation policy — allowlist or any-hardware?
Enforcing attestation keeps software passkeys out. Should enrollment
further pin an AAGUID allowlist (only the exact key models David
owns), or accept any attested roaming authenticator? Allowlist is
tighter; any-hardware survives buying a different brand of backup
key without a code change.

### Q2: Backup-key ceremony
Enroll both keys at setup (two credential records, either
sufficient), or enroll-on-loss (backup key is enrolled only when the
primary is declared lost)? Both-upfront means the safe key works the
moment it is needed; enroll-on-loss means a stolen safe key is inert
but recovery depends on a working enrollment path during an
incident.

### Q3: Terminal fallback
Keep a browserless break-glass (an `ssh-keygen -Y` signature over a
gateway-issued nonce, exchanged for a session ticket), or accept
browser-only and lean on the PIV/SSH levers for terminal cases? The
fallback doubles the ceremony surface; its absence bets that a
browser is always reachable when the gateway is.

### Q4: Break-glass session scope
`credentials.toml` today issues platform-admin-equivalent claims.
Should the passkey session carry the same authority, or a narrower
break-glass role (deploy rollback, merge approval, auth
administration — and nothing else)? Narrower is safer; same-scope is
simpler and matches what break-glass has always meant here.

### Q5: Where the credential record lives
ConfigMap in `infra/cluster/manifests/` (in-tree, public mirror
carries a public key — cryptographically fine) versus a Secret
(out-of-tree, consistent with how every other sensitive object is
referenced by name). The material is public; the question is whether
*policy* wants all auth-adjacent objects handled one way regardless
of secrecy.

### Q6: PVC retirement sequencing
Retire `boss-auth` in the same car that lands the WebAuthn RP, or
one train later after the new ceremony is proven in prod? Same-car
is one clean cut; a soak respects that this is the only emergency
door while it is being replaced.
