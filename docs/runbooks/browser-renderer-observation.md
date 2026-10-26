# Attended browser renderer evidence

BOSS distinguishes attended browser observations from machine execution receipts.
This protocol records what a signed-in person submitted, then requires a separate
executor to review the complete originals. It never exports the browser session
or treats a client pass flag as trusted execution.

After the source has converged and the registry rows have been published, file a
`browser-renderer-observation` packet naming the car in the published contract.
The observe step resolves that contract itself. A person explicitly starts the
fixed synthetic suite, reviews every assertion and intercepted request, then
explicitly submits the full renderer, suite and report attachments. The opaque
frame has no network or ambient session access. A trusted opaque wrapper fences
the inner frame's document navigation with `frame-src`; a data-only policy canary
must be refused before any pinned script executes. The wrapper relays only its
own child's bounded, nonce-bound messages. Failed runs can be retained as
incomplete evidence; they cannot qualify. Reloads and new attempts use new packets.

The contract (`infra/step-plugins/browser-renderer-contract.json`) pins the
renderer the gateway actually serves from main, because the observe step refuses
any served byte that differs from the pin. `source_head` is the head the named
car's gate vouched for; `asset_head` is the main commit the pinned renderer bytes
come from. They differ once a later train edits `answer-question.js` (train
#1279 did, 2026-10-20), and then an observation qualifies the renderer as served
under that car's claim, not the car's gated bytes, which nothing serves any more.
Every such edit fails the one named check
`the_served_renderer_and_suite_are_the_bytes_the_browser_contract_pins` until
the contract and its test fixture are re-pinned.

The observation is the signer's key (`writer = "signer"` on the observe step's
field, with `sign_offs_required = ["platform-admin"]`): both metadata doors accept
it only from a verified gateway session of a person holding that sign-off role,
and refuse every machine-door caller, whatever id it sends. The same session then
stamps the step and completes it. Qualification refuses an observation whose
pinned field is not signer-written, so a packet filed under an older row cannot
qualify. The row declares a writer, so the drift publish holds it by default: it
goes live only at a deliberate `boss workflow publish`.

A different person or registered agent reviews the original attachments and runs:

```sh
boss browser qualify OBSERVATION_UUID --findings-file review.md --accept-attended
```

The adapter independently reads the immutable observation, authenticated observer,
exact source and suite bindings, attachment listing and whole bytes. It refuses
self-review, incomplete cases, failed assertions and unexpected synthetic writes.
It records an immutable `attended-browser-independent-review` qualification and
completes only the qualification step. The qualification, and the acceptance
recorded on the car, name both heads: `source_head`, and `asset_head` with the
pinned `asset` the browser actually ran.

Accepting this assurance for a delivered car is a separate explicit action. The
car must have been deliberately converted to a published `ship-a-change` version
containing the distinct `attended-qualified` terminal. Publishing does not move
an existing packet. First inspect `boss job convert CAR_UUID --to vN --dry-run`,
then record the conversion through the supported door when authorized. A v9 car,
draft protocol or unmatched qualification is refused by the acceptance adapter.

```sh
boss browser accept CAR_UUID --observation OBSERVATION_UUID --accept-attended
```

Acceptance rechecks both immutable originals and every attachment. It records
their exact event and digest links on the new terminal before setting its marker,
then verifies closure with outcome `attended-qualified`. The original `proven`
step's metadata, receipts and completion evidence remain intact. BOSS's ordinary
`boss prove` path and its machine-run execution requirement remain unchanged.

Source tests, a green gate and this runbook do not constitute a human browser
observation, registry publication, packet conversion or qualification. Those acts
leave their own records when actually performed.
