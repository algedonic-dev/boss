-- Existing generations retain NULL until a publication validates their actual
-- header and rows. A missing witness never authorizes a prepared delta.
ALTER TABLE membership_projection_generations ADD COLUMN basis_digest text;

-- Older binaries do not supply this witness. Invalidate once per affected
-- identity and statement, never once per row. Native publication already owns
-- the global publication lock and header lock before touching these rows.
CREATE FUNCTION invalidate_membership_row_basis() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE membership_projection_generations SET basis_digest=NULL
        WHERE basis_digest IS NOT NULL AND identity IN (SELECT identity FROM new_rows);
    ELSIF TG_OP = 'DELETE' THEN
        UPDATE membership_projection_generations SET basis_digest=NULL
        WHERE basis_digest IS NOT NULL AND identity IN (SELECT identity FROM old_rows);
    ELSE
        UPDATE membership_projection_generations SET basis_digest=NULL
        WHERE basis_digest IS NOT NULL AND identity IN
            (SELECT identity FROM old_rows UNION SELECT identity FROM new_rows);
    END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER invalidate_membership_insert_basis AFTER INSERT ON membership_projection_rows
REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION invalidate_membership_row_basis();
CREATE TRIGGER invalidate_membership_update_basis AFTER UPDATE ON membership_projection_rows
REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION invalidate_membership_row_basis();
CREATE TRIGGER invalidate_membership_delete_basis AFTER DELETE ON membership_projection_rows
REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION invalidate_membership_row_basis();

CREATE FUNCTION invalidate_membership_header_basis() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.header IS DISTINCT FROM OLD.header
        AND NEW.basis_digest IS NOT DISTINCT FROM OLD.basis_digest THEN
        NEW.basis_digest := NULL;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER invalidate_membership_header_basis BEFORE UPDATE OF header ON membership_projection_generations
FOR EACH ROW EXECUTE FUNCTION invalidate_membership_header_basis();
