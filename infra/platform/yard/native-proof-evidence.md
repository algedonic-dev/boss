# Native evidence before a proof

Backlog4196a7b1 adds an opt-in validated mode to the existing protected
GET reader: `boss-sor-read --evidence sponsorship-v1` or
`boss-sor-read --evidence gate-timeout-v1`, or the bounded remote archive
readback `boss-sor-read --evidence gcs-integrity-v1`. Ordinary one-path reads keep
their transport contract. The validated mode holds no credential and
uses that same protected reader for every native request.

The named contract lives under `infra/platform/proof-observers/`.
It binds the observer module, expected packet kind, query, original car,
branch and fixed delay. No caller supplies a URL, identity, JSON evidence
or executable path. The original car's stored merge, its actual commit
epoch and the supplied fixed convergence pair must agree. A later HEAD
does not move the cut. Generic code validates full native identity and
pagination; the separately named observer decides only its declared
event claim. No domain kind dispatch is added to BOSS core.
Contract rows are append-only and named by version. A stored observer
selects that exact version; a new contract never repins its caller.

Exit0 and JSON `observation: seen` witness the declared event only.
Exit1 and `observation: unseen` require native coverage spanning the
declared interval with no gaps. Without that coverage, even a complete
retained inventory returns exit75/UNKNOWN. A lost, refused, malformed or
contradictory native record stays uncertainty. Error diagnostics name
the offending packet. A supported withdrawal before compute is checked
before requiring an executed head; it cannot witness an executed test.

Sponsorship checks actual server admission, kind, real partition and
simulation status. It does not prove consent was copied. Gate timeout
checks server admission/completion, the native launch bound, completion
actor, verdict/head binding, failed check and retained timeout block.
It does not judge quotation correctness. Archive integrity reuses the
same native completion actor/host/disposition and server bounds before
checking its complete remote readback receipt. Plan, mutation, archive
integrity and restore remain separate domain claims.

The build-free preflight executes the shared adversarial controls before
parking, and the full gate repeats them through their owning test.
Existing car probe/observer metadata is not silently rewritten by a
deploy. Migrate a selected stored observer through the supported metadata
door after review, independently reread its exact text, then run its
native read-only acceptance. A metadata-only repair needs no unrelated
source re-gate. Complete original attempts remain in the record; this
source delivery alone clears no proof, alarm or historical finding.

For a closed already-proven original, use the separately reviewed
`boss ops forge observe-proof <contract-name> --wait` read. The contract
is already its registration; no new car metadata or proof replacement is
needed. This explicit invocation runs through the existing verified
probe account, view, protected GET broker and timeout. It takes no
script, URL, actor or cutoff. `run-car-probe` and `--record-probe` keep
their existing owed-proof admission; `--recheck` still reruns the old
proof rather than silently selecting this observation.

The immutable ops result says `protected-proof-observation`, with its
full raw stdout/stderr and actual execution context. Decode stdout as
one complete JSON document: it binds the contract's exact byte SHA256
(the observation plan identity), claim/version, original merge and fixed
epoch, original GET/proof-history hashes, and deployed source head. Whole
selected witness packets include their native admission/completion and
actor/host facts. Query pages are explicit byte/hash references, not a
claim that every unrelated response was archived. Complete old proof and
attempt bodies stay at their original resource and must be independently
conserved by before/after reads during native acceptance. No car, step,
proof, attempt or claim is written on any observation outcome.

The output stays below the runner's existing102400-byte cap. A required
witness or diagnostic that cannot be conserved whole is explicitly
UNKNOWN, with byte/hash references and `complete:false`, never a cropped
positive or a claimed complete diagnostic. This source boundary does not
authorize native execution: ordinary review/convergence and separately
qualified protected native acceptance remain owed. In particular, the
event-only sponsorship and timeout contracts never clear consent-copy or
quotation-correctness claims, and archive readback never proves restore.

A timed-out captured read stays UNKNOWN. Its diagnostic retains the
command and deadline plus each partial stream's exact base64 bytes,
byte count and SHA256 when they fit. Diagnostic `complete:true` means
those captured facts are retained; `process_output_complete:false`
records that the process did not finish. Large independently readable
car metadata becomes an explicit hash reference before discarding any
small captured stream. If the captured streams exceed the same carrier,
their counts/hashes remain with `retained_complete:false` and diagnostic
`complete:false`; no reduced exception message is called complete.
