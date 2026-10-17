# BOSS private collector TLS source boundary

This partial source implements accepted decision 001fe8d0 in the existing
credential broker handler crate. It installs no rule, grant, Secret, listener,
Service, network exception, controller binding or managed login. Parent 3869107c
remains open for protected installation and complete native acceptance.

The fixed target is boss/boss-managed-cache-server-tls. The existing SecretStore
whole-object reader and UID/resourceVersion conditional writer are the only
storage doors. Missing or foreign state refuses; the module cannot create a
Secret or fall back to an unconditional write. Private EC-P256 key material is
minted in process and stays in the named Secret boundary. Debug/errors and
public progress never contain it. One fixed-DNS, one-day server certificate
and its independent public trust are minted per request generation. The root
signing key is discarded, not exposed as a general CA service.

Schema: tls.next, tls.current and tls.previous each hold a strict object with
original Secret UID, generation UUID, private key PEM, server certificate PEM, public trust PEM and
expiry. tls.promoted-at records the broker judgment time, and tls.effect holds
metadata only. tls.retired conserves retired UUIDs, preventing a late request
from minting a different identity under an old generation. At its 1024-generation
audit bound the lifecycle refuses rather than deleting evidence; a later
audit-preserving archive/recovery contract is required before extending it.
Partial, unknown, duplicate and resurrected identities refuse. Every slot retains
the UID observed before staging; prepare, promotion and revocation refuse a
copied slot under a replacement object, including every later retry. A slot
without this original UID refuses; this inert schema has no implicit migration.

Prepare stages only next and verifies the exact whole-object readback. A lost
committed acknowledgement reuses the original pair without reminting. Promotion
requires the protected port's independently observed staged generation, even
on first installation with no current certificate. It compares Secret UID and
version, served leaf PEM SHA256 and installed public-trust SHA256/generation,
then re-reads the unchanged object before CAS. The prior current remains as
previous. Revocation requires the same current positive, the exact previous
generation, complete old-collector absence and expired original token witnesses,
after an overlap outlasting the authoritative 600-second collector token.
Lost acknowledgement or uncertain observation never authorizes replacement.

## Durable original installation and request binding

Lifecycle entry points require an original-bound store. FileBoundStore retains
the complete original empty Secret creation receipt and broker Pod in a separate
owner-only 0700 directory. Fixed records are exclusive, owner-only regular files;
symlinks, hardlinks, partial records and conflicting originals refuse. Each
request reserves a durable intent bound to that installation before minting.
If an acknowledgement is lost, existing matching slots can be replayed. A
reserved request with missing slots refuses rather than reminting. An empty
replacement Secret cannot bootstrap itself, even with the same request UUID.
The original creation resourceVersion is required and compared as an opaque
string. Only that exact original empty object can begin staging. If the same
UID later loses every active slot under another version, both old and fresh
request UUIDs refuse. Deleted keys, blank slot values, and retained effect or
retirement metadata all receive the same refusal. The installer cannot
overwrite the original with a later empty read.

The native Pod reader uses a fixed cluster HTTPS target and bounded responses,
with no proxy, redirects or ambient trust roots. Original and current Pod UID,
complete spec, selected container incarnation, readiness and read-only whole
Secret projection must match before Secret access. Protected public trust
records retain the complete named ConfigMap original, its UID and exact CA
digest, joined to the installation and reserved generation. Effect evaluation
requires that same trust UID, digest and generation.

These adapters are unconfigured source. A protected installer must supply the
original receipts and public trust observations; ordinary lifecycle requests
cannot supply them. Actual broker Pod read authority, protected public-trust
reading and installation remain unpaid. No new reader grant is inferred from
the existence of an HTTP adapter.

## Required protected installation adapter

The ProtectedInstallation port is not configured by this car. It must run in
the existing broker's protected boundary, retain complete original native
observations privately before returning metadata, and use its own broker-local
completion clock. Observations must occur between invocation and completion;
an operation exceeding 60 seconds, an expired certificate, a missing positive
or a future/stale observation refuses.

Independently re-read the exact named Secret object. Bind its UID and version
to the original observation and the broker's original Pod/container/image and
read-only TLS projection. Refuse replacement or restart. The initial installer
must project and serve tls.next before promotion; it cannot require current to
exist or report a copied PATCH as served evidence. Staging remains a separately
authorized installer operation, never an untrusted collector action.

