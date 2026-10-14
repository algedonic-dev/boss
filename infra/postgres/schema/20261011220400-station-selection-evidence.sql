-- Full owner observations are carried in bounded facts in the SAME claim
-- transaction. The final v2 manifest binds their order and exact receipt bytes.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.station.selection_evidence', 'jobs', 'An immutable bounded part of a station selection receipt, closed by its claim manifest', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
