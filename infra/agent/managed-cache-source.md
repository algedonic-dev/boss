# Private managed cache source checkpoint

Accepted decision 6f3ef63c keeps cache bytes in the private broker boundary.
The coordination service sees metadata only. This checkpoint supplies confined
filesystem persistence, the existing one-store lifecycle over authenticated
metadata IPC, and a worker copy into its private writable run home. It does not
install these adapters or activate native managed launches.

The filesystem adapter pins directory descriptors, refuses links, foreign owners,
non-private permissions and special files, holds an exclusive OS lock, and saves
immutable transition originals before an atomic state rename and directory fsync.
An error after a committed write is never treated as rollback. Leased, quarantined
and collected states remain unavailable after restart. Collection retains the
exact original privately with a private digest witness. Reconciliation validates
that original and the same principal; it still does not publish Ready.

The IPC vocabulary carries run, credential, generation, nonce and transition
metadata. It has no actor assertion, cache path, provider bytes, terminal claims
or release operation. The existing broker workload verifier checks a bounded
RS256 workload token; this adapter additionally pins signed ServiceAccount UID
and Pod UID. A separate broker scope port must resolve the registered active run,
its authenticated controller, issuer, principal, generation and independently
observed native incarnation. Signature membership and an enrollment receipt do
not supply that authority. No scope implementation or production listener is
installed. Parser, transport and private verifier failures expose fixed errors.

The worker accepts only a bound metadata plan matching its run and downward-API
Pod UID. It pins the read-only issuer projection, copies its fixed auth.json leaf
into a new private regular 0600 file, fsyncs it, and leaves the issuer unchanged.
Its private CODEX_HOME can then be renewed by the native harness. The controller's
existing managed-launch guard still refuses: preparing this helper does not
satisfy publication, collection, recovery or native installation obligations.

## Remaining source and native placement contract

The existing broker is in namespace boss; retained run PVCs are in boss-dev.
A PVC cannot be mounted across those namespaces. The existing recovery Job's
read-only/no-login/no-ServiceAccount-token posture is preserved. Its UID and
complete manifest contract can judge a collector, but its journaled Exec channel
must never carry private auth bytes: the native transport preserves stdout and
request originals. This source does not add a token or broker mount to that Job.

A reviewable scoped next placement is a trusted private collector adapter in
boss-dev, separate from the untrusted worker and outside coordination journaling.
It must conserve the originally created PVC UID, original run/Job/Pod/container
identity, terminal and retention evidence, fixed mount path, Unix owner and exact
read-only mount intent. The private adapter must attest that its local descriptor
actually belongs to that volume; a matching pathname or existing directory cannot
prove this. Metadata IPC must authenticate the controller workload independently
of the worker, with publisher-owned endpoint, audience, signed ServiceAccount UID
and broker-owned registered-run scope. No caller supplies these placements.

The precise native collector identity, narrow workload-token projection and
private mount/transport authority are not installed or granted here. They need a
concrete scoped authority review and issuer/broker implementation; no credential
is placed by hand. The ScopePort, supported account/format PrivateVerifier,
private mount attestation, issuer installation, exact durable publication/recovery
and controller acknowledgement wiring remain actual SOURCE obligations. Opaque
receipt references from the separate private human enrollment workflow never
stand in for consent, installation, generation readiness or these effects.

The manifests and adapter activation remain OFF. Fixture success proves source
behavior only. Real provider consent, broker installation, credential grants,
native account verification, runtime READ/USE and isolation probes remain held.
