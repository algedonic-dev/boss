// Test adapter: display fixtures carry explicit native JSON numeric evidence.
import type { CarProof } from './yard';
import { parseProofJobsJSON } from './proof-json';
import type { ProofNumbers } from './proof-numbers';

export function proofFixture(over: Partial<CarProof> = {}): CarProof {
  const proof: CarProof = { probe: null, expect: null, event: null, attempt: null, stamped: null, ...over };
  const a = proof.attempt;
  const metadata = { proof_probe: proof.probe, proof_event: proof.event,
    ...(a === null ? {} : { proof_attempt: a.observer?.evidence ?? { at: a.at, exit: a.exit, why: a.why },
      waits_on: a.observer?.declaration }) };
  const job = (parseProofJobsJSON(JSON.stringify({ data: [{ metadata }] })) as {
    data: { metadata: Record<string, unknown>; proofNumbers: ProofNumbers }[];
  }).data[0]!;
  return { ...proof, facts: { metadata: job.metadata, numbers: job.proofNumbers, ownerId: null, openedAt: null, steps: [] } };
}
