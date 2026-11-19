//! The protocol's bill of resources. Declaration only: the claim transaction
//! gains reservations in console car 2, not as a side effect of this schema.
use crate::flow_resources::Amount;
use serde::{Deserialize, Serialize};

/// Public resource quantities have ONE JSON representation. Preserve authored
/// decimal text; arithmetic uses the separately checked decimal value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Quantity(String);
impl Quantity {
    pub fn parse(text: &str) -> Result<Self, String> {
        Amount::parse(text)?;
        Ok(Self(text.into()))
    }
    pub fn amount(&self) -> Result<Amount, String> {
        Amount::parse(&self.0)
    }
}
impl std::fmt::Display for Quantity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl<'de> Deserialize<'de> for Quantity {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text=String::deserialize(d).map_err(|_|serde::de::Error::custom("resource amount must be a quoted exact decimal string, for example \"1\" or \"0.5\""))?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}
impl From<u64> for Quantity {
    fn from(v: u64) -> Self {
        Self(v.to_string())
    }
}
impl From<i32> for Quantity {
    fn from(v: i32) -> Self {
        Self(v.to_string())
    }
}

/// The TOML authoring boundary alone accepts integer literals. Floats are
/// refused BEFORE a generic number can round them. API JSON remains strings.
pub(crate) fn toml_amount(value: &mut toml::Value, key: &str) -> Result<(), String> {
    match value {
        toml::Value::Integer(n) => {
            let text = n.to_string();
            Quantity::parse(&text)?;
            *value = toml::Value::String(text);
            Ok(())
        }
        toml::Value::String(text) => Quantity::parse(text).map(|_| ()),
        _ => Err(format!(
            "{key}: resource amount must be an integer literal or a quoted exact decimal, for example \"0.5\"; float literals are refused"
        )),
    }
}

/// Exact string amounts retain their authored representation. Missing
/// dimensions mean zero demand; they never mean a pool has zero capacity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceBill {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcpu: Option<Quantity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_gb: Option<Quantity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_gb: Option<Quantity>,
    /// The box a run's `vcpu` is drawn from, and the station whose
    /// floating share of it was drawn: written by the claim, never
    /// authored ([`crate::pools::validate_bill`] refuses it on a row).
    /// With it, `vcpu` bills `host:<node>` instead of the summed `vcpu`
    /// HUD pool (backlog 5608c1a2; see [`crate::flow_resources::resolve_host`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostDraw>,
    /// The run profiles whose runs bind no worker, so this bill charges
    /// them no `vcpu` (backlog db88f6a9): read at the claim off the billed
    /// packet's [`RUN_PROFILE_KEY`] by
    /// [`crate::flow_resources::waive_workerless`], which records the bill
    /// with the list consumed — a claim never carries it. A run naming no
    /// profile, or one not listed, is billed: a missing word over-counts,
    /// it never admits a worker no box was asked about.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workerless_profiles: Vec<String>,
}

/// Where a claim placed a run's vCPU: the estate node and the station
/// whose floating share it drew (station-tree.md §3: "the record names
/// which one satisfied it").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostDraw {
    pub node: String,
    pub station: String,
}

/// The pool a model CLASS names: `robots:<class>`. One spelling for a
/// step's `resources.model`, a `model:<class>` executor, and the class an
/// agents row records (T2d, backlog 06d8988b).
///
/// The station tree names it `robots:<model>` (docs/design/station-tree.md
/// §3), and the <model> there is this <class>: the model FAMILY a run's
/// model id belongs to (`opus` for opus-5-5, `sol` for gpt-6.1-sol), the
/// token an agent row's `model_class` already holds. So T5's rename from
/// `model-slot:<class>` (backlog 84f570a1, 2026-11-07) changed the prefix
/// and keyed nothing new: the from-run resolver, the agents door and the
/// `model:<class>` executor read the same class they read before.
pub fn robot_pool(class: &str) -> String {
    format!("{ROBOTS_PREFIX}{class}")
}

