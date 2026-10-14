-- Design 2ec0c3e4: a present observation preserves stored historical
-- claims; it does not fabricate past creation events. Source preparation
-- only: applying this vocabulary does not capture or migrate a manual.
-- c58245f6 supplies the exact declarations missing from source car ed869884.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('content.manual.baseline_observed', 'content', 'A privileged actor observed a complete raw manual inventory now: stable admission identity, observation time and actor, full section/history fields and serial identities, counts, canonical hash, sequence state and protected log cutoff', NULL),
  ('content.manual.section_created', 'content', 'One committed manual creation: resulting section, new history version and stored history identity', NULL),
  ('content.manual.section_updated', 'content', 'One committed manual update: resulting section, new history version and stored history identity', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
