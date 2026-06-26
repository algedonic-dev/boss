-- =========================================================================
-- 01-registries.sql — Registries — SubjectKind, Class taxonomy, Locations, Calendar reservations.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- Class registry (Subject classification taxonomy)
--
-- A Class is a logical grouping of Subjects, identified by a stable
-- `code` within a `subject_kind`. See docs/design/primitive-vocabulary.md
-- for the full framing. Classes are *not* Subjects — no independent
-- lifecycle, no Jobs, no events. They define a membership predicate
-- over Subjects of a given kind plus display + grouping metadata.
--
-- Roles ('ceo', 'service-tech', …) are Classes of Employee Subjects.
-- AccountTypes are Classes of Account Subjects. Catalog asset models
-- are Classes of Asset Subjects. One table seats every taxonomy in
-- the system.
--
-- Only attribute-defined Classes are supported today (membership = the
-- Subject's `member_attribute` column equals this Class's `code`).
-- Synthetic Classes (predicate or junction-table membership) are a
-- later extension via additional columns.
-- -----------------------------------------------------------------------------

-- Subject Kind registry — the data-driven alphabet of Subject
-- discriminators (asset, account, vendor, recipe, equipment, …) so
-- tenants can extend it without a core code change. Each row names one
-- discriminator; `Subject::Custom.custom_kind` writes validate against
-- this registry.
CREATE TABLE IF NOT EXISTS subject_kinds (
    kind         TEXT PRIMARY KEY,
    label        TEXT NOT NULL,
    -- Optional self-FK so tenants can declare hierarchies (Account
    -- has parent='account', Recipe has parent='product', …). Walks
    -- pair with the Class registry's parent_code for two-level
    -- expressivity. Deferrable so seeds can land out of order.
    parent_kind  TEXT REFERENCES subject_kinds(kind) DEFERRABLE INITIALLY DEFERRED,
    description  TEXT,
    -- 'platform' for the core kinds shipped by BOSS; tenants override
    -- with their tenant id (or 'brewery' / 'used-device-shop' for
    -- the in-tree examples) so the admin UI can sort the registry
    -- by ownership.
    owning_team  TEXT NOT NULL,
    metadata     JSONB NOT NULL DEFAULT '{}'::jsonb,
    sort_order   INTEGER NOT NULL DEFAULT 0,
    retired_at   TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS subject_kinds_parent ON subject_kinds(parent_kind)
    WHERE parent_kind IS NOT NULL;


-- Seed: the SubjectKind registry rows BOSS core ships.
--
-- Top of the taxonomy: the five roots — four noun axes
-- (person / location / object / intangible) plus the calendar
-- coordination primitive every operating model needs. Tenant authors
-- pick which root a new Subject kind specializes via parent_kind.
--
-- Below the roots: the concrete Subject kinds the platform ships out
-- of the box, each specializing one root via parent_kind.
INSERT INTO subject_kinds (kind, label, description, owning_team, sort_order, parent_kind) VALUES
    -- Roots
    ('person',         'Person',         'The "who" axis. Specializations: account (customer cluster), employee (operator), vendor (counterparty), tenant-defined contacts.', 'platform',  1, NULL),
    ('location',       'Location',       'The "where" axis. Physical and logical places — brewhouse, taproom, depot, customer-site.',                                              'platform',  2, NULL),
    ('object',         'Object',         'The "what" axis. Tracked physical things — equipment, kegs, tanks, refurb devices. Hosts the Equipment KB pattern.',                       'platform',  3, NULL),
    ('intangible',     'Intangible',     'The fourth noun axis: identity-bearing things with no physical embodiment — agreements, campaigns, workflow documents, entitlements. Hosts the contract / SLA / lease family.', 'platform',  4, NULL),
    ('calendar',       'Calendar',       'The "when" axis. Reservations, events, due dates, business calendars. Coordinates time across the other roots.',                       'platform',  5, NULL),

    -- Person specializations
    ('account',        'Account',        'External account / customer (B2B account, wholesale customer).',                                                                         'platform', 20, 'person'),
    ('customer',       'Customer',       'External end-consumer. Distinct from account (B2B) — customers are individuals buying via a DTC channel like /shop. Carries email + name + purchase history.', 'platform', 25, 'person'),
    ('employee',       'Employee',       'A person on the roster. Subject of HR Jobs (hire, certification, …).',                                                                    'platform', 50, 'person'),
    ('vendor',         'Vendor',         'External supplier (parts vendor, ingredient supplier).',                                                                                  'platform', 60, 'person'),
    ('campaign',       'Campaign',       'Marketing motion / launch / promo. Time-bounded account-cluster operation.',                                                  'platform', 40, 'intangible'),

    -- Object specializations
    ('asset',          'Asset',          'A tracked physical unit (kettle, oven, mixer, refurbished device). The serial-numbered instance the Equipment KB describes.',                    'platform', 10, 'object'),
    ('product',        'Product',        'A finished good produced by the tenant — keg of beer, refurbished switch ready to ship, packaged batch. Distinct from `asset` (a tracked instance) and `parts` (raw inputs); products are countable on-hand-by-location output. Identity via SKU.', 'platform', 11, 'object'),

    -- Workflow-document Subjects are intangibles: agreements and
    -- transactional artifacts with identity but no physical
    -- embodiment. Future `contract`, `sla`, `lease` SubjectKinds
    -- land under the same root.
    ('purchase_order', 'Purchase Order', 'A PO Subject for vendor-procurement workflows. Workflow-document intangible.', 'platform', 30, 'intangible'),
    -- Transactional-document intangibles, siblings of purchase_order. They
    -- host the Class taxonomies for carrier, discrepancy-kind, and
    -- marketing-asset kind.
    ('shipment',        'Shipment',        'A dispatch of goods to an account. Workflow-document intangible; owns the carrier + status taxonomies.',      'platform', 31, 'intangible'),
    ('vendor-invoice',  'Vendor Invoice',  'An AP document against a vendor. Workflow-document intangible; owns the discrepancy-kind taxonomy.',           'platform', 32, 'intangible'),
    ('marketing-asset', 'Marketing Asset', 'A marketing content artifact (photo, video, deck, …) under a campaign. Intangible; owns the kind taxonomy.',   'platform', 33, 'intangible'),
    ('invoice',         'Invoice',         'An AR document billed to an account. Workflow-document intangible; owns the status + revenue-category taxonomies.', 'platform', 34, 'intangible'),
    ('message',         'Message',         'An internal message / system signal. Workflow-document intangible; owns the kind taxonomy.',                   'platform', 35, 'intangible'),

    -- Location root has the same kind name; no specialization shipped.
    -- (Tenant Locations get parent_kind='location' once they're added.)

    -- Escape hatch — `Subject::Custom { custom_kind, ref_id }` rows
    -- declare their own parent_kind in the registry; the literal
    -- `custom` row stays parentless so it never accidentally
    -- inherits.
    ('custom',         'Custom',         'Escape-hatch Subject for tenant-defined discriminators that haven''t been promoted to first-class kinds yet.',                            'platform', 999, NULL);

-- Calendar reservability is a SubjectKind property (data, not a closed
-- type): only a kind flagged `calendar_reservable` can hold a calendar
-- reservation. The calendar service enforces this on reserve, and its
-- GIST exclusion constraint guarantees one hard reservation per
-- individual subject per overlapping window. Employees (PTO, job-step
-- scheduling), assets (equipment time), and accounts (meetings) are the
-- v1 reservable kinds; tenants flag more without forking core.
UPDATE subject_kinds
   SET metadata = metadata || '{"calendar_reservable": true}'::jsonb
 WHERE kind IN ('employee', 'asset', 'account');


CREATE TABLE IF NOT EXISTS classes (
    subject_kind     TEXT NOT NULL,
    code             TEXT NOT NULL,
    display_name     TEXT NOT NULL,
    parent_code      TEXT,
    -- For attribute-defined Classes (v1): the column on the Subject
    -- whose value matches `code`. NULL reserved for future synthetic
    -- Classes (predicate / junction-table membership).
    member_attribute TEXT,
    metadata         JSONB NOT NULL DEFAULT '{}'::jsonb,
    sort_order       INTEGER NOT NULL DEFAULT 0,
    retired_at       TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (subject_kind, code),
    -- Subclass relationships stay within a single subject_kind.
    FOREIGN KEY (subject_kind, parent_code)
        REFERENCES classes(subject_kind, code)
        DEFERRABLE INITIALLY DEFERRED
);


CREATE INDEX IF NOT EXISTS classes_subject_kind ON classes(subject_kind) WHERE retired_at IS NULL;

CREATE INDEX IF NOT EXISTS classes_parent ON classes(subject_kind, parent_code) WHERE parent_code IS NOT NULL;


-- Seed: Employee role classes. `employees.role` is validated against
-- these by boss-people-api (subject_kind='employee',
-- member_attribute='role'). `department` in metadata is the natural
-- department roll-up; not load-bearing (employees.department is its
-- own column).
INSERT INTO classes (subject_kind, code, display_name, member_attribute, metadata, sort_order) VALUES
    ('employee', 'ceo',                 'CEO',                  'role', '{"department": "executive", "is_management": true, "is_executive": true}'::jsonb,  10),
    ('employee', 'cto',                 'CTO',                  'role', '{"department": "executive", "is_management": true, "is_executive": true}'::jsonb,  11),
    ('employee', 'coo',                 'COO',                  'role', '{"department": "executive", "is_management": true, "is_executive": true}'::jsonb,  12),
    ('employee', 'cfo',                 'CFO',                  'role', '{"department": "executive", "is_management": true, "is_executive": true}'::jsonb,  13),
    ('employee', 'vp-sales',            'VP Sales',             'role', '{"department": "sales", "is_management": true}'::jsonb,                            20),
    ('employee', 'sales-mgr',           'Sales Manager',        'role', '{"department": "sales", "is_management": true}'::jsonb,                            21),
    ('employee', 'sales-rep',           'Sales Rep',            'role', '{"department": "sales", "is_management": false}'::jsonb,                           22),
    ('employee', 'service-mgr',         'Service Manager',      'role', '{"department": "service", "is_management": true}'::jsonb,                          30),
    ('employee', 'service-tech',        'Service Tech',         'role', '{"department": "service", "is_management": false}'::jsonb,                         31),
    ('employee', 'refurb-supervisor',   'Refurb Supervisor',    'role', '{"department": "refurb", "is_management": true}'::jsonb,                           40),
    ('employee', 'refurb-tech',         'Refurb Tech',          'role', '{"department": "refurb", "is_management": false}'::jsonb,                          41),
    ('employee', 'qa-lead',             'QA Lead',              'role', '{"department": "qa", "is_management": true}'::jsonb,                               50),
    ('employee', 'qa-tech',             'QA Tech',              'role', '{"department": "qa", "is_management": false}'::jsonb,                              51),
    ('employee', 'warehouse-mgr',       'Warehouse Manager',    'role', '{"department": "warehouse", "is_management": true}'::jsonb,                        60),
    ('employee', 'warehouse-clerk',     'Warehouse Clerk',      'role', '{"department": "warehouse", "is_management": false}'::jsonb,                       61),
    ('employee', 'parts-buyer',         'Parts Buyer',          'role', '{"department": "warehouse", "is_management": false}'::jsonb,                       62),
    ('employee', 'controller',          'Controller',           'role', '{"department": "finance", "is_management": true}'::jsonb,                          70),
    ('employee', 'ap-specialist',       'AP Specialist',        'role', '{"department": "finance", "is_management": false}'::jsonb,                         71),
    ('employee', 'hr-generalist',       'HR Generalist',        'role', '{"department": "people", "is_management": false}'::jsonb,                          80),
    ('employee', 'recruiter',           'Recruiter',            'role', '{"department": "people", "is_management": false}'::jsonb,                          81),
    ('employee', 'support-specialist',  'Support Specialist',   'role', '{"department": "support", "is_management": false}'::jsonb,                         90),
    ('employee', 'it-manager',          'IT Manager',           'role', '{"department": "people", "is_management": true}'::jsonb,                           91),
    ('employee', 'marketing-mgr',       'Marketing Manager',    'role', '{"department": "marketing", "is_management": true}'::jsonb,                       100),
    ('employee', 'marketing-specialist','Marketing Specialist', 'role', '{"department": "marketing", "is_management": false}'::jsonb,                      101),
    ('employee', 'content-writer',      'Content Writer',       'role', '{"department": "marketing", "is_management": false}'::jsonb,                      102),
    ('employee', 'brand-designer',      'Brand Designer',       'role', '{"department": "marketing", "is_management": false}'::jsonb,                      103),
    -- Generic / cross-tenant operator roles. `owner` covers the
    -- sole-proprietor case + tenant-side founder seeds
    -- where the executive isn't a CEO/CTO/COO/CFO.
    ('employee', 'owner',               'Owner',                'role', '{"department": "executive", "is_management": true, "is_executive": true}'::jsonb,    1),
    -- Reserved fixture role for the boss-testing smoke harness.
    -- Carries the `emp-smoke` employee in defaults.rs with Read on
    -- every projection resource — never Write, Close, or SignOff.
    ('employee', 'smoke-tester',        'Smoke Tester',         'role', '{"department": "executive", "is_test_fixture": true}'::jsonb,                         2),
    -- Platform-admin is the role the operator-baseline CEO/COO/CTO
    -- (`emp-ceo`/`emp-coo`/`emp-cto`) carry. Distinct from any
    -- tenant org chart's CEO/COO/CTO so a tenant policy grant on
    -- role='ceo' doesn't also grant the platform-baseline operator
    -- tenant-CEO access. See infra/operator-baseline/operator_hires.toml.
    ('employee', 'platform-admin',      'Platform admin',       'role', '{"department": "executive", "is_management": true, "is_executive": true, "is_system_role": true}'::jsonb,  3),
    -- Audit-readonly: every Boss tenant ships with this role for
    -- external auditors / CPAs / SOC-2 reviewers.
    ('employee', 'audit-readonly',      'Audit (read-only)',    'role', '{"department": "audit", "is_management": false, "is_executive": false, "is_system_role": true}'::jsonb,    4);


-- Seed: Employee department classes. `employees.department` is
-- validated against these by boss-people-api (subject_kind='employee',
-- member_attribute='department').
INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order) VALUES
    -- Platform-level identities (emp-bootstrap-admin, emp-audit, etc.)
    -- carry department='platform' to distinguish "people who run the
    -- BOSS deployment itself" from any tenant's org-chart departments.
    ('employee', 'platform',  'Platform',  'department',  1),
    ('employee', 'executive', 'Executive', 'department', 10),
    ('employee', 'sales',     'Sales',     'department', 20),
    ('employee', 'service',   'Service',   'department', 30),
    ('employee', 'refurb',    'Refurb',    'department', 40),
    ('employee', 'qa',        'QA',        'department', 50),
    ('employee', 'warehouse', 'Warehouse', 'department', 60),
    ('employee', 'finance',   'Finance',   'department', 70),
    ('employee', 'people',    'People',    'department', 80),
    ('employee', 'support',   'Support',   'department', 90),
    ('employee', 'marketing', 'Marketing', 'department', 100);


-- Seed: Employee employment_type classes. Validated by boss-people-api
-- against (subject_kind='employee', member_attribute='employment_type').
INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order) VALUES
    ('employee', 'full-time',  'Full-Time',  'employment_type', 10),
    ('employee', 'part-time',  'Part-Time',  'employment_type', 20),
    ('employee', 'contractor', 'Contractor', 'employment_type', 30);


