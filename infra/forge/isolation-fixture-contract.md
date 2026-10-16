# BOSS synthetic isolation fixture

Source authority: design `a21c491c-6a72-4698-804d-65cc38e68f9c`, R1,
accepted by David on 2026-10-15. This purpose supplies one native synthetic
receiver; it neither activates isolation nor qualifies the full matrix.
Parents `2710c8fc-94e6-47d1-bee0-b4ec81706d0a` and
`b3071bf4-5996-4cf8-96db-e4b23b2f67a1` retain their remaining obligations.

## Published operations

The existing versioned ops registry admits only these forge operations:

* `prepare-isolation-fixture RUNTIME NONCE HEAD HELPER_SHA256 IMAGE_SHA256 LIFETIME_SECONDS`
* `observe-isolation-fixture ORIGINAL_PREPARE_REQUEST`
* `retire-isolation-fixture ORIGINAL_PREPARE_REQUEST`

Invoke through `boss ops forge VERB ... --wait` after actual publication,
recorded quiescence and a fresh registered runtime executor admission.
The runtime must be an active, own-started builder of the isolation child.
Do not run these commands merely because a source gate passed. Existing
forge rights must answer positively; this purpose cannot grant them.
The driver resolves the original ops packet, active executor, accepted R1,
original rollout authorization and complete native executor roster itself.
Names, namespace, listener, helper argv and selectors are derived internally.

`HEAD` names the protected published forge generation. The image digest
must equal its installed OCI platform manifest; the existing verified image
control archive must contain the exact helper bytes. The initial source
generation must match. Receiver execution checks its actual helper hash,
DownwardAPI Pod UID, image/container identity and complete native objects.
This source pin is not a claim that an image has already been published.

## Effect and evidence boundaries

The receiver has no service-account token, credential/host/PVC mounts,
host/process namespaces, init or extra container. It updates only synthetic
counters in a bounded memory emptyDir. There is no forwarding to the real
system of record, Kubernetes API, provider or arbitrary URL. Asserted user,
role or machine headers confer no authority: requests without a valid
synthetic proof refuse. A valid synthetic proof is accepted even when those
asserted headers accompany it. A generated synthetic authority stays in
the receiver; accepted one-attempt signatures may be conserved without
exporting that authority. Replays cannot produce another effect.

The authenticated forge executor runs the fixed helper in the exact
receiver Pod. Its positive, unsigned, forged and replay controls target
the Service DNS derived from the original request, not a localhost-only
substitute. A failed Service path cannot qualify the fixture. The candidate
does not receive synthetic authority. Actual worker traffic and effects
must subsequently be compared with the full receiver originals; source
controls and a ready receipt cannot substitute for that execution.

`/var/lib/boss/isolation-fixture` conserves append-only full intent/state
records with original acknowledged UIDs. The separate protected invocation
root retains complete argv, nonsecret input, stdout, stderr and actual exit
from byte one. No tail is a receipt. HTTP reads require actual status200;
a refused redirect or failed read is never an empty collection.

Observe copies the complete snapshot into the protected file store and
independently compares actual readback bytes before returning a bounded
artifact reference. Retire does the same before any deletion, sends native
UID preconditions for the original Service and Job, independently observes
absence, then conserves the final records. Compression retains every byte;
evidence exceeding the existing file-store limit refuses deletion. Expiry,
TTL, missing originals and acknowledgements alone cannot prove teardown.

Lost create acknowledgement, replacement UID, unavailable or truncated
evidence, an expired fixture or a second unresolved request remains
UNKNOWN. No generic relaunch, guessed adoption or intent-clearing door is
provided. Identical ready-request replay rechecks the original incarnation
and returns its original receipt; it never re-creates the receiver.

Inventory reads the same protected original journal through a confined,
read-only descriptor and compares acknowledged UID plus complete conserved
spec and identity. A prefix, label, copied annotation or same-shaped
replacement does not declare an object. A reader without the original
forge journal reports UNKNOWN. The purpose adds no ConfigMap or other
native declaration object and changes no orphan-deletion authority.

## Unpaid runtime qualification

Publication, actual forge rights, coordinated quiescence, provision,
first-worker binding, each direct/routed READ/USE negative, exposed trusted
positive, Unix-socket/file-canary and executable category, recovery and
observed teardown still require native receipts. This receiver supplies
only its fixed HTTP effect families. No worker egress, selector, LAN mode,
grant, provider account or private credential is changed by this source.
