//! WHO "ME" IS — `GET /api/jobs/assignments?for=me` (backlog 74569e94,
//! design ea906603 Q2, David 2026-09-27).
//!
//! The top board's NEEDS YOU row is the viewer's own queue. Measured
//! the day it was filed: the bare assignments read as
//! `claude@algedonic.dev` answered 0, while `assignee_id=agent-claude`
//! answered 199 — the steps sat on the other spelling of the same
//! actor. `boss orient`'s MY WORK already expanded the caller through
//! the agents registry (65a89769); David's answer moves that expansion
//! into the jobs API, so every surface that says "yours" asks the one
//! server that knows the aliases, and adds the roles the viewer holds.
//!
//! Pure: the handler reads the registry and hands the rows here.

use serde::{Deserialize, Serialize};

use crate::agents::AgentRow;

/// Whom one `for=me` read asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    /// The viewer's id first, then every id the agents registry ties
    /// to it — the aliases when the viewer is an agent's id, the id and
    /// sibling aliases when the viewer IS an alias. A viewer the
    /// registry does not know is read alone.
    pub ids: Vec<String>,
    /// The roles the viewer holds, over both vocabularies the claim
    /// door judges (`http/steps.rs`, backlog 4b103f0f): the platform
    /// role the request asserts, then the `role` the registry row of
    /// each matched agent declares.
    pub roles: Vec<String>,
}

/// The viewer `id` holding platform role `role`, expanded through the
/// agents registry `agents`. The same expansion as `boss orient`'s
/// `my_work_identities`, which this answers for on the server.
pub fn me(id: &str, role: &str, agents: &[AgentRow]) -> Me {
    fn push(into: &mut Vec<String>, v: &str) {
        if !v.is_empty() && !into.iter().any(|o| o == v) {
            into.push(v.to_string());
        }
    }
    let mut ids = vec![id.to_string()];
    let mut roles: Vec<String> = Vec::new();
    push(&mut roles, role);
    for agent in agents
        .iter()
        .filter(|a| a.id == id || a.aliases.iter().any(|x| x == id))
    {
        push(&mut ids, &agent.id);
        for alias in &agent.aliases {
            push(&mut ids, alias);
        }
        if let Some(r) = agent.role.as_deref() {
            push(&mut roles, r);
        }
    }
    Me { ids, roles }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, aliases: &[&str], role: Option<&str>) -> AgentRow {
        AgentRow {
            id: id.into(),
            display_name: id.into(),
            default_model: "opus-5".into(),
            role: role.map(str::to_string),
            department: None,
            hourly_budget_usd_micros: None,
            max_concurrent_runs: None,
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
        }
    }

    fn registry() -> Vec<AgentRow> {
        vec![
            agent(
                "agent-claude",
                &["claude@algedonic.dev"],
                Some("engineering-agent"),
            ),
            agent("agent-other", &["other@algedonic.dev"], None),
        ]
    }

    #[test]
    fn an_agents_id_reads_for_its_aliases_and_the_roles_it_holds() {
        assert_eq!(
            me("agent-claude", "platform-admin", &registry()),
            Me {
                ids: vec!["agent-claude".into(), "claude@algedonic.dev".into()],
                roles: vec!["platform-admin".into(), "engineering-agent".into()],
            }
        );
    }

    #[test]
    fn an_alias_reads_for_its_agent_from_the_other_end() {
        assert_eq!(
            me("claude@algedonic.dev", "platform-admin", &registry()).ids,
            vec!["claude@algedonic.dev", "agent-claude"]
        );
    }

    /// A person the registry does not name is read alone — never
    /// nobody — with the one role the request carries.
    #[test]
    fn a_person_the_registry_does_not_know_is_read_alone() {
        assert_eq!(
            me("emp-david", "platform-admin", &registry()),
            Me {
                ids: vec!["emp-david".into()],
                roles: vec!["platform-admin".into()],
            }
        );
        assert_eq!(me("emp-david", "", &[]).roles, Vec::<String>::new());
    }
}
