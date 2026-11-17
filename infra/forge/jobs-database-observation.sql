-- Fixed statistics views only: no application rows, query text or bind values in this file; the verb's masked statement text comes from jobs-database-statements.sql.
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
), memory_settings AS MATERIALIZED (
    -- The settings that decide when a sort or hash spills to a temporary
    -- file and where it lands (backlog 891e48c9). A superuser-only row is
    -- absent from pg_settings for any other role, so it reads unavailable.
    SELECT wanted.name, actual.setting, actual.unit, actual.source,
           actual.context, actual.pending_restart,
           CASE WHEN actual.name IS NULL THEN 'named PostgreSQL setting unavailable' END AS why
    FROM (VALUES ('hash_mem_multiplier'), ('log_temp_files'), ('shared_preload_libraries'),
                 ('temp_tablespaces'), ('work_mem')) AS wanted(name)
    LEFT JOIN pg_settings actual ON actual.name = wanted.name
), statements_source AS MATERIALIZED (
    -- The pg_stat_statements view errors unless the library is preloaded,
    -- so this file never names it; jobs-database.py reads the fixed
    -- jobs-database-statements.sql only when this says present.
    SELECT (SELECT extversion FROM pg_extension WHERE extname = 'pg_stat_statements') AS extension_version,
           (SELECT 'pg_stat_statements' = ANY(string_to_array(replace(replace(setting, ' ', ''), '"', ''), ','))
            FROM pg_settings WHERE name = 'shared_preload_libraries') AS library_preloaded
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
    'memory_settings', (SELECT jsonb_build_object(
        'state', CASE WHEN bool_and(why IS NULL) THEN 'observed' ELSE 'unavailable' END,
        'rows', jsonb_agg(to_jsonb(row_data) ORDER BY name),
        'meaning', 'server values as this observer session reads them; work_mem and hash_mem_multiplier can differ per role, database, session or transaction (the membership reads SET LOCAL work_mem), so they are defaults, not what every statement ran at'
    ) FROM memory_settings row_data),
    'statements_source', (SELECT jsonb_build_object(
        'state', CASE WHEN extension_version IS NOT NULL AND library_preloaded THEN 'present' ELSE 'absent' END,
        'extension_version', extension_version,
        'library_preloaded', library_preloaded,
        'why', CASE WHEN extension_version IS NULL THEN 'pg_stat_statements extension is not installed in this database'
                    WHEN library_preloaded IS NULL THEN 'shared_preload_libraries is unreadable to this role'
                    WHEN NOT library_preloaded THEN 'pg_stat_statements is not in shared_preload_libraries; its view errors until the server restarts with it' END
    ) FROM statements_source),
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
        'temp_bytes', temp_bytes, 'temp_files', temp_files, 'deadlocks', deadlocks,
        'block_read_time_ms', CASE WHEN current_setting('track_io_timing') = 'on' THEN blk_read_time END,
        'block_write_time_ms', CASE WHEN current_setting('track_io_timing') = 'on' THEN blk_write_time END
    ) FROM pg_stat_database WHERE datname = current_database()),
    -- pg_stat_user_indexes is built from the catalogs, so an index the
    -- statistics store holds no entry for still reads idx_scan 0, never null
    -- (review 38174765 F1). Only pg_stat_have_stats tells the two apart.
    'audit_log_indexes', (SELECT jsonb_build_object(
        'state', CASE WHEN count(*) > 0 AND bool_and(x.stats_present) THEN 'observed' ELSE 'unavailable' END,
        'why', CASE WHEN count(*) = 0 THEN 'public.audit_log indexes absent or unread'
                    WHEN NOT bool_and(x.stats_present)
                    THEN 'no cumulative statistics entry for '
                         || string_agg(x.index_name::text, ', ' ORDER BY x.index_name) FILTER (WHERE NOT x.stats_present)
                         || '; the view reads a missing entry as idx_scan 0, which is not observed non-use' END,
        'stats_reset', (SELECT stats_reset FROM pg_stat_database WHERE datname = current_database()),
        'rows', COALESCE(jsonb_agg(jsonb_build_object(
            'schema_name', x.schema_name, 'table_name', x.table_name,
            'index_name', x.index_name, 'index_oid', x.index_oid,
            'indisvalid', x.indisvalid, 'stats_present', x.stats_present,
            'idx_scan', CASE WHEN x.stats_present THEN x.idx_scan END
        ) ORDER BY x.index_name), '[]'::jsonb),
        'meaning', 'fixed public.audit_log indexes only; observed only when every index has a cumulative statistics entry, and idx_scan is null for an index without one; cumulative scan starts, not planner choice or read benefit; zero is not a day of non-use; stats_reset may be unknown and per-index counters can also reset'
    ) FROM (SELECT n.nspname AS schema_name, t.relname AS table_name, c.relname AS index_name,
                   c.oid::bigint AS index_oid, i.indisvalid, s.idx_scan,
                   pg_stat_have_stats('relation', (SELECT oid FROM pg_database WHERE datname = current_database()),
                                      i.indexrelid) AS stats_present
            FROM pg_index i
              JOIN pg_class t ON t.oid = i.indrelid
              JOIN pg_namespace n ON n.oid = t.relnamespace
              JOIN pg_class c ON c.oid = i.indexrelid
              LEFT JOIN pg_stat_user_indexes s ON s.indexrelid = i.indexrelid
            WHERE n.nspname = 'public' AND t.relname = 'audit_log') x),
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
