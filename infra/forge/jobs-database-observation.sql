-- Fixed statistics views only: no application rows, query text or bind values.
WITH activity AS MATERIALIZED (
    SELECT pid, backend_type, application_name, client_addr, datname,
           state, wait_event_type, wait_event,
           EXTRACT(EPOCH FROM clock_timestamp() - xact_start) AS transaction_age_seconds,
           EXTRACT(EPOCH FROM clock_timestamp() - query_start) AS query_age_seconds
    FROM pg_stat_activity
    WHERE
      -- serving-activity begin
      (datname = current_database() OR backend_type <> 'client backend') AND
      -- serving-activity end
      pid <> pg_backend_pid()
), bounded_activity AS (
    SELECT * FROM activity ORDER BY pid LIMIT 128
), wait_groups AS (
    SELECT state, wait_event_type, wait_event, count(*) AS count
    FROM activity GROUP BY state, wait_event_type, wait_event
), backend_io AS (
    SELECT backend_type, object, context, reads, writes, writebacks, extends,
           op_bytes, fsyncs, stats_reset,
           CASE WHEN current_setting('track_io_timing') = 'on' THEN read_time END AS read_time,
           CASE WHEN current_setting('track_io_timing') = 'on' THEN write_time END AS write_time,
           CASE WHEN current_setting('track_io_timing') = 'on' THEN fsync_time END AS fsync_time
    FROM pg_stat_io
), disconnect_settings AS MATERIALIZED (
    SELECT wanted.name, actual.setting, actual.reset_val, actual.source,
           actual.unit, actual.context, actual.pending_restart,
           CASE WHEN actual.name IS NULL THEN 'named PostgreSQL setting unavailable' END AS why
    FROM (VALUES ('client_connection_check_interval'), ('tcp_keepalives_count'),
                 ('tcp_keepalives_idle'), ('tcp_keepalives_interval')) AS wanted(name)
    LEFT JOIN pg_settings actual ON actual.name = wanted.name
)
SELECT jsonb_build_object(
    'observed_at', clock_timestamp(),
    'database', current_database(),
    'server_version_num', current_setting('server_version_num')::integer,
    'server_started_at', pg_postmaster_start_time(),
    'server_port', current_setting('port')::integer,
    'transaction_read_only', current_setting('transaction_read_only'),
    -- These are this observer connection's values, never an app-role limit.
    'observer_session', jsonb_build_object(
        'backend_pid', pg_backend_pid(), 'database', current_database(),
        'user', current_user, 'server_address', inet_server_addr(),
        'server_port', inet_server_port(), 'client_address', inet_client_addr(),
        'transport', CASE WHEN inet_client_addr() IS NULL THEN 'unix-socket' ELSE 'tcp' END,
        'statement_timeout', current_setting('statement_timeout'),
        'lock_timeout', current_setting('lock_timeout'),
        'transaction_read_only', current_setting('transaction_read_only'),
        'meaning', 'observer session only; statement_timeout is not an application-role ceiling'
    ),
    'disconnect_settings', (SELECT jsonb_build_object(
        'state', CASE WHEN bool_and(why IS NULL) THEN 'observed' ELSE 'unavailable' END,
        'rows', jsonb_agg(to_jsonb(row_data) ORDER BY name),
        'meaning', 'setting is this observer session; Unix-socket TCP values may be zero; reset_val is the session RESET default, source/context describe its origin, not a guarantee for other roles or connections'
    ) FROM disconnect_settings row_data),
    'io_timing', current_setting('track_io_timing') = 'on',
    'wal_io_timing', current_setting('track_wal_io_timing') = 'on',
    'io_timing_meaning', 'times are null when current tracking is off; cumulative counters are not request latency',
    'activity_count', (SELECT count(*) FROM activity),
    'activities_limited_to', 128,
    'wait_groups', (SELECT COALESCE(jsonb_agg(to_jsonb(w) ORDER BY state, wait_event_type, wait_event), '[]'::jsonb) FROM wait_groups w),
    'activity_scope', 'sampling_scope identifies selected clients; server background processes are included; no query text',
    'activities', (SELECT COALESCE(jsonb_agg(to_jsonb(a) || jsonb_build_object(
        'blocking_pids', CASE WHEN wait_event_type = 'Lock' THEN pg_blocking_pids(pid) ELSE ARRAY[]::integer[] END
    ) ORDER BY pid), '[]'::jsonb) FROM bounded_activity a),
    -- database-local begin
    'database_io', (SELECT jsonb_build_object(
        'stats_reset', stats_reset, 'blocks_read', blks_read, 'blocks_hit', blks_hit,
        'temp_bytes', temp_bytes, 'deadlocks', deadlocks,
        'block_read_time_ms', CASE WHEN current_setting('track_io_timing') = 'on' THEN blk_read_time END,
        'block_write_time_ms', CASE WHEN current_setting('track_io_timing') = 'on' THEN blk_write_time END
    ) FROM pg_stat_database WHERE datname = current_database()),
    -- database-local end
    'backend_io_scope', 'server-wide all backend types, not database-only or physical-device latency',
    'backend_io', (SELECT COALESCE(jsonb_agg(to_jsonb(i) ORDER BY backend_type, object, context), '[]'::jsonb) FROM backend_io i),
    'wal', (SELECT jsonb_build_object(
        'source_view', 'pg_stat_wal', 'stats_reset', stats_reset,
        'wal_records', wal_records, 'wal_fpi', wal_fpi, 'wal_bytes', wal_bytes,
        'wal_buffers_full', wal_buffers_full, 'wal_write', wal_write, 'wal_sync', wal_sync,
        'wal_write_time_ms', CASE WHEN current_setting('track_wal_io_timing') = 'on' THEN wal_write_time END,
        'wal_sync_time_ms', CASE WHEN current_setting('track_wal_io_timing') = 'on' THEN wal_sync_time END
    ) FROM pg_stat_wal),
    -- The declared server is PostgreSQL16. Its checkpoint counters live in
    -- pg_stat_bgwriter; referencing the PG17-only view would lose all samples.
    'checkpointer', (SELECT jsonb_build_object(
        'source_view', 'pg_stat_bgwriter',
        'pg_stat_checkpointer_available', FALSE,
        'pg_stat_checkpointer_unavailable', 'PostgreSQL16 exposes checkpoint counters in pg_stat_bgwriter',
        'counters', jsonb_build_object(
            'checkpoints_timed', checkpoints_timed, 'checkpoints_req', checkpoints_req,
            'checkpoint_write_time_ms', checkpoint_write_time,
            'checkpoint_sync_time_ms', checkpoint_sync_time,
            'buffers_checkpoint', buffers_checkpoint, 'stats_reset', stats_reset
        )
    ) FROM pg_stat_bgwriter)
);