-- Seed: Employee status classes. Validated by boss-people-api against
-- (subject_kind='employee', member_attribute='status').
INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order) VALUES
    ('employee', 'active',     'Active',     'status', 10),
    ('employee', 'on-leave',   'On Leave',   'status', 20),
    ('employee', 'terminated', 'Terminated', 'status', 30);


-- -----------------------------------------------------------------------------
-- Locations (platform primitive — see
-- docs/architecture-decisions.md §Locations)
-- -----------------------------------------------------------------------------
--
-- A Location is a place. Has identity, lifecycle, and attributes
-- (kind, hierarchy parent, timezone, optional geo, optional
-- address). Lives near the top of the schema so domain tables can
-- FK into it (employees.location, requisitions.location).
--
-- `kind` is validated by the Class registry against
-- (subject_kind='location', member_attribute='kind') — tenant
-- extensible per docs/design/class-registry.md.
--
-- `account_id` is a soft reference, not an FK constraint: a location
-- can name an account it belongs to without coupling this registry to
-- the accounts table's load order.
CREATE TABLE IF NOT EXISTS locations (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    kind            TEXT NOT NULL,
    parent_id       TEXT REFERENCES locations(id) DEFERRABLE INITIALLY DEFERRED,
    timezone        TEXT NOT NULL,
    latitude        DOUBLE PRECISION,
    longitude       DOUBLE PRECISION,
    address         TEXT,
    -- Soft reference to accounts(id) — not an FK (see table comment).
    account_id      TEXT,
    metadata        JSONB NOT NULL DEFAULT '{}'::jsonb,
    retired_at      TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS locations_parent  ON locations(parent_id)  WHERE parent_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS locations_kind    ON locations(kind)       WHERE retired_at IS NULL;

CREATE INDEX IF NOT EXISTS locations_account ON locations(account_id) WHERE account_id IS NOT NULL;


-- Seed: LocationKind classes. Two tenant-flavored sets in one
-- list — brewery codes (taproom, brewhouse, …) for the public OSS
-- playground demo, used-device-shop codes (hq, field-region,
-- warehouse-zone, remote) for the in-tree refurb-business
-- example. Tenants in their own deployments customise this set.
INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order) VALUES
    -- Generic / cross-tenant
    ('location', 'remote',           'Remote',          'kind', 10),
    -- Used-device-shop example
    ('location', 'hq',               'HQ',              'kind', 20),
    ('location', 'field-region',     'Field Region',    'kind', 21),
    ('location', 'warehouse-zone',   'Warehouse Zone',  'kind', 22),
    -- Boss Brewery example (OSS playground tenant)
    ('location', 'taproom',          'Taproom',         'kind', 30),
    ('location', 'brewhouse',        'Brewhouse',       'kind', 31),
    ('location', 'distribution-route','Distribution Route','kind', 32);


