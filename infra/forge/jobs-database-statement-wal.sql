-- Fixed text-free statement counters; optional extension, never query text.
WITH statement_rows AS MATERIALIZED (
    -- false suppresses stored SQL at the PostgreSQL source, before retention.
    SELECT dbid::text AS dbid, userid::text AS userid, queryid::text AS queryid,
           toplevel, calls::text AS calls, wal_records::text AS wal_records,
           wal_fpi::text AS wal_fpi, wal_bytes::text AS wal_bytes
    FROM pg_stat_statements(false)
), statement_count AS (SELECT count(*) AS total FROM statement_rows)
SELECT jsonb_build_object(

        'observed_at', clock_timestamp(),
        'server_started_at', pg_postmaster_start_time(),
        'server_version_num', current_setting('server_version_num')::integer,
        'database', current_database(),
        'database_oid', (SELECT oid::text FROM pg_database WHERE datname = current_database()),
        'state', 'observed', 'source', 'pg_stat_statements(false)',
        'total', (SELECT total FROM statement_count), 'limit', 128,
        'returned', (SELECT LEAST(total, 128) FROM statement_count),
        'omitted', (SELECT GREATEST(total - 128, 0) FROM statement_count),
        'extension_version', (SELECT extversion FROM pg_extension WHERE extname = 'pg_stat_statements'),
        'stats_reset', (SELECT stats_reset FROM pg_stat_statements_info),
        'dealloc', (SELECT dealloc::text FROM pg_stat_statements_info),
        'role', current_user, 'labels', 'UNKNOWN; query identifiers are not statement labels',
        'rows', (SELECT COALESCE(jsonb_agg(to_jsonb(s) ORDER BY dbid, userid, queryid, toplevel), '[]'::jsonb)
                 FROM (SELECT dbid, userid, queryid, toplevel, calls, wal_records, wal_fpi, wal_bytes
                       FROM statement_rows ORDER BY wal_bytes::numeric DESC, dbid, userid, queryid, toplevel LIMIT 128) s),
        'meaning', 'bounded cumulative endpoint counters; no query text, no per-entry reset continuity, not a global delta ranking'
    );
