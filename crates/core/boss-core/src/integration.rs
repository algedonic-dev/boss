//! Integration evidence (approved design 8f28b94f): presentation modules
//! authorize nothing here. These are source contracts, not a publisher or
//! deployment observer. Adapters must supply independently verified authority.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IntegrationEntry {
    pub active: bool,
    pub service: String,
}

/// A versioned registry declaration, never an implicit module default.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IntegrationConfig {
    pub version: u64,
    pub author: String,
    pub supersedes: Option<u64>,
    pub entries: BTreeMap<String, IntegrationEntry>,
}

/// Data reported by an observer. Deserializing this does not authenticate it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeploymentObservation {
    pub configuration_version: u64,
    pub deployment_uid: String,
    pub artifact: String,
    pub generation: u64,
    pub services: Vec<String>,
    pub observed_at: DateTime<Utc>,
    pub valid_until: DateTime<Utc>,
    pub recorded_by: String,
}

/// Current deployment identity is a separate adapter input, not the receipt
/// vouching for its own identity. Neither input states downstream readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentIdentity {
    pub uid: String,
    pub artifact: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceAuthority {
    Verified,
    Refused,
    Unavailable,
}

/// Pure authority projection port. The later authenticated adapter must
/// resolve publication approval, existing row policy and approved signer
/// from their records; accepting a field on these input documents is invalid.
pub trait IntegrationAuthority {
    fn publication(&self, config: &IntegrationConfig) -> EvidenceAuthority;
    fn read(&self, integration: &str, entry: &IntegrationEntry) -> EvidenceAuthority;
    fn observer(&self, observation: &DeploymentObservation) -> EvidenceAuthority;
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnknownIntegration {
    ConfigurationUnavailable,
    NotPublished,
    VersionMismatch,
    Undeclared,
    Unauthorized,
    AuthorityUnavailable,
    Malformed,
    ObservationUnavailable,
    StaleObservation,
    DeploymentMismatch,
    UnapprovedObserver,
}

/// The entire public-safe view. Private deployment/signer/service details
/// never serialize through this shape.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntegrationState {
    Enabled {
        configuration_version: u64,
        observation_generation: u64,
    },
    Disabled {
        configuration_version: u64,
    },
    Unknown {
        reason: UnknownIntegration,
    },
}

pub struct IntegrationContext<'a> {
    pub deployment: &'a DeploymentIdentity,
    pub configuration_version: u64,
    pub now: DateTime<Utc>,
    /// Supplied by the approved observer policy, never chosen by the receipt.
    pub maximum_validity_seconds: i64,
    /// The adapter derives this from the existing service registry.
    pub known_services: &'a [&'a str],
    pub authority: &'a dyn IntegrationAuthority,
}

