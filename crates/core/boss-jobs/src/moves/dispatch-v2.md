`dispatch.rs` is the immutable dispatcher input of the deployed historical
diagnosis model `registered-agent-handoff-v2:da0c4a6ddef6fa9120bf8bc663c5d37bb13c1437d2106425d7cb28848a2c5bdf`.
Its exact bytes include the original tests because the original model hashed
the whole file. It is model-version data; it is not compiled as the dispatcher.

The compiled implementation lives in `dispatch_live.rs`, wired explicitly by
`moves.rs`, and its tests live in `dispatch_tests.rs`. `handoff_model_source`
pins the entire live production prefix to the canonical input and requires
one exact test-module declaration as the live file's suffix. Missing, repeated
or extended suffixes refuse; production appended after tests cannot hide.
A production change requires an explicit model
version decision. The existing whole-model fingerprint pin continues to cover
the other five Rust inputs and handoff registry bytes without normalization.

This fixed artifact cannot collapse into the editable test harness while
preserving the deployed content address. The separation repairs the actual
combined-train failure (urgent c8d22e63, 2026-10-15): adding a required field to
a test constructor changed the identifier despite unchanged production logic.
Neither existing diagnosis receipts nor historical findings are rewritten.
