# A successful PATCH transport is not confirmation of the applied runtime.
# Compare every reviewed field; only explicit API defaults may be added.
# Unknown admission mutations refuse rather than silently certify them.
def podpath: ["jobTemplate","spec","template","spec"];
def jobpath: ["jobTemplate","spec"];
def containerpath($p): ($p | length) == 6 and $p[0:5] == (podpath + ["containers"])
  and ($p[5] | type) == "number";
def image_policy($image):
  if ($image | type) != "string" then null
  elif ($image | contains("@")) then "IfNotPresent"
  elif ($image | split("/")[-1] | contains(":") | not) or ($image | endswith(":latest"))
  then "Always" else "IfNotPresent" end;
def defaulted($p; $v; $source):
  ($p == ["suspend"] and $v == false)
  or ($p == ["concurrencyPolicy"] and $v == "Allow")
  or ($p == ["successfulJobsHistoryLimit"] and $v == 3)
  or ($p == ["failedJobsHistoryLimit"] and $v == 1)
  or (($p == jobpath + ["parallelism"] or $p == jobpath + ["completions"]) and $v == 1)
  or ($p == jobpath + ["backoffLimit"] and $v == 6)
  or (($p == jobpath + ["suspend"] or $p == jobpath + ["manualSelector"]) and $v == false)
  or ($p == jobpath + ["completionMode"] and $v == "NonIndexed")
  or ($p == jobpath + ["podReplacementPolicy"] and $v == "TerminatingOrFailed")
  or ($p == podpath + ["dnsPolicy"] and $v == "ClusterFirst")
  or ($p == podpath + ["schedulerName"] and $v == "default-scheduler")
  or ($p == podpath + ["terminationGracePeriodSeconds"] and $v == 30)
  or ($p == podpath + ["enableServiceLinks"] and $v == true)
  or ($p == podpath + ["preemptionPolicy"] and $v == "PreemptLowerPriority")
  or ($p == podpath + ["serviceAccountName"] and $v == "default")
  or ($p == podpath + ["serviceAccount"] and
      $v == ($source.jobTemplate.spec.template.spec.serviceAccountName // "default"))
  or ($p == podpath + ["securityContext"] and $v == {})
  or (($p == ["jobTemplate","metadata"] or $p == ["jobTemplate","spec","template","metadata"])
      and ($v == {} or $v == {creationTimestamp:null}))
  or (($p == ["jobTemplate","metadata","creationTimestamp"]
       or $p == ["jobTemplate","spec","template","metadata","creationTimestamp"]) and $v == null)
  or (containerpath($p[0:-1]) and (
      ($p[-1] == "terminationMessagePath" and $v == "/dev/termination-log")
      or ($p[-1] == "terminationMessagePolicy" and $v == "File")
      or ($p[-1] == "resources" and $v == {})
      or ($p[-1] == "imagePullPolicy" and
        $v == image_policy($source | getpath($p[0:-1]) | .image))))
  or (($p | length) == 11 and $p[0:5] == podpath + ["containers"]
      and $p[6] == "env" and $p[8:11] == ["valueFrom","configMapKeyRef","optional"] and $v == false);
def matches($expected; $p; $source):
  . as $actual |
  if ($actual | type) != ($expected | type) then false
  elif ($expected | type) == "object" then
    all($expected | keys[]; . as $key |
      (($actual | has($key)) and ($actual[$key] | matches($expected[$key]; $p + [$key]; $source)))
      or (($key == "creationTimestamp") and $expected[$key] == null
        and defaulted($p + [$key]; null; $source) and ($actual | has($key) | not)))
    and all($actual | keys[]; . as $key | ($expected | has($key))
      or defaulted($p + [$key]; $actual[$key]; $source))
  elif ($expected | type) == "array" then
    ($actual | length) == ($expected | length)
    and all(range(0; $expected | length); . as $i |
      $actual[$i] | matches($expected[$i]; $p + [$i]; $source))
  else $actual == $expected end;
length == 1 and ($receipt | length) == 1 and (
  .[0] as $actual | $receipt[0] as $r |
  $actual.metadata.name == (if $kind == "ConfigMap" then "break-glass-deposit-known-hosts" else "boss-break-glass-deposit" end)
  and $actual.kind == $kind and $actual.apiVersion == (if $kind == "ConfigMap" then "v1" else "batch/v1" end)
  and $actual.metadata.namespace == $r.namespace
  and $actual.metadata.uid == (if $kind == "ConfigMap" then $r.config_map.uid else $r.cronjob.uid end)
  and $actual.metadata.deletionTimestamp == null
  and ($actual.metadata.resourceVersion | type == "string" and test("^[1-9][0-9]*$"))
  and (if $kind == "ConfigMap" then $actual.data == $r.data else
    [$source[0].items[] | select(.kind == "CronJob" and .metadata.name == "boss-break-glass-deposit")] as $jobs |
    ($jobs | length) == 1 and ($actual.metadata.labels | type) == "object"
    and all($jobs[0].metadata.labels | to_entries[]; . as $entry |
      $actual.metadata.labels[$entry.key] == $entry.value)
    and ($actual.spec | matches($jobs[0].spec; []; $jobs[0].spec))
  end))