-- Seed: account_type Class rows. Free-text per-tenant taxonomy
-- validated via the Class registry — `(subject_kind='account',
-- member_attribute='type')`. Tenants ship their own values; the
-- two in-tree examples seed below.
INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order) VALUES
    -- Generic / cross-tenant fallback for an unclassified account.
    ('account', 'unspecified',          'Unspecified',          'type', 0),
    -- Boss Brewery example (OSS playground tenant)
    ('account', 'wholesale-distributor', 'Wholesale Distributor', 'type', 10),
    ('account', 'bar-restaurant',        'Bar / Restaurant',      'type', 11),
    ('account', 'chain-retail',          'Chain Retail',          'type', 12),
    ('account', 'corporate-event',       'Corporate / Event',     'type', 13),
    -- Brewery account types the sim + data-seed post; the account_type
    -- gate validates against these.
    ('account', 'wholesale',             'Wholesale',             'type', 14),
    ('account', 'wholesale-prospect',    'Wholesale Prospect',    'type', 15),
    ('account', 'direct-consumer',       'Direct Consumer',       'type', 16),
    ('account', 'taproom-direct',        'Taproom Direct',        'type', 17),
    -- Used-device-shop example
    ('account', 'service-account',       'Service Account',       'type', 20),
    ('account', 'lab',                   'Laboratory',            'type', 21),
    ('account', 'clinic',                'Clinic',                'type', 22);