/// Derive availability from immutable inputs. Enabled means the declaration
/// and current observation agree; it does not mean healthy, ready, or allowed
/// to mutate. A draft or launch-intent document cannot supply authority.
pub fn integration_state(
    integration: &str,
    config: Option<&IntegrationConfig>,
    observation: Option<&DeploymentObservation>,
    context: &IntegrationContext<'_>,
) -> IntegrationState {
    use UnknownIntegration as U;
    let unknown = |reason| IntegrationState::Unknown { reason };
    let Some(config) = config else {
        return unknown(U::ConfigurationUnavailable);
    };
    if config.version == 0
        || config.author.trim().is_empty()
        || config
            .supersedes
            .is_some_and(|v| v == 0 || v >= config.version)
        || config.entries.iter().any(|(id, entry)| {
            id.trim().is_empty()
                || entry.service.trim().is_empty()
                || !context.known_services.contains(&entry.service.as_str())
        })
    {
        return unknown(U::Malformed);
    }
    if config.version != context.configuration_version {
        return unknown(U::VersionMismatch);
    }
    let Some(entry) = config.entries.get(integration) else {
        return unknown(U::Undeclared);
    };
    match context.authority.read(integration, entry) {
        EvidenceAuthority::Verified => (),
        EvidenceAuthority::Refused => return unknown(U::Unauthorized),
        EvidenceAuthority::Unavailable => return unknown(U::AuthorityUnavailable),
    }
    match context.authority.publication(config) {
        EvidenceAuthority::Verified => (),
        EvidenceAuthority::Refused => return unknown(U::NotPublished),
        EvidenceAuthority::Unavailable => return unknown(U::AuthorityUnavailable),
    }
    if !entry.active {
        return IntegrationState::Disabled {
            configuration_version: config.version,
        };
    }
    let Some(observation) = observation else {
        return unknown(U::ObservationUnavailable);
    };
    if observation.generation == 0
        || observation.recorded_by.trim().is_empty()
        || observation.deployment_uid.trim().is_empty()
        || context.deployment.uid.trim().is_empty()
        || observation.artifact.trim().is_empty()
        || observation.artifact.len() > 512
        || observation
            .services
            .iter()
            .any(|s| !context.known_services.contains(&s.as_str()))
        || observation
            .services
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != observation.services.len()
        || context.maximum_validity_seconds <= 0
    {
        return unknown(U::Malformed);
    }
    if observation.configuration_version != config.version {
        return unknown(U::VersionMismatch);
    }
    if observation.deployment_uid != context.deployment.uid
        || observation.artifact != context.deployment.artifact
    {
        return unknown(U::DeploymentMismatch);
    }
    if !observation.services.contains(&entry.service) {
        return unknown(U::ObservationUnavailable);
    }
    let Some(maximum_validity) = chrono::Duration::try_seconds(context.maximum_validity_seconds)
    else {
        return unknown(U::Malformed);
    };
    if observation.observed_at > context.now
        || observation.valid_until <= context.now
        || observation.valid_until <= observation.observed_at
        || observation.valid_until - observation.observed_at > maximum_validity
    {
        return unknown(U::StaleObservation);
    }
    match context.authority.observer(observation) {
        EvidenceAuthority::Verified => IntegrationState::Enabled {
            configuration_version: config.version,
            observation_generation: observation.generation,
        },
        EvidenceAuthority::Refused => unknown(U::UnapprovedObserver),
        EvidenceAuthority::Unavailable => unknown(U::AuthorityUnavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Authority;
    impl IntegrationAuthority for Authority {
        fn publication(&self, _: &IntegrationConfig) -> EvidenceAuthority {
            EvidenceAuthority::Verified
        }
        fn read(&self, _: &str, _: &IntegrationEntry) -> EvidenceAuthority {
            EvidenceAuthority::Verified
        }
        fn observer(&self, _: &DeploymentObservation) -> EvidenceAuthority {
            EvidenceAuthority::Verified
        }
    }
    struct RefusedAuthority {
        publication: EvidenceAuthority,
        read: EvidenceAuthority,
        observer: EvidenceAuthority,
    }
    impl IntegrationAuthority for RefusedAuthority {
        fn publication(&self, _: &IntegrationConfig) -> EvidenceAuthority {
            self.publication
        }
        fn read(&self, _: &str, _: &IntegrationEntry) -> EvidenceAuthority {
            self.read
        }
        fn observer(&self, _: &DeploymentObservation) -> EvidenceAuthority {
            self.observer
        }
    }
    #[test]
    fn only_published_pinned_observed_authorized_evidence_enables_an_integration() {
        let now = DateTime::from_timestamp(100, 0).unwrap();
        let config = IntegrationConfig {
            version: 2,
            author: "fixture-author".into(),
            supersedes: Some(1),
            entries: BTreeMap::from([(
                "billing".into(),
                IntegrationEntry {
                    active: true,
                    service: "fixture-service".into(),
                },
            )]),
        };
        let observation = DeploymentObservation {
            configuration_version: 2,
            deployment_uid: "fixture-uid".into(),
            artifact: "a".repeat(40),
            generation: 3,
            services: vec!["fixture-service".into()],
            observed_at: now,
            valid_until: now + chrono::Duration::seconds(60),
            recorded_by: "fixture-observer".into(),
        };
        let deployment = DeploymentIdentity {
            uid: observation.deployment_uid.clone(),
            artifact: observation.artifact.clone(),
        };
        let context = IntegrationContext {
            deployment: &deployment,
            configuration_version: 2,
            now,
            maximum_validity_seconds: 60,
            known_services: &["fixture-service"],
            authority: &Authority,
        };
        let derive = |c: Option<&IntegrationConfig>, o: Option<&DeploymentObservation>| {
            integration_state("billing", c, o, &context)
        };
        assert_eq!(
            derive(Some(&config), Some(&observation)),
            IntegrationState::Enabled {
                configuration_version: 2,
                observation_generation: 3
            }
        );
        assert_eq!(
            derive(None, Some(&observation)),
            IntegrationState::Unknown {
                reason: UnknownIntegration::ConfigurationUnavailable
            }
        );
        assert_eq!(
            derive(Some(&config), None),
            IntegrationState::Unknown {
                reason: UnknownIntegration::ObservationUnavailable
            }
        );
        let mut disabled = config.clone();
        disabled.entries.get_mut("billing").unwrap().active = false;
        assert_eq!(
            derive(Some(&disabled), None),
            IntegrationState::Disabled {
                configuration_version: 2
            }
        );
        let mut wrong = config.clone();
        wrong.version = 3;
        assert_eq!(
            derive(Some(&wrong), Some(&observation)),
            IntegrationState::Unknown {
                reason: UnknownIntegration::VersionMismatch
            }
        );
        let mut absent = config.clone();
        absent.entries.clear();
        assert_eq!(
            derive(Some(&absent), Some(&observation)),
            IntegrationState::Unknown {
                reason: UnknownIntegration::Undeclared
            }
        );
        for mutate in [0, 1, 2, 3, 4, 5, 6, 7] {
            let mut bad = observation.clone();
            match mutate {
                0 => bad.valid_until = now,
                1 => bad.observed_at = now + chrono::Duration::seconds(1),
                2 => bad.valid_until = now + chrono::Duration::seconds(61),
                3 => bad.artifact = "b".repeat(40),
                4 => bad.deployment_uid = "other".into(),
                5 => bad.services.clear(),
                6 => bad.configuration_version = 1,
                _ => bad.services.push("fixture-service".into()),
            }
            assert!(
                matches!(
                    derive(Some(&config), Some(&bad)),
                    IntegrationState::Unknown { .. }
                ),
                "mutation {mutate}"
            );
        }
        let public = serde_json::to_string(&derive(Some(&config), Some(&observation))).unwrap();
        for secret in [
            "fixture-uid",
            "fixture-observer",
            "fixture-service",
            "fixture-author",
            &observation.artifact,
        ] {
            assert!(!public.contains(secret));
        }
        for (publication, read, observer, expected) in [
            (
                EvidenceAuthority::Refused,
                EvidenceAuthority::Verified,
                EvidenceAuthority::Verified,
                UnknownIntegration::NotPublished,
            ),
            (
                EvidenceAuthority::Verified,
                EvidenceAuthority::Refused,
                EvidenceAuthority::Verified,
                UnknownIntegration::Unauthorized,
            ),
            (
                EvidenceAuthority::Verified,
                EvidenceAuthority::Verified,
                EvidenceAuthority::Refused,
                UnknownIntegration::UnapprovedObserver,
            ),
            (
                EvidenceAuthority::Unavailable,
                EvidenceAuthority::Verified,
                EvidenceAuthority::Verified,
                UnknownIntegration::AuthorityUnavailable,
            ),
            (
                EvidenceAuthority::Verified,
                EvidenceAuthority::Unavailable,
                EvidenceAuthority::Verified,
                UnknownIntegration::AuthorityUnavailable,
            ),
            (
                EvidenceAuthority::Verified,
                EvidenceAuthority::Verified,
                EvidenceAuthority::Unavailable,
                UnknownIntegration::AuthorityUnavailable,
            ),
        ] {
            let authority = RefusedAuthority {
                publication,
                read,
                observer,
            };
            let refused_context = IntegrationContext {
                authority: &authority,
                ..context
            };
            assert_eq!(
                integration_state(
                    "billing",
                    Some(&config),
                    Some(&observation),
                    &refused_context
                ),
                IntegrationState::Unknown { reason: expected }
            );
        }
        let mut too_long = observation.clone();
        too_long.valid_until += chrono::Duration::nanoseconds(1);
        assert_eq!(
            derive(Some(&config), Some(&too_long)),
            IntegrationState::Unknown {
                reason: UnknownIntegration::StaleObservation
            }
        );
        for malformed in [
            IntegrationConfig {
                version: 0,
                ..config.clone()
            },
            IntegrationConfig {
                author: " ".into(),
                ..config.clone()
            },
            IntegrationConfig {
                supersedes: Some(2),
                ..config.clone()
            },
            IntegrationConfig {
                entries: BTreeMap::from([(
                    "billing".into(),
                    IntegrationEntry {
                        active: true,
                        service: "unregistered".into(),
                    },
                )]),
                ..config.clone()
            },
        ] {
            assert_eq!(
                derive(Some(&malformed), Some(&observation)),
                IntegrationState::Unknown {
                    reason: UnknownIntegration::Malformed
                }
            );
        }
        let impossible_bound = IntegrationContext {
            maximum_validity_seconds: i64::MAX,
            ..context
        };
        assert_eq!(
            integration_state(
                "billing",
                Some(&config),
                Some(&observation),
                &impossible_bound
            ),
            IntegrationState::Unknown {
                reason: UnknownIntegration::Malformed
            }
        );
    }
}
