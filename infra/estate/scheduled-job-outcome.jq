# A declared producer's latest finished native attempt, independently of
# any filesystem sample. A newer running retry cannot erase a failure.
def require($ok; $why): if $ok then . else error($why) end;
def text: type == "string" and length > 0;
def instant:
  require(type == "string" and test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]+)?Z$"); "invalid native Job timestamp")
  | capture("^(?<whole>[^.]+)(?:\\.(?<fraction>[0-9]+))?Z$")
  | [(.whole + "Z" | fromdateiso8601), (.fraction // "" | sub("0+$"; ""))];
require(type == "array" and length == 1; "native Job inventory must be one entire document")
| .[0]
| require(type == "object" and (.items | type) == "array" and
  (.metadata.continue // "") == ""; "native Job inventory malformed or truncated")
| [.items[] | select(any(.metadata.ownerReferences[]?;
    .kind == "CronJob" and .name == $source.cronjob and .controller == true))] as $attempts
| require(all($attempts[];
    .metadata.namespace == $source.namespace and (.metadata.uid | text) and
    (.metadata.name | text) and (.metadata.deletionTimestamp == null) and
    ([.metadata.ownerReferences[] | select(.controller == true)] | length) == 1 and
    ([.metadata.ownerReferences[] | select(.controller == true)][0].uid | text) and
    ((.metadata.creationTimestamp | instant) <= [$now, ""]) and
    (.status | type) == "object" and
    (.status.conditions // [] | type) == "array"); "native attempt identity or status unavailable")
| require(($attempts | map(.metadata.uid) | unique | length) == ($attempts | length);
    "duplicate native Job identity")
| require(($attempts | map(.metadata.creationTimestamp | instant) | unique | length) == ($attempts | length);
    "ambiguous native Job order")
| require(($attempts | map(.metadata.ownerReferences[] | select(.controller == true) | .uid) | unique | length) <= 1;
    "CronJob owner identity changed within retained inventory")
| require(all($attempts[];
    ([.status.conditions[]? | select(.status == "True" and (.type == "Failed" or .type == "Complete"))] | length) <= 1);
    "contradictory native terminal conditions")
| ($attempts | map(select(any(.status.conditions[]?;
    .status == "True" and (.type == "Failed" or .type == "Complete"))))
    | sort_by(.metadata.creationTimestamp | instant) | last) as $last
| if $last == null then {id:($source.namespace + "/" + $source.cronjob), state:"pending"}
  else ($last.status.conditions | map(select(.status == "True" and (.type == "Failed" or .type == "Complete")))[0]) as $condition
    | require(($last.status.active // 0) == 0;
        "terminal native Job still active")
    | require($condition.type != "Complete" or $last.status.succeeded == 1;
        "Complete condition lacks a successful native attempt")
    | {id:($source.namespace + "/" + $source.cronjob),
       state:(if $condition.type == "Failed" then "failed" else "succeeded" end),
       namespace:$source.namespace, cronjob:$source.cronjob,
       cronjob_uid:($last.metadata.ownerReferences[] | select(.controller == true) | .uid),
       job:$last.metadata.name, job_uid:$last.metadata.uid,
       created_at:$last.metadata.creationTimestamp, condition:$condition.type,
       reason:($condition.reason // "unspecified"), message:($condition.message // ""),
       observed_at:($now | todateiso8601)}
  end
