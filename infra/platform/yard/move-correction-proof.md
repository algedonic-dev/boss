# Historical move correction proof

Urgent parent c8d22e63-fc9b-4617-91ca-8d5ab771d7c0 and landed defect
6b8e4e5a-0394-46f8-96ef-9380fc898c4b implement the accepted exact-diagnosis
decision a34bc69e. Native histories are diagnosis inputs; a correction also
requires an independent completed machine execution. Source delivery does
not constitute historical clearance.

The registered-agent-handoff-v2 model binds a cause by its exact audit
sequence, event identity, kind and native audit insertion time (created_at).
The mover uses that insertion time; producer Event.timestamp is a different
clock and cannot substitute. Missing or mismatched insertion time remains
unknown. Matching the cause does not supply a missing run admission or relax
the historical cutoff. A fresh diagnosis uses a new request identity and the
versioned source fingerprint; retained older diagnoses and raw moves remain
unchanged.

Correction input names request_id, diagnosis_id and proof_request only.
BOSS resolves proof_request through the existing JobsRepository: a closed
ops-request for forge/run-car-probe, its completed execute step, its pinned
protocol, its exact named artifact car and that car's completed proven step.
Execution output, disposition and exit_code must declare the existing
runner:ops writer in both the pinned protocol and materialized fields.
Unprotected legacy execution records refuse. The new registry source is
held pending deliberate publication and native positive/refusal controls.
No live protocol is repinned by this source change.

The successful unattended proof producer emits one final
boss-prove-receipt frame containing schema=1, the executor's actual resolved
artifact UUID and proof_json carrying the exact complete proof JSON string it
recorded. A later edit of a request's arguments cannot redirect that envelope.
Probe-authored framing, duplicate frames, clipping, missing context, an
override, unsuccessful execution and a different car proof refuse. The
known tree head is required; tree_standing=unreadable means the converged
view has no remote-tracking reference and does not excuse an unreadable
execution context. The full observed head must match the artifact's recorded
merge reference. The raw protected output, recorded proof and all resolved
packet/step/protocol snapshots remain in the correction receipt.

Proof stdout is exactly one JSON object with schema=1 and these fields:
diagnosis_id, original_seq, original_hash, packet, cause_event_id, cause_seq,
model, artifact_head and result. The UUID, canonical original hash, packet,
audit cause and diagnosis model must equal the retained diagnosis. The
artifact_head is the machine-observed full tree head, and result is
false-interpretation. Unknown fields, incomplete output or another original
refuse. A source-presence probe does not produce these observations.

The actual probe must inspect the corrective artifact and the exact original
historical inputs. A command that merely prints proposed bindings is not a
qualification. Use only the existing approved low-privilege probe/read doors;
this source supplies no new verb, grant, provider operation or correction
execution authority.

Admission copies resolved records instead of following mutable references
later. Current fault readers validate the conserved proof and diagnosis;
legacy missing proof stays unknown and keeps driving the raw crossing.
Replay retains the original receipt, including unsupported historical models,
without rerunning an effect or rewriting a raw row. Withdrawals remain
append-only. Parent c8 stays open for deliberate producer publication,
actual artifact-bound machine proof, bounded correction admission and fresh
region/alert observations. No historical correction is performed by the
source tests or comprehensive gate.

## Native admission observations

Design 6d996b25 adds evidence beside an exact diagnosis through POST
`/api/yard/moves/{seq}/diagnoses/{diagnosis}/admission-observations` with
only `request_id`. GET the same path followed by that UUID reads the whole
stored observation under the same named-actor/global diagnosis authority.
The server resolves packet identities from the original; it captures one
repeatable-read audit snapshot and retains native admission sequence,
insertion time, event identity and complete event payload. A maximum
sequence is a snapshot bound, not a committed-history watermark.

The original diagnosis is copied whole. When newly read cause-cutoff
slices equal its retained inputs, a receipt-internal reference and hash
retain those identical originals once. Disagreed slices are copied
separately and reported as disagreement. Later admissions never become
historical state at the old cutoff. Absent-in-snapshot and duplicate
admission observations supply no registration, route or clearance proof.

Observations have their own immutable event/projection so adding reader
evidence leaves the diagnosis model's source fingerprint unchanged. Replay
uses copied evidence without live reads. Same-command retries return the
first immutable observation; a new observation requires a new UUID.
The two-million-byte receipt bound and the existing outbox event ceiling
both refuse whole oversized records; neither truncates evidence. Native
observation, any justified interpretation model, independent machine proof
and historical correction remain separate operations.
