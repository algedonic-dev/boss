-- GPT-6 Sol's configured fallback can be admitted as its own model
-- (backlog 936d1a83; agent-run 8be0e8f2, 2026-10-04).
--
-- MEASURED: boss dispatch --model gpt-6-sol refused before claim;
-- neither the live card nor agent_spec::known_models priced that name.
-- Naming gpt-6.1-sol instead would misattribute a fallback execution.
-- This adds its own row without changing any model default, budget
-- requirement, unknown-model refusal, or existing migration.
--
-- SOURCE: https://developers.openai.com/api/docs/models/gpt-6-sol,
-- fetched 2026-10-04. Standard tier, short context (<=272K input),
-- USD per million tokens: $2 input / $0.20 cached input / $2.50 cache
-- writes / $10 output. Columns below are USD micro-units per million
-- tokens, not cents, per-token rates, or Codex credits.
--
-- These API prices are a USD proxy for Codex execution, not evidence
-- of this workspace's invoice or actual spend. Codex credit billing
-- is a separate unit; no credit-to-dollar conversion is asserted.
-- Above 272K input tokens, the source doubles input and cache rates
-- and multiplies output by 1.5 for the full request. The existing card
-- has one rate per model, so this preserves its short-context floor;
-- it does not claim to price long context or other processing tiers.
-- No blend: total-only usage stays unpriced, as on the existing card.
--
-- agent_spec reads this migration at compile time; its schema-directory
-- and database equality tests keep native admission and pricing equal.

INSERT INTO agent_rate_card (model, input_usd_micros_per_mtok, output_usd_micros_per_mtok, cache_read_usd_micros_per_mtok, cache_write_usd_micros_per_mtok, note) VALUES
  ('gpt-6-sol', 2000000, 10000000, 200000, 2500000, 'OpenAI GPT-6 Sol fallback — API Standard short-context USD proxy per MTok: $2 input / $10 output / $0.20 cached input / $2.50 cache writes; <=272K input, long-context floor only; not actual Codex credit spend or an invoice (developers.openai.com/api/docs/models/gpt-6-sol, fetched 2026-10-04; backlog 936d1a83)')
ON CONFLICT (model) DO NOTHING;
