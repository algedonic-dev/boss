-- Inactive row capability. LOGGED data; no active reader pointer is added.
CREATE TABLE membership_row_epochs (
    identity text NOT NULL,
    epoch uuid NOT NULL,
    state jsonb NOT NULL,
    witness text,
    PRIMARY KEY(identity,epoch)
);
CREATE TABLE membership_row_nodes (
    identity text NOT NULL,
    epoch uuid NOT NULL,
    hash text NOT NULL,
    encoded bytea NOT NULL,
    PRIMARY KEY(identity,epoch,hash),
    FOREIGN KEY(identity,epoch) REFERENCES membership_row_epochs(identity,epoch) ON DELETE CASCADE,
    CHECK (length(hash)=64)
);

CREATE FUNCTION membership_row_header_invalidate() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.state IS DISTINCT FROM OLD.state AND NEW.witness IS NOT DISTINCT FROM OLD.witness THEN
        NEW.witness := NULL;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER membership_row_header_changed BEFORE UPDATE ON membership_row_epochs
FOR EACH ROW EXECUTE FUNCTION membership_row_header_invalidate();

CREATE FUNCTION membership_row_nodes_invalidate() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE membership_row_epochs SET witness=NULL
        WHERE (identity,epoch) IN (SELECT identity,epoch FROM new_nodes);
    ELSIF TG_OP = 'DELETE' THEN
        UPDATE membership_row_epochs SET witness=NULL
        WHERE (identity,epoch) IN (SELECT identity,epoch FROM old_nodes);
    ELSE
        UPDATE membership_row_epochs SET witness=NULL
        WHERE (identity,epoch) IN (
            SELECT identity,epoch FROM old_nodes UNION SELECT identity,epoch FROM new_nodes
        );
    END IF;
    RETURN NULL;
END;
$$;
CREATE TRIGGER membership_row_nodes_inserted AFTER INSERT ON membership_row_nodes
REFERENCING NEW TABLE AS new_nodes FOR EACH STATEMENT EXECUTE FUNCTION membership_row_nodes_invalidate();
CREATE TRIGGER membership_row_nodes_deleted AFTER DELETE ON membership_row_nodes
REFERENCING OLD TABLE AS old_nodes FOR EACH STATEMENT EXECUTE FUNCTION membership_row_nodes_invalidate();
CREATE TRIGGER membership_row_nodes_updated AFTER UPDATE ON membership_row_nodes
REFERENCING OLD TABLE AS old_nodes NEW TABLE AS new_nodes FOR EACH STATEMENT EXECUTE FUNCTION membership_row_nodes_invalidate();