-- Seed: the module-tier taxonomies that live as Class rows. The owning
-- service validates writes against these, keyed on (subject_kind, code).
--
-- account.tier + account.note-kind own subject_kind='account'. carrier,
-- discrepancy-kind, and marketing kind own the shipment / vendor-invoice
-- / marketing-asset subject kinds — transactional-document intangibles
-- registered above (siblings of purchase_order). The asset event-payload
-- taxonomies (intake-source, warranty-coverage, condition) own
-- subject_kind='asset'.
INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order) VALUES
    -- account.tier
    ('account', 'platinum', 'Platinum', 'tier', 30),
    ('account', 'gold',     'Gold',     'tier', 31),
    ('account', 'silver',   'Silver',   'tier', 32),
    -- account.note-kind
    ('account', 'note',        'Note',        'note-kind', 40),
    ('account', 'call',        'Call',        'note-kind', 41),
    ('account', 'meeting',     'Meeting',     'note-kind', 42),
    ('account', 'email',       'Email',       'note-kind', 43),
    ('account', 'interaction', 'Interaction', 'note-kind', 44),
    -- shipment.carrier
    ('shipment', 'fedex',        'FedEx',        'carrier', 10),
    ('shipment', 'ups',          'UPS',          'carrier', 11),
    ('shipment', 'freight',      'Freight',      'carrier', 12),
    ('shipment', 'local-pickup', 'Local Pickup', 'carrier', 13),
    -- vendor-invoice.discrepancy-kind
    ('vendor-invoice', 'overbilled',  'Overbilled',  'discrepancy-kind', 10),
    ('vendor-invoice', 'shorted',     'Shorted',     'discrepancy-kind', 11),
    ('vendor-invoice', 'wrong-price', 'Wrong Price', 'discrepancy-kind', 12),
    ('vendor-invoice', 'wrong-qty',   'Wrong Qty',   'discrepancy-kind', 13),
    -- marketing-asset.kind
    ('marketing-asset', 'photo',      'Photo',      'kind', 10),
    ('marketing-asset', 'video',      'Video',      'kind', 11),
    ('marketing-asset', 'doc',        'Doc',        'kind', 12),
    ('marketing-asset', 'template',   'Template',   'kind', 13),
    ('marketing-asset', 'logo',       'Logo',       'kind', 14),
    ('marketing-asset', 'brief-body', 'Brief Body', 'kind', 15),
    ('marketing-asset', 'retro',      'Retro',      'kind', 16),
    ('marketing-asset', 'deck',       'Deck',       'kind', 17),
    ('marketing-asset', 'one-pager',  'One-Pager',  'kind', 18),
    -- asset.intake-source / warranty-coverage / condition — the asset
    -- event-payload enums, modeled as data (no DB CHECK existed; validated
    -- at the asset-event ingest boundary in boss-assets).
    ('asset', 'used-trade-in',  'Used Trade-In',  'intake-source',     10),
    ('asset', 'buyback',        'Buyback',        'intake-source',     11),
    ('asset', 'returned-lease', 'Returned Lease', 'intake-source',     12),
    ('asset', 'oem-new',        'OEM New',        'intake-source',     13),
    ('asset', 'standard',       'Standard',       'warranty-coverage', 20),
    ('asset', 'extended',       'Extended',       'warranty-coverage', 21),
    ('asset', 'new',            'New',            'condition',         30),
    ('asset', 'used',           'Used',           'condition',         31),
    -- asset.category — Boss Brewery equipment-KB categories (catalog
    -- MODEL taxonomy, distinct from the event-payload enums above).
    -- Posted via examples/brewery/data/catalog.json; the catalog
    -- category gate validates against these (fail-loud on an unknown).
    ('asset', 'brewhouse-vessel',    'Brewhouse Vessel',    'category', 40),
    ('asset', 'brewhouse-utility',   'Brewhouse Utility',   'category', 41),
    ('asset', 'fermentation-vessel', 'Fermentation Vessel', 'category', 42),
    ('asset', 'conditioning-vessel', 'Conditioning Vessel', 'category', 43),
    ('asset', 'utility',             'Utility',             'category', 44),
    ('asset', 'packaging',           'Packaging',           'category', 45),
    ('asset', 'barrel',              'Barrel',              'category', 46);


