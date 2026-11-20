//! Runtime configuration for boss-dispatcher.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct DispatcherConfig {
    pub nats_url: String,
    pub jobs_api_url: String,
    pub people_api_url: String,
    pub inventory_api_url: String,
    pub commerce_api_url: String,
    pub products_api_url: String,
    pub shipping_api_url: String,
    pub ledger_api_url: String,
    pub messages_api_url: String,
    /// Clock service base URL. The schedule runner consumes its SSE tick
    /// feed (`GET /api/clock/ticks`) to drive sim-day-boundary firing of
    /// schedule-triggered rules.
    pub clock_api_url: String,
    /// Calendar service base URL. The schedule runner fetches the
    /// business calendars its schedule rules reference at startup.
    pub calendar_api_url: String,
    /// Policy service base URL — the scope gate on
    /// `GET /api/dispatcher/schedule` (design ea906603), the one read
    /// here that asks who is reading.
    pub policy_api_url: String,
    pub http_bind: String,
    /// Postgres URL — the dispatcher loads its rule registry (the
    /// append-only versioned `dispatcher_rules` table) from here at
    /// startup and serves it at `/api/dispatcher/rules`. Replaces the
    /// legacy `BOSS_DISPATCHER_RULES` rules.toml file path.
    pub postgres_url: String,
    /// The AUTHORED rule registry directory (`infra/dispatcher/rules`),
    /// from `BOSS_DISPATCHER_RULES` — the image carries it at
    /// `/opt/boss/infra/dispatcher/rules`.
    ///
    /// NOT the runtime registry: that is the `dispatcher_rules` table
    /// above, and it holds no justification. This is the one thing the
    /// table cannot answer — each rule's `why` — which the read surface
    /// joins on so an operator can ask what the system is enforcing AND
    /// why. `None`, or a directory that will not read, leaves every
    /// `why` null and the response SAYS so; it never degrades into the
    /// confident wrong answer "no rule records a why".
    pub authored_rules_dir: Option<PathBuf>,
    /// External webhook URL for the `webhook.notify` handler to forward
    /// matched events to. `None` (the normal deployment) makes
    /// `webhook.notify` a no-op; a regen sets it to the brewery-engine's
    /// callback server so its CounterpartyEngine (banks, suppliers,
    /// courier, tax authority) reacts to live events as an external party.
    pub webhook_url: Option<String>,
    /// The credential broker's issuer endpoint — the forge whose
    /// tokens the `credential.rotate.forgejo` handler mints and
    /// revokes. Defaults to the deployment's own forge.
    pub broker_forge_url: String,
    /// The broker's Forgejo root credential (admin token). Sourced
    /// from the `boss-credential-broker-root` k8s Secret via env;
    /// `None` leaves the handler registered but unconfigured, so a
    /// rotation rule firing without it dead-letters with the knob's
    /// name instead of tripping UnknownHandler. Never logged.
    #[serde(skip)]
    pub broker_forgejo_token: Option<String>,
    /// The Cloudflare v4 API the `credential.rotate.cloudflare-tunnel`
    /// handler speaks (packet 04e5f833). Overridable for a stub;
    /// defaults to the real endpoint.
    pub broker_cloudflare_api_url: String,
    /// The broker's Cloudflare root credential — one account API
    /// token (Cloudflare Tunnel:Edit, Zone:DNS:Edit, Zone:Read),
    /// minted once by David into the same `boss-credential-broker-root`
    /// Secret under key `cloudflare-token`. Same posture as the
    /// forgejo root: `None` leaves the handler registered but
    /// unconfigured, so a firing rule dead-letters naming the knob.
    /// Never logged.
    #[serde(skip)]
    pub broker_cloudflare_token: Option<String>,
    /// Where the tree's DNS zone declarations live — the directory the
    /// image carries `infra/cluster/dns/` at (`<zone>.toml` beside
    /// `check-declared.sh`), which the `dns.observe` handler runs over
    /// the records it fetched (backlog 5e58922c). Same posture as the
    /// rules directory: set explicitly by the deployment
    /// (`BOSS_DNS_DECLARATIONS`), `None` leaves the handler registered
    /// but unconfigured so a firing rule dead-letters naming the knob.
    pub dns_declarations_dir: Option<String>,
    /// The restricted read-only Stripe key the `sensor.poll` handler
    /// sends as the bearer for a sensor declaring credential
    /// `stripe-restricted-read` (design 14c9b2ad, backlog 2d33e111).
    /// Minted once by David into the same `boss-credential-broker-root`
    /// Secret under key `stripe-restricted-read`. Same posture as the
    /// two broker roots: `None` leaves the handler registered, and the
    /// poll files the `sensor_unreadable:<id>` alarm naming the knob
    /// instead of failing silently. Never logged.
    #[serde(skip)]
    pub broker_stripe_key: Option<String>,
    /// The Stripe API origin the adapter reads (`BOSS_STRIPE_API_BASE`);
    /// a stub stands in for the tests, the real endpoint is the default.
    pub stripe_api_base: String,
    /// The GitHub REST API the `credential.rotate.github-app-installation`
    /// handler speaks (design 76155676). Overridable for a stub.
    pub broker_github_api_url: String,
    /// The GitHub App root (design 76155676, backlog 81eb6d4d): the App
    /// id, the installation id on the algedonic-dev organisation, and
    /// the App private key (PEM), placed once by David into the same
    /// `boss-credential-broker-root` Secret under keys `github-app.id`,
    /// `github-app.installation-id` and `github-app.private-key.pem`.
    /// Each `None` when its env var is unset or empty; the binary builds
    /// the root only when all three are present and otherwise registers
    /// the handler unconfigured, naming every missing key. Never logged.
    #[serde(skip)]
    pub broker_github_app_id: Option<String>,
    #[serde(skip)]
    pub broker_github_app_installation_id: Option<String>,
    #[serde(skip)]
    pub broker_github_app_private_key: Option<String>,
}

