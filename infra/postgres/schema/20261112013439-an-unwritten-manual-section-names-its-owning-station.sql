-- A safe manual index names unfinished work without publishing its body
-- (design 778de0c5 D2, backlog 270736f7). `owner_station` is the station
-- tree's id of the station that owns the section (factory-taxonomy-v2:
-- a row names its owner by station, not by department); `authoring_job_id`
-- is the packet writing it. Legacy sections declared neither; NULL is unknown.
ALTER TABLE manual_sections
    ADD COLUMN IF NOT EXISTS owner_station TEXT,
    ADD COLUMN IF NOT EXISTS authoring_job_id UUID;
