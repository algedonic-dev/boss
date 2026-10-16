//! Fixed private collector intent. No creation, credentials, or stop assertion.
//! The trusted private handler and native installation remain separate obligations.
use serde_json::{Value, json};
use uuid::Uuid;

/// Only broker/controller-owned originals may construct this plan. This
/// renders intent; it does not attest a stop or authorize native creation.
pub(crate) struct Spec<'a> {
    pub run: Uuid,
    pub nonce: Uuid,
    pub workspace_uid: &'a str,
    pub image_witness: &'a Value,
    pub trust_generation: Uuid,
}
fn helper_hash() -> String {
    super::super::hash(include_bytes!(
        "../../../../../../infra/agent/private-collector.py"
    ))
}
fn contract_hash() -> String {
    super::super::hash(include_bytes!(
        "../../../../../../infra/agent/private-collector-contract.json"
    ))
}
/// An image witnessed before the private helper existed cannot carry it.
pub(crate) fn manifest(spec: &Spec<'_>) -> Result<Value, super::CacheError> {
    let contract: Value = serde_json::from_str(include_str!(
        "../../../../../../infra/agent/private-collector-contract.json"
    ))
    .map_err(|_| super::CacheError::InvalidBinding)?;
    let image = spec.image_witness["image"]
        .as_str()
        .ok_or(super::CacheError::Unavailable)?;
    let generation = spec.image_witness["generation"]
        .as_str()
        .ok_or(super::CacheError::Unavailable)?;
    let Some((repository, digest)) = image.split_once("@sha256:") else {
        return Err(super::CacheError::InvalidBinding);
    };
    if spec.run.is_nil()
        || spec.nonce.is_nil()
        || spec.trust_generation.is_nil()
        || spec.workspace_uid.is_empty()
        || spec.workspace_uid.len() > 128
        || !spec
            .workspace_uid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || repository.is_empty()
        || repository.contains(char::is_whitespace)
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || generation.len() != 40
        || !generation
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || spec.image_witness["image_platform"] != "linux/amd64"
        || spec.image_witness["collector_helper_sha256"] != helper_hash()
        || spec.image_witness["collector_contract_sha256"] != contract_hash()
    {
        return Err(super::CacheError::InvalidBinding);
    }
    let run = spec.run.to_string();
    let projection = contract["projection"]
        .as_str()
        .ok_or(super::CacheError::InvalidBinding)?;
    Ok(json!({"apiVersion":"batch/v1","kind":"Job",
        "metadata":{"name":format!("agent-private-collection-{run}"),"namespace":"boss-dev",
            "labels":{"boss.dev/agent-run":run,"boss.dev/purpose":"private-cache-collection"},
            "annotations":{"boss.dev/original-workspace-uid":spec.workspace_uid,"boss.dev/image-generation":generation,"boss.dev/trust-generation":spec.trust_generation.to_string()}},
        "spec":{"backoffLimit":0,"activeDeadlineSeconds":contract["deadline_seconds"],"ttlSecondsAfterFinished":300,
            "template":{"metadata":{"labels":{"boss.dev/agent-run":run,"boss.dev/purpose":"private-cache-collection"}},
                "spec":{"serviceAccountName":contract["account"],"automountServiceAccountToken":false,
                    "restartPolicy":"Never","hostNetwork":false,"hostPID":false,"hostIPC":false,"shareProcessNamespace":false,
                    "imagePullSecrets":[{"name":"forgejo-registry"}],
                    "affinity":{"nodeAffinity":{"requiredDuringSchedulingIgnoredDuringExecution":{"nodeSelectorTerms":[{"matchExpressions":[{"key":"node-role.kubernetes.io/control-plane","operator":"DoesNotExist"},{"key":"node-role.kubernetes.io/master","operator":"DoesNotExist"}]}]}}},
                    "securityContext":{"runAsUser":65534,"runAsGroup":1500,"runAsNonRoot":true,"fsGroup":1500,"seccompProfile":{"type":"RuntimeDefault"}},
                    "volumes":[{"name":"retained","persistentVolumeClaim":{"claimName":crate::launch_job::workspace_name(&run),"readOnly":true}},
                        {"name":"collection-authority","projected":{"defaultMode":288,"sources":[
                            {"serviceAccountToken":{"path":"token","audience":contract["audience"],"expirationSeconds":contract["token_seconds"]}},
                            {"configMap":{"name":contract["trust"],"items":[{"key":"ca.crt","path":"ca.crt"}]}}]}}],
                    "containers":[{"name":"collector","image":image,"imagePullPolicy":"Always","command":[contract["helper"]],
                        "securityContext":{"allowPrivilegeEscalation":false,"privileged":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}},
                        "resources":{"requests":{"cpu":"100m","memory":"64Mi"},"limits":{"cpu":"1","memory":"128Mi"}},
                        "env":[{"name":"BOSS_COLLECTION_RUN","value":run},{"name":"BOSS_COLLECTION_NONCE","value":spec.nonce.to_string()},
                            {"name":"BOSS_COLLECTION_WORKSPACE_UID","value":spec.workspace_uid},
                            {"name":"BOSS_COLLECTION_POD_UID","valueFrom":{"fieldRef":{"fieldPath":"metadata.uid"}}}],
                        "volumeMounts":[{"name":"retained","mountPath":"/retained","readOnly":true},{"name":"collection-authority","mountPath":projection,"readOnly":true}]}]}}}}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_collector_requires_new_immutable_helper_witness_and_original_workspace() {
        let run = Uuid::from_u128(1);
        let nonce = Uuid::from_u128(2);
        let input = Spec {
            run,
            nonce,
            workspace_uid: "original",
            image_witness: &json!({}),
            trust_generation: Uuid::from_u128(3),
        };
        assert!(manifest(&input).is_err());
        let witness = json!({"image":format!("registry.invalid/agent@sha256:{}","a".repeat(64)),"generation":"b".repeat(40),"image_platform":"linux/amd64","collector_helper_sha256":helper_hash(),"collector_contract_sha256":contract_hash()});
        let input = Spec {
            image_witness: &witness,
            ..input
        };
        let value = manifest(&input).unwrap();
        let pod = &value["spec"]["template"]["spec"];
        assert_eq!(pod["serviceAccountName"], "boss-managed-cache-collector");
        assert_eq!(pod["automountServiceAccountToken"], false);
        assert_eq!(
            pod["volumes"][0]["persistentVolumeClaim"]["claimName"],
            crate::launch_job::workspace_name(&run.to_string())
        );
        assert_eq!(pod["volumes"][0]["persistentVolumeClaim"]["readOnly"], true);
        assert_eq!(pod["containers"][0]["volumeMounts"][0]["readOnly"], true);
        assert!(pod.get("initContainers").is_none());
        assert_eq!(pod["volumes"].as_array().unwrap().len(), 2);
        assert_eq!(value["spec"]["activeDeadlineSeconds"], 120);
        for key in [
            "collector_helper_sha256",
            "collector_contract_sha256",
            "image",
            "generation",
        ] {
            let mut changed = witness.clone();
            changed[key] = json!("wrong");
            assert!(
                manifest(&Spec {
                    image_witness: &changed,
                    ..input
                })
                .is_err(),
                "{key}"
            );
        }
    }
}
