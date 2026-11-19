def text: type == "string" and length > 0;
def object_identity:
    type == "object" and .metadata.namespace == "longhorn-system"
    and (.metadata.name | text) and (.metadata.uid | text);
def collection($kind):
    type == "object" and .apiVersion == "v1"
    and (.kind == $kind or .kind == "List")
    and (.metadata.continue == null or .metadata.continue == "")
    and (.items | type == "array") and all(.items[]; object_identity);
if ($ns | length) != 1 or ($ds | length) != 1 or ($pods | length) != 1 or ($events | length) != 1
then error("expected exactly one document per fixed read") else . end
| $ns[0] as $n | $ds[0] as $d | $pods[0] as $p | $events[0] as $e
| if ($n | .apiVersion == "v1" and .kind == "Namespace" and .metadata.name == "longhorn-system" and (.metadata.uid | text))
    and ($d | object_identity and .apiVersion == "apps/v1" and .kind == "DaemonSet" and .metadata.name == "longhorn-manager"
        and .spec.selector.matchLabels.app == "longhorn-manager")
    and ($p | collection("PodList")) and ($e | collection("EventList"))
    and all($p.items[];
        .metadata.labels.app == "longhorn-manager"
        and any(.metadata.ownerReferences[]?; .kind == "DaemonSet" and .name == "longhorn-manager" and .uid == $d.metadata.uid and .controller == true)
        and (.status | type == "object")
        and (.status.containerStatuses == null or (.status.containerStatuses | type == "array"))
        and all((.status.containerStatuses // [])[];
            (.name | text) and (.restartCount | type == "number" and . >= 0 and floor == .)
            and (.ready | type == "boolean")))
  then {
    state:"observed", cause:"UNKNOWN",
    coverage:"Current fixed reads plus retained events only; no guaranteed historical window or inferred root cause",
    namespace_uid:$n.metadata.uid,
    daemonset:{uid:$d.metadata.uid, generation:$d.metadata.generation, status:$d.status},
    pods:($p.items | sort_by(.metadata.name) | map({name:.metadata.name,uid:.metadata.uid,
        resourceVersion:.metadata.resourceVersion,creationTimestamp:.metadata.creationTimestamp,
        deletionTimestamp:.metadata.deletionTimestamp,node:.spec.nodeName,phase:.status.phase,
        conditions:.status.conditions,
        containers:(if .status.containerStatuses == null then "UNKNOWN" else .status.containerStatuses | map({name,ready,restartCount,state,lastState,containerID,imageID}) end)})),
    events:($e.items | map(select(
        (.involvedObject.kind == "DaemonSet" and .involvedObject.name == "longhorn-manager")
        or (.involvedObject.kind == "Pod" and ((.involvedObject.name // "") | startswith("longhorn-manager-"))))))
  }
  else error("fixed namespace, DaemonSet, complete List or owner/status shape did not match") end
