-- A zero queue-depth cooldown means depart on clear track, without a
-- fixed batching delay (613295dc). Depth/interval remain positive and
-- every basis retains its exclusive parameter shape. The registry's
-- check_rule and its native table-agreement corpus pin this boundary.
ALTER TABLE cadence_rules DROP CONSTRAINT cadence_rules_params_check;
ALTER TABLE cadence_rules ADD CONSTRAINT cadence_rules_params_check CHECK (
    (basis = 'wall'
        AND every_minutes IS NOT NULL AND every_minutes > 0
        AND at_times IS NULL AND min_dock_depth IS NULL
        AND cooldown_minutes IS NULL
        AND cadence IS NULL AND anchor_date IS NULL AND business_calendar IS NULL)
 OR (basis = 'clock'
        AND at_times IS NOT NULL
        AND every_minutes IS NULL AND min_dock_depth IS NULL
        AND cooldown_minutes IS NULL
        AND cadence IS NULL AND anchor_date IS NULL AND business_calendar IS NULL)
 OR (basis = 'queue-depth'
        AND min_dock_depth IS NOT NULL AND min_dock_depth > 0
        AND cooldown_minutes IS NOT NULL AND cooldown_minutes >= 0
        AND every_minutes IS NULL AND at_times IS NULL
        AND cadence IS NULL AND anchor_date IS NULL AND business_calendar IS NULL)
 OR (basis = 'calendar'
        AND cadence IS NOT NULL
        AND anchor_date IS NOT NULL
        AND at_times IS NOT NULL AND jsonb_array_length(at_times) = 1
        AND every_minutes IS NULL AND min_dock_depth IS NULL
        AND cooldown_minutes IS NULL)
);
