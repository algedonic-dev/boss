-- The ten statements that wrote the most temporary-file blocks (backlog
-- 891e48c9), from pg_stat_statements. jobs-database.py runs this fixed
-- file ONLY when jobs-database-observation.sql reports the extension
-- installed in this database and its library preloaded: the view errors
-- otherwise, and that state is reported there as absent.
-- Normalized text only, never a bind value: pg_stat_statements already
-- stores constants as $n. What it keeps verbatim (a utility statement's
-- literals, a dollar-quoted body, a comment) is masked here, server-side,
-- because the verb retains psql's native stdout: a mask applied after
-- this file would leave the unmasked copy in the record. The 'meaning'
-- line below states exactly what is masked, in the words the verb's
-- about repeats (pinned by
-- the_verb_contract_states_the_statement_text_it_prints_and_its_mask).
-- Review c5ce84a8 F3 measured the earlier single regexp leaving four
-- shapes unmasked, so the text is now LEXED, left to right, one token
-- per regexp_matches row, the way PostgreSQL's own scanner reads it:
--   1 a double-quoted identifier, kept (schema, not data), so a quote
--     inside it cannot shift the parity of the strings after it;
--   2 a word, kept, so an identifier ending in e is never read as an E
--     prefix and foo$bar$ is never read as a dollar quote;
--   3 an E-string, backslash escapes honoured;
--   4 a standard string with a backslash in it, masked to the END of the
--     text: it reads differently under standard_conforming_strings on
--     and off, and either reading is inside what is masked;
--   5 a standard string (also the body after U&, B, X or N);
--   6 a dollar-quote opener and the entire remaining text;
--   7 its tag (a capture, not a token);
--   8 a block-comment opener and the entire remaining text;
--   9 a line comment, to CR or LF;
--  10 any other single character, kept.
-- PostgreSQL's regexp takes the longest match at the leftmost position,
-- which makes the order above a lexer before the first conservative
-- opener. Review be75935a found that closing at a LAST delimiter can
-- consume a later string's opening quote or a different dollar tag,
-- printing its body. Masking the entire suffix avoids interpreting
-- nesting or a closing delimiter beyond the 2048-character input cut.
-- Strings and E-strings that never close are masked to the end too.
WITH top AS MATERIALIZED (
    SELECT s.queryid, s.dbid, s.calls, s.temp_blks_read, s.temp_blks_written,
           s.total_exec_time, s.query
    FROM pg_stat_statements s
    WHERE s.temp_blks_written > 0
    ORDER BY s.temp_blks_written DESC, s.queryid
    LIMIT 10
)
SELECT jsonb_build_object(
    'state', 'observed',
    'database', current_database(),
    'transaction_read_only', current_setting('transaction_read_only'),
    'block_size', current_setting('block_size')::integer,
    'info', (SELECT jsonb_build_object('dealloc', dealloc, 'stats_reset', stats_reset)
             FROM pg_stat_statements_info),
    'limited_to', 10,
    'query_text_limited_to', 240,
    'query_text_lexed_to', 2048,
    'rows', (SELECT COALESCE(jsonb_agg(jsonb_build_object(
        'queryid', t.queryid, 'database', d.datname, 'calls', t.calls,
        'temp_blks_read', t.temp_blks_read, 'temp_blks_written', t.temp_blks_written,
        'total_exec_time', t.total_exec_time,
        'query', left(regexp_replace(masked.text, '\s+', ' ', 'g'), 240),
        'query_withheld', NULL
    ) ORDER BY t.temp_blks_written DESC, t.queryid), '[]'::jsonb)
    FROM top t LEFT JOIN pg_database d ON d.oid = t.dbid
    CROSS JOIN LATERAL (
        SELECT string_agg(CASE WHEN token.m[3] IS NOT NULL THEN 'E''?'''
                               WHEN token.m[4] IS NOT NULL OR token.m[5] IS NOT NULL THEN '''?'''
                               WHEN token.m[6] IS NOT NULL THEN '$?$'
                               WHEN token.m[8] IS NOT NULL OR token.m[9] IS NOT NULL THEN '/*?*/'
                               ELSE COALESCE(token.m[1], token.m[2], token.m[10]) END,
                          '' ORDER BY token.k) AS text
        FROM regexp_matches(left(t.query, 2048),
            '("(?:[^"]|"")*")|((?:[A-Za-z_]|[^\x01-\x7f])(?:[A-Za-z0-9_$]|[^\x01-\x7f])*)|([eE]''(?:[^''\\]|\\.|'''')*\\?(?:''|$))|(''(?:[^''\\]|'''')*\\.*)|(''(?:[^''\\]|'''')*(?:''|$))|(\$((?:(?:[A-Za-z_]|[^\x01-\x7f])(?:[A-Za-z0-9_]|[^\x01-\x7f])*)?)\$.*)|(/\*.*)|(--[^\n\r]*)|(.)',
            'g') WITH ORDINALITY AS token(m, k)
    ) masked),
    'meaning', 'cumulative since info.stats_reset, server-wide across databases and roles; temp_blks are block_size blocks; total_exec_time is milliseconds; only statements that wrote temporary blocks; normalized pg_stat_statements text, whitespace collapsed and truncated to 240 characters; query text is lexed from its first 2048 characters and masked: single-quoted string bodies (standard, E, U& and B/X forms) and line comments are replaced by ? markers; the first dollar-quote or block-comment opener masks the rest of the text; a backslash inside a standard string masks the rest of the text; line comments end at CR or LF; double-quoted identifiers are kept'
);