-- Seed: lifecycle / status taxonomies that were closed Rust enums until
-- v1.1.0. Same registry-izing treatment as the taxonomies above —
-- newtype-over-String on the domain side, vocabulary as data here. The
-- owning service validates writes against (subject_kind, code):
--
--   asset.phase     — boss-assets projection state (Registered→Decommissioned).
--   shipment.status — boss-shipping lifecycle (label-created→delivered/exception).
--   invoice.status  — boss-commerce AR lifecycle (paid/outstanding/past-due/written-off).
--   message.kind    — boss-messages routing kind (direct/signal/archived).
--
-- The phase / status values are projection-derived (the system emits
-- them; they are not free-text tenant input), so the gate primarily
-- fail-loud-rejects an unseeded code — missing even one here breaks a
-- regen. shipment / invoice / message are the transactional-document
-- intangibles registered in subject_kinds above (siblings of
-- purchase_order / vendor-invoice / marketing-asset).
INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order) VALUES
    -- asset.phase — the AssetLifecyclePhase projection states, in
    -- pipeline order.
    ('asset', 'registered',      'Registered',       'phase', 50),
    ('asset', 'received',        'Received',         'phase', 51),
    ('asset', 'triaging',        'Triaging',         'phase', 52),
    ('asset', 'refurbing',       'Refurbing',        'phase', 53),
    ('asset', 'qa',              'QA',               'phase', 54),
    ('asset', 'ready',           'Ready',            'phase', 55),
    ('asset', 'shipped',         'Shipped',          'phase', 56),
    ('asset', 'installed',       'Installed',        'phase', 57),
    ('asset', 'out-for-service', 'Out for Service',  'phase', 58),
    ('asset', 'decommissioned',  'Decommissioned',   'phase', 59),
    -- shipment.status — the ShipmentStatus lifecycle.
    ('shipment', 'label-created', 'Label Created', 'status', 20),
    ('shipment', 'picked-up',     'Picked Up',     'status', 21),
    ('shipment', 'in-transit',    'In Transit',    'status', 22),
    ('shipment', 'delivered',     'Delivered',     'status', 23),
    ('shipment', 'exception',     'Exception',     'status', 24),
    -- invoice.status — the InvoiceStatus AR lifecycle.
    ('invoice', 'paid',        'Paid',         'status', 10),
    ('invoice', 'outstanding', 'Outstanding',  'status', 11),
    ('invoice', 'past-due',    'Past Due',     'status', 12),
    ('invoice', 'written-off', 'Written Off',  'status', 13),
    -- message.kind — the MessageKind routing kind.
    ('message', 'direct',   'Direct',   'kind', 10),
    ('message', 'signal',   'Signal',   'kind', 11),
    ('message', 'archived', 'Archived', 'kind', 12);