Connect only to boss-managed-cache-collection.boss.svc:7444, with proxy and
ambient roots disabled and the selected generation's public trust. Validate
the fixed hostname and exact served leaf; reconstruct its canonical PEM before
comparing the digest. Public trust reaches only the protected collector plan.
Re-read that plan's exact trust bytes/generation from the protected installation,
not an asserted request field. The existing FileRecords installation.json
continues to bind native controller/credential/image/store metadata and remains
unchanged; it is not proof of TLS projection or listener readiness.

Before revocation, a separately authorized observer must obtain a complete,
stable census of original collector Jobs/Pods for the exact previous generation,
joined to protected registered launch records. Untrusted labels, a truncated
page, missing binding, replaced UID, unresolved create/delete acknowledgement or
unknown expiry refuse. Each original collector must be independently absent,
and the last originally issued Pod-bound token must have expired. Worker denies
and all noncollector routing remain unchanged. Preserve full native inputs,
effects and receipts in the broker-only descriptor-confined store; no private
key/cache bytes enter packets, journals, arguments or access logs.

## Protected observer source and remaining authority

`managed_collector_tls::observer::ProtectedObserver` implements this join as
inert source. It reuses the original-bound store's descriptor and identity
parsers. Every invocation gets its own UUID, local clock and sixty-second
deadline. Whole native HTTP status, headers and raw body, fresh current-trust
observations, canonical served leaf PEM/DER and full TLS errors are retained
privately beside an attributable outcome manifest. An oversized HTTP body is
retained as an explicitly incomplete bounded prefix and cannot qualify.
Original workload, Secret and public-trust reads bracket the operation;
immutable desired trust alone is never evidence that trust is installed.

The direct TLS context loads exactly the selected public root, checks the fixed
hostname and served certificate, and installs neither ambient trust nor a key
logging callback. The native reader uses the existing pinned Kubernetes client
and only fixed namespace/resource paths. It has no list/watch, caller URL,
redirect, proxy, ambient credential lookup or installer side effect.

Retirement consumes a protected registered-launch producer through
`RegisteredCensus`; this car does not install that producer. Its strict
generation-closure record binds the installation, previous generation,
original broker Pod UID, opaque immutable revision and issuance closure time.
Closure means the producer has permanently stopped old-generation issuance,
including unresolved creates, token requests and deletion acknowledgements;
it is not a caller-supplied completeness flag. The record retains each original
Job, owned Pod and original Pod-bound TokenRequest request/reply, with issuance
time and actual returned expiry. Unknown expiry, unresolved operations,
replacement or inconsistent identity, excess token lifetime, malformed record
or changed closure refuses. Two rounds of named native GETs must independently
return exact Kubernetes NotFound objects for every original Job and Pod.
Forbidden, redirects, empty input, terminating or replacement objects cannot
stand for absence. The complete closure is read again and must be unchanged.
The conservative last possible expiry is closure time plus the registry's
fixed token lifetime, also covering an authoritative sealed empty generation.
An empty namespace query supplies no such authority.

The next authorized producer work is still required before native use:

- Resolve the authority gap for the exact original broker Pod GET in `boss`.
  Accepted 001's proposed `boss-dev` reads do not grant this namespace read.
- Resolve the named `boss-dev/boss-managed-cache-public-trust` ConfigMap GET
  and configure its independently attributable fresh current-trust producer.
  A retained installer plan cannot substitute for that current read.
- Implement and authorize the protected registered-launch closure producer,
  conserving all original launch and TokenRequest replies, unresolved
  acknowledgements, stable generation closure and replay-safe publication.
  Neither a label census nor an unapproved list/watch grant can replace it.
- Configure the readers through a separately authorized protected installer;
  prove those exact native authority and producer bindings before enabling
  lifecycle mutations. No credential placement or new grant is made here.

Until those authorities and producers exist, their absence is an explicit
refusal. Accepted 001 and its parent remain open; source controls establish
mechanics rather than native installation or operational qualification.

Fake ports and real synthetic certificate/TLS controls establish source behavior
only. Native adapter installation, issuer rule/registry binding, renewal cadence,
grant convergence, protected projections, listener/Service and collector network
placement, provider enrollment and first-worker READ/USE remain unpaid.