/// The one grammar of a robot model class: nonempty lowercase letters,
/// digits and `-`. Read by both judges of a class — a bill's `model`
/// ([`ResourceBill::validate`]) and the suffix of a `robots:<class>` pool
/// ([`crate::pools::PoolSpec::validate`]). Until backlog 3901fc49 the pool
/// checked only its prefix while the bill refused `_` and `:`, so
/// `robots:bad_class` published and a from-run claim resolved to a class
/// its own bill refused, answering HTTP 500. Lowercase because a pool name
/// is: a class the bill accepted in capitals named a pool no row could
/// declare.
pub fn is_robot_class(class: &str) -> bool {
    !class.is_empty()
        && class
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A bill's `model` that names no class of its own: the class is the one
/// the billed PACKET's own `model` is metered under, read at the claim
/// (backlog af5dd547; design 012822ea, conductor decision 20261106_1507Z
/// (3)). It exists because `agent-run` `building` is one protocol row
/// serving every model, while a literal `model` is one fixed class per
/// row. A sentinel in the existing key rather than a new key, so a build
/// that predates it still reads the row (the bill is closed to unknown
/// keys) and refuses its claims on `model-slot:from-run` (or, since T5,
/// `robots:from-run`), pools [`crate::pools::PoolSpec::validate`] refuses
/// to declare.
pub const MODEL_FROM_RUN: &str = "from-run";

/// The packet metadata key a [`MODEL_FROM_RUN`] bill reads: the `model`
/// `boss dispatch` writes on every `agent-run` (its metadata_schema
/// requires it).
pub const RUN_MODEL_KEY: &str = "model";

/// The packet metadata key [`ResourceBill::workerless_profiles`] is read
/// against: the execution `profile` `boss dispatch` writes on every
/// `agent-run` (backlog db88f6a9).
pub const RUN_PROFILE_KEY: &str = "profile";

/// The prefix every robot pool carries, `robots:<class>` — see
/// [`robot_pool`]. It was `model-slot:` until T5 (backlog 84f570a1).
pub const ROBOTS_PREFIX: &str = "robots:";

/// The prefix the robot pools carried before T5 (backlog 84f570a1). Read
/// by one check only: a build older than T5 bills an unresolved from-run
/// claim as `model-slot:from-run`, so that spelling stays undeclarable
/// ([`crate::pools::PoolSpec::validate`]). No reader keys on it.
pub const MODEL_SLOT_PREFIX_BEFORE_T5: &str = "model-slot:";

impl ResourceBill {
    /// Whether this bill takes its model class from the run it bills.
    pub fn takes_model_from_run(&self) -> bool {
        self.model.as_deref() == Some(MODEL_FROM_RUN)
    }

    /// The same bill with its model class named: what a claim records
    /// once the class has been read off the run.
    pub fn with_model_class(&self, class: &str) -> Self {
        Self {
            model: Some(class.into()),
            ..self.clone()
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.model.as_ref().is_some_and(|m| !is_robot_class(m)) {
            return Err("resources.model must name a nonempty robot model class".into());
        }
        let token = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        };
        if self
            .host
            .as_ref()
            .is_some_and(|h| !token(&h.node) || h.station.trim().is_empty())
        {
            return Err("resources.host must name an estate node and a station".into());
        }
        // A waiver names profiles, and waives something: a list on a bill
        // that bills no vCPU would read as a fact about runs it decides
        // nothing for (backlog db88f6a9).
        if !self.workerless_profiles.is_empty() && self.vcpu.is_none() {
            return Err(
                "resources.workerless_profiles waives vcpu, so the bill must bill vcpu".into(),
            );
        }
        if self.workerless_profiles.iter().any(|p| !token(p)) {
            return Err(
                "resources.workerless_profiles must name lowercase run profiles, for example \"analyst\""
                    .into(),
            );
        }
        for (name, amount) in [
            ("vcpu", &self.vcpu),
            ("ram_gb", &self.ram_gb),
            ("disk_gb", &self.disk_gb),
        ] {
            if amount.as_ref().is_some_and(|a| a.amount().is_err()) {
                return Err(format!(
                    "resources.{name} must be a nonnegative exact decimal string"
                ));
            }
        }
        Ok(())
    }

    /// The mapping is one definition, shared by declaration validation and the
    /// later reservation port. Disk demand belongs to the workspace stock.
    pub fn pools(&self) -> Vec<(String, Quantity)> {
        let mut rows = Vec::new();
        if let Some(model) = &self.model {
            rows.push((robot_pool(model), Quantity::from(1)));
        }
        let vcpu = self.host.as_ref().map_or_else(
            || "vcpu".to_string(),
            |h| format!("{}{}", crate::station_tree::HOST_POOL_PREFIX, h.node),
        );
        for (name, amount) in [
            (vcpu, &self.vcpu),
            ("ram_gb".into(), &self.ram_gb),
            ("disk:ci-workspaces".into(), &self.disk_gb),
        ] {
            if let Some(amount) = amount {
                rows.push((name, amount.clone()));
            }
        }
        rows
    }
}
