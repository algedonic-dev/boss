-- 694c4ba8: factory routing is a declaration, never an inference from
-- an agent's UI identity or role. Existing unspecified rows retain their
-- prior eligibility. Rotation changes use the existing registry publish
-- door, which records agent.updated, never a projection-only UPDATE.
ALTER TABLE agents ADD COLUMN factory_rotation boolean NOT NULL DEFAULT true;