/// An env var's value, `None` when unset or blank.
fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            nats_url: std::env::var("BOSS_NATS_URL")
                .unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string()),
            jobs_api_url: std::env::var("BOSS_JOBS_URL")
                .unwrap_or_else(|_| boss_ports::url("jobs")),
            people_api_url: std::env::var("BOSS_PEOPLE_URL")
                .unwrap_or_else(|_| boss_ports::url("people")),
            inventory_api_url: std::env::var("BOSS_INVENTORY_URL")
                .unwrap_or_else(|_| boss_ports::url("inventory")),
            commerce_api_url: std::env::var("BOSS_COMMERCE_URL")
                .unwrap_or_else(|_| boss_ports::url("commerce")),
            products_api_url: std::env::var("BOSS_PRODUCTS_URL")
                .unwrap_or_else(|_| boss_ports::url("products")),
            shipping_api_url: std::env::var("BOSS_SHIPPING_URL")
                .unwrap_or_else(|_| boss_ports::url("shipping")),
            ledger_api_url: std::env::var("BOSS_LEDGER_URL")
                .unwrap_or_else(|_| boss_ports::url("ledger")),
            messages_api_url: std::env::var("BOSS_MESSAGES_URL")
                .unwrap_or_else(|_| boss_ports::url("messages")),
            clock_api_url: std::env::var("BOSS_CLOCK_URL")
                .unwrap_or_else(|_| boss_ports::url("clock")),
            calendar_api_url: std::env::var("BOSS_CALENDAR_URL")
                .unwrap_or_else(|_| boss_ports::url("calendar")),
            policy_api_url: std::env::var("BOSS_POLICY_URL")
                .unwrap_or_else(|_| boss_ports::url("policy")),
            // Loopback: the gateway is the sole trust boundary and is
            // co-located in every deployment (SECURITY.md §Deployment
            // trust model). BOSS_DISPATCHER_BIND widens deliberately.
            http_bind: std::env::var("BOSS_DISPATCHER_BIND")
                .unwrap_or_else(|_| format!("127.0.0.1:{}", boss_ports::prod("dispatcher"))),
            postgres_url: std::env::var("BOSS_POSTGRES_URL")
                .unwrap_or_else(|_| "postgres://boss:boss@127.0.0.1/boss".to_string()),
            authored_rules_dir: std::env::var("BOSS_DISPATCHER_RULES")
                .ok()
                .map(PathBuf::from),
            webhook_url: std::env::var("BOSS_EVENT_WEBHOOK_URL").ok(),
            broker_forge_url: std::env::var("BOSS_BROKER_FORGE_URL")
                .unwrap_or_else(|_| "http://10.20.0.15:3000".to_string()),
            broker_forgejo_token: std::env::var("BOSS_BROKER_FORGEJO_TOKEN")
                .ok()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty()),
            broker_cloudflare_api_url: std::env::var("BOSS_BROKER_CLOUDFLARE_API_URL")
                .unwrap_or_else(|_| "https://api.cloudflare.com/client/v4".to_string()),
            broker_cloudflare_token: std::env::var("BOSS_BROKER_CLOUDFLARE_TOKEN")
                .ok()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty()),
            dns_declarations_dir: std::env::var("BOSS_DNS_DECLARATIONS")
                .ok()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty()),
            broker_stripe_key: std::env::var("BOSS_BROKER_STRIPE_KEY")
                .ok()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty()),
            stripe_api_base: std::env::var("BOSS_STRIPE_API_BASE")
                .unwrap_or_else(|_| "https://api.stripe.com".to_string()),
            broker_github_api_url: std::env::var("BOSS_BROKER_GITHUB_API_URL")
                .unwrap_or_else(|_| "https://api.github.com".to_string()),
            broker_github_app_id: env_value("BOSS_BROKER_GITHUB_APP_ID"),
            broker_github_app_installation_id: env_value("BOSS_BROKER_GITHUB_APP_INSTALLATION_ID"),
            broker_github_app_private_key: env_value("BOSS_BROKER_GITHUB_APP_PRIVATE_KEY"),
        }
    }
}