-- Seed: default Location rows. employees.location FKs into these.
-- Timezones default to America/Los_Angeles for HQ / field and UTC for
-- the placeholder remote bucket; tenants override per-location when
-- they deploy.
INSERT INTO locations (id, name, kind, timezone) VALUES
    ('loc-hq',                  'Headquarters',         'hq',           'America/Los_Angeles'),
    ('loc-field-default',       'Field (default)',      'field-region', 'America/Los_Angeles'),
    ('loc-remote-default',      'Remote (default)',     'remote',       'UTC'),
    -- Brewery example tenant — production sites referenced by the
    -- brewery employee roster + the morning-brew /
    -- equipment-preventive-maintenance JobKinds. Added here so a fresh
    -- bootstrap-db.sh produces a DB the brewery roster can FK
    -- into without an out-of-band insert.
    ('loc-brewery-brewhouse',   'Brewery Brewhouse',    'production',   'America/Los_Angeles'),
    ('loc-brewery-taproom',     'Brewery Taproom',      'retail',       'America/Los_Angeles');


-- -----------------------------------------------------------------------------
-- Calendar reservations (global-calendar primitive — see
-- docs/architecture-decisions.md §Calendar)
-- -----------------------------------------------------------------------------
--
-- A reservation claims a time window on a single resource. The
-- "no two hard reservations overlap on the same resource"
-- invariant is enforced by the GIST exclusion constraint below —
-- Postgres refuses a conflicting INSERT, so the application code
-- has no race window between "check available" and "write".
--
-- Half-open ranges `[start, end)` so back-to-back reservations
-- (one ends at 11:00, next starts at 11:00) don't conflict.
--
-- `cancelled_at IS NOT NULL` rows stay in the table for audit;
-- the partial index + WHERE on the exclusion constraint mean
-- they're invisible to the conflict check.
CREATE TABLE IF NOT EXISTS calendar_reservations (
    id              UUID PRIMARY KEY,
    -- The reserved Subject, stored as (kind, id) — the calendar's I/O
    -- label `resource_kind`/`resource_id` for `boss_core::job::Subject`.
    -- No closed CHECK: which kinds may be reserved is data (the
    -- `calendar_reservable` flag on the subject_kinds registry, enforced
    -- by the calendar service on reserve). Two columns so reads can
    -- filter on `resource_kind` alone without a composite prefix scan.
    resource_kind   TEXT NOT NULL,
    resource_id     TEXT NOT NULL,
    start_ts        TIMESTAMPTZ NOT NULL,
    end_ts          TIMESTAMPTZ NOT NULL,
    CHECK (end_ts > start_ts),

    -- What this reservation is for. `reason_ref_id` is the
    -- traceable identifier of the originating thing — a JobId,
    -- StepId, PmScheduleId, etc. — so cancellation cascades
    -- (delete every reservation with reason_ref_id = X) and the
    -- UI can render "this overlaps Job-12345".
    -- Free-form reason tag — see `boss_core::calendar::reason` for the
    -- conventional values BOSS emits. Any string is valid; a tenant
    -- adds its own reason without a schema change.
    reason_kind     TEXT NOT NULL,
    reason_ref_id   TEXT NOT NULL,

    -- Defaults to 'hard'; the caller can override (a customer-facing
    -- meeting is hard; an internal 1:1 is soft).
    strength        TEXT NOT NULL DEFAULT 'hard'
                    CHECK (strength IN ('hard','soft')),

    notes           TEXT,
    created_by      TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    cancelled_at    TIMESTAMPTZ
);


