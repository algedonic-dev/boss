-- Generic SKU catalog foundation (d4cd56b7). Legacy goods rows and
-- finished_product_inventory retain their schema, keys and facts.
-- Business SKU/Class rows are tenant protocol data, never inserted here.
CREATE TABLE product_sku_versions (
    sku TEXT NOT NULL,
    version BIGINT NOT NULL CHECK (version > 0),
    data JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (sku, version),
    CHECK (jsonb_typeof(data) = 'object'),
    CHECK (data ->> 'sku' = sku),
    CHECK ((data ->> 'version')::BIGINT = version)
);

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain)
VALUES
  ('products.sku.version.appended', 'products', 'A complete immutable SKU catalog version', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
