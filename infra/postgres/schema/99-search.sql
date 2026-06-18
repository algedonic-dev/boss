-- =========================================================================
-- 99-search.sql — Cross-domain full-text search function (applies LAST).
-- =========================================================================


-- -----------------------------------------------------------------------------
-- Cross-domain full-text search function
-- Used by the Omnibox and /api/people/search endpoint.
-- Defined last so every referenced table exists when the function is
-- created on a fresh schema apply.
-- -----------------------------------------------------------------------------

CREATE OR REPLACE FUNCTION search_all(query text, max_results integer DEFAULT 10)
RETURNS TABLE(
    entity_type text,
    entity_id   text,
    label       text,
    detail      text,
    path        text,
    rank        real
) LANGUAGE sql STABLE AS $$
  SELECT * FROM (
    SELECT 'employee'::text AS entity_type, e.id AS entity_id, e.name AS label,
           e.role || ' — ' || e.department AS detail,
           '/people/' || e.id AS path,
           ts_rank(to_tsvector('english', e.name || ' ' || e.email || ' ' || e.role || ' ' || e.department),
                   plainto_tsquery('english', query))::real AS rank
    FROM employees e
    WHERE to_tsvector('english', e.name || ' ' || e.email || ' ' || e.role || ' ' || e.department)
          @@ plainto_tsquery('english', query)
       OR e.name ILIKE '%' || query || '%'
       OR e.id ILIKE '%' || query || '%'
    UNION ALL
    SELECT 'model'::text, m.sku, m.name, m.manufacturer || ' — ' || m.category,
           '/catalog/' || m.sku,
           ts_rank(to_tsvector('english', m.name || ' ' || m.manufacturer || ' ' || m.sku || ' ' || m.category),
                   plainto_tsquery('english', query))::real
    FROM asset_models m
    WHERE to_tsvector('english', m.name || ' ' || m.manufacturer || ' ' || m.sku || ' ' || m.category)
          @@ plainto_tsquery('english', query)
       OR m.name ILIKE '%' || query || '%'
       OR m.sku ILIKE '%' || query || '%'
    UNION ALL
    SELECT 'asset'::text, d.asset_id, d.asset_id, d.sku || ' — ' || d.phase,
           '/assets/' || d.asset_id,
           (CASE WHEN d.asset_id ILIKE query || '%' THEN 1.0 ELSE 0.5 END)::real
    FROM assets d
    WHERE d.asset_id ILIKE '%' || query || '%'
    UNION ALL
    SELECT 'account'::text, c.id, c.name, c.city || ', ' || c.state || ' — ' || c.tier,
           '/accounts/' || c.id,
           ts_rank(to_tsvector('english', c.name || ' ' || c.director || ' ' || c.city),
                   plainto_tsquery('english', query))::real
    FROM accounts c
    WHERE to_tsvector('english', c.name || ' ' || c.director || ' ' || c.city)
          @@ plainto_tsquery('english', query)
       OR c.name ILIKE '%' || query || '%'
       OR c.id ILIKE '%' || query || '%'
    UNION ALL
    -- Open service tickets: id prefix match ranks highest (operators
    -- type "tkt-1234" → go straight to the ticket), summary full-text
    -- search second (operators type "cooling alarm" → find every
    -- open ticket matching that failure mode).
    SELECT 'ticket'::text, t.ticket_id, t.ticket_id,
           t.summary || ' — ' || t.asset_id,
           '/service/' || t.ticket_id,
           GREATEST(
             CASE WHEN t.ticket_id ILIKE query || '%' THEN 1.0 ELSE 0.0 END,
             ts_rank(to_tsvector('english', t.summary || ' ' || t.ticket_id),
                     plainto_tsquery('english', query))
           )::real
    FROM asset_open_tickets t
    WHERE t.ticket_id ILIKE '%' || query || '%'
       OR to_tsvector('english', t.summary || ' ' || t.ticket_id)
          @@ plainto_tsquery('english', query)
    UNION ALL
    -- Active bulletins: title-prefix match ranks highest, title+body
    -- FTS second. Expired bulletins drop out. Audience filtering is
    -- the reader surface's job — search is permissive.
    SELECT 'bulletin'::text, b.id::text, b.title,
           left(b.body, 100),
           '/me',
           GREATEST(
             CASE WHEN b.title ILIKE query || '%' THEN 1.0 ELSE 0.0 END,
             ts_rank(to_tsvector('english', b.title || ' ' || b.body),
                     plainto_tsquery('english', query))
           )::real
    FROM bulletins b
    WHERE (b.expires_on IS NULL OR b.expires_on >= CURRENT_DATE)
      AND (b.title ILIKE '%' || query || '%'
           OR to_tsvector('english', b.title || ' ' || b.body)
              @@ plainto_tsquery('english', query))
    UNION ALL
    -- Published manual sections: FTS over title+body, slug prefix
    -- match for operators who remember the URL.
    SELECT 'manual_section'::text, m.slug, m.title,
           left(m.body, 100),
           '/manual/' || m.slug,
           GREATEST(
             CASE WHEN m.slug ILIKE query || '%' THEN 1.0 ELSE 0.0 END,
             ts_rank(to_tsvector('english', m.title || ' ' || m.body),
                     plainto_tsquery('english', query))
           )::real
    FROM manual_sections m
    WHERE m.published = true
      AND (m.slug ILIKE '%' || query || '%'
           OR to_tsvector('english', m.title || ' ' || m.body)
              @@ plainto_tsquery('english', query))
  ) results
  ORDER BY rank DESC
  LIMIT max_results
$$;

