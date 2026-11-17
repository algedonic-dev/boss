-- An automation names its host station (backlog 6ecb5480; design
-- 4b27a65a, docs/design/station-tree.md section 6: "Automations get a
-- host station (station = <its station id>)"). The stable id of a
-- standing tree station; nullable, because a row the registry already
-- holds keeps what the instance recorded (the seed inserts, never
-- updates: design e187198f), and the bundle pin
-- every_automation_names_a_standing_station_of_the_tree holds every
-- declared row to one.
ALTER TABLE automation_actors ADD COLUMN IF NOT EXISTS station text;