-- The load-bearing invariant. `btree_gist` provides the equality
-- operator on TEXT in a GIST index; combined with the range-overlap
-- operator on `tstzrange`, Postgres rejects any INSERT that would
-- create two `hard` reservations on the same resource whose
-- windows touch. Cancelled rows are exempt via the partial WHERE.
ALTER TABLE calendar_reservations
    ADD CONSTRAINT calendar_no_hard_overlap
    EXCLUDE USING gist (
        (resource_kind || ':' || resource_id) WITH =,
        tstzrange(start_ts, end_ts, '[)') WITH &&
    ) WHERE (strength = 'hard' AND cancelled_at IS NULL);


-- Per-resource lookup ("show me every reservation on emp-042").
CREATE INDEX IF NOT EXISTS calendar_reservations_resource
    ON calendar_reservations (resource_kind, resource_id, start_ts)
    WHERE cancelled_at IS NULL;


-- Cancellation-cascade lookup ("delete every reservation tied to
-- the job-step that just cancelled").
CREATE INDEX IF NOT EXISTS calendar_reservations_reason_ref
    ON calendar_reservations (reason_kind, reason_ref_id)
    WHERE cancelled_at IS NULL;


-- ---------------------------------------------------------------------------
-- Business calendars — named, data-defined sets of non-business days
-- (`us-banking`, `us-tax`, …). Reference data, like `classes`: seeded via
-- POST /api/calendar/business-calendars/batch, queried for business-day math
-- by the dispatcher's timing triggers and the simulator. The business-day
-- LOGIC is generic (`boss_core::calendar::BusinessCalendar`); only the
-- holiday/window DATA lives here. "A tax calendar is just data."
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS business_calendars (
    code         TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    -- Non-business weekdays as `Weekday::num_days_from_monday()`
    -- (Mon=0 … Sun=6). Default Sat+Sun. Stored so a tenant with a
    -- different week (e.g. Fri+Sat) is data, not a code change.
    weekend      SMALLINT[] NOT NULL DEFAULT '{5,6}',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- The non-business dates for a calendar — federal holidays plus any
-- closed windows expanded to individual days. `reason` is for humans
-- (the SPA / audit); the business-day query only needs the date.
CREATE TABLE IF NOT EXISTS business_calendar_closed_days (
    calendar_code TEXT NOT NULL
                  REFERENCES business_calendars(code) ON DELETE CASCADE,
    day           DATE NOT NULL,
    reason        TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (calendar_code, day)
);

CREATE INDEX IF NOT EXISTS business_calendar_closed_days_cal
    ON business_calendar_closed_days (calendar_code);

