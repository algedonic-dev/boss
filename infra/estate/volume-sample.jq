# The pure acceptance port. The sample's timestamp and native identity
# ride intact; rereading it is not another mounted measurement. There is
# no floor here: compare_volumes remains the only disk_tight judge.
def instant:
  if type == "string" and test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]+)?Z$")
  # Fraction strings retain native precision without float epoch rounding;
  # removing trailing zeroes also makes equivalent spellings the same instant.
  then capture("^(?<whole>[^.]+)(?:\\.(?<fraction>[0-9]+))?Z$") as $t
    | [($t.whole + "Z" | fromdateiso8601), ($t.fraction // "" | sub("0+$"; ""))]
  else error("missing or malformed UTC timestamp") end;
def integer: type == "number" and floor == . and . >= 0 and . <= 9007199254740991;
def require($condition; $reason): if $condition then . else error($reason) end;
require(($native | length) == 1 and ($attempts | length) == 1;
    "the native packet and Kubernetes inventory must each be one entire JSON document")
| $native[0] as $all
| require(($all | type) == "object" and ($all.data | type) == "array" and
    ($all.total | integer) and $all.total == ($all.data | length); "the native job list is malformed or truncated")
| require(($all.data | length) > 0; "no native maintenance packet names a sample")
| require(all($all.data[]; .kind == $source.workflow and .partition == "real" and .simulated == false); "the native answer has a wrong workflow or non-real packet")
| ($all.data | sort_by(.opened_at | instant)) as $jobs
| $jobs[-1] as $job
| require(($jobs | map(select((.opened_at | instant) == ($job.opened_at | instant))) | length) == 1; "the newest native packet is ambiguous")
| ($job.steps | map(select(.spec_slug == $source.step))) as $steps
| require(($steps | length) == 1; "the native step is missing or ambiguous")
| $steps[0] as $step | $step.metadata.filesystem_sample as $s
| require(($s | type) == "object"; "the newest native attempt has no filesystem sample")
| require($s.schema == 1 and $s.namespace == $namespace and $s.claim == $claim and
    $s.volume == $volume and $s.mount_source == ("/dev/longhorn/" + $volume); "the sample does not name this namespace, claim and backing volume")
| require($s.position == "final" and ($s.unread // "") == ""; "the newest sample failed or is only a start reading")
| require($step.job_id == $job.id and $step.status == "completed" and
    $step.assignee_id == $source.actor and $step.completed_by == $source.actor and
    $step.metadata.result == "ok"; "the native step did not complete under its recorded owner")
| require(($s.pod_uid | type) == "string" and ($s.pod_uid | length) > 0 and
    ($s.pod | type) == "string" and ($s.pod | length) > 0 and
    ($s.node | type) == "string" and ($s.node | length) > 0 and
    ($s.mount | type) == "string" and ($s.mount | startswith("/")); "the sample's pod or mount identity is absent")
| $attempts[0] as $inventory
| require(($inventory.items | type) == "array" and ($inventory.metadata.continue // "") == ""; "the Kubernetes Job inventory is malformed or truncated")
| ($inventory.items | map(select(any(.metadata.ownerReferences[]?;
    .kind == "CronJob" and .name == $source.cronjob and .controller == true)))
    | sort_by(.metadata.creationTimestamp | instant)) as $attempts
| require(($attempts | length) > 0; "no retained native Kubernetes Job identifies this producer")
| $attempts[-1] as $attempt
| require(($attempts | map(select((.metadata.creationTimestamp | instant) == ($attempt.metadata.creationTimestamp | instant))) | length) == 1;
    "the newest Kubernetes attempt is ambiguous")
| require(($attempt.metadata.uid | type) == "string" and ($attempt.metadata.uid | length) > 0 and
    ($s.kubernetes_job_uid | type) == "string" and ($s.kubernetes_job_uid | length) > 0;
    "the sample or Kubernetes controller identity is absent or malformed")
| require($attempt.metadata.uid == $s.kubernetes_job_uid; "a newer Kubernetes attempt has no matching recorded final sample")
| require($attempt.metadata.namespace == $namespace and $attempt.status.succeeded == 1 and
    ($attempt.status.failed // 0) == 0 and ($attempt.status.active // 0) == 0 and
    any($attempt.status.conditions[]?; .type == "Complete" and .status == "True") and
    (all($attempt.status.conditions[]?; .type != "Failed" or .status != "True"));
    "the latest Kubernetes Job is not a successful single-attempt run")
| ($s.observed_at | instant) as $at
| require(($job.opened_at | instant) <= $at and ($attempt.metadata.creationTimestamp | instant) <= $at and
    ($attempt.status.completionTime | instant) >= $at and ($attempt.status.completionTime | instant) <= [$now, ""] and
    ($step.completed_at | instant) >= $at and ($step.completed_at | instant) <= [$now, ""];
    "the sample and native start/completion timestamps are inconsistent")
| require($at <= [$now, ""] and $at >= [($now - $max_age), ""]; "the mounted sample is future-dated or expired at the declared cadence")
| require(($s.capacity_bytes | integer) and $s.capacity_bytes > 0 and
    ($s.free_bytes | integer) and $s.free_bytes <= $s.capacity_bytes and
    ($s.used_bytes | integer) and $s.used_bytes <= $s.capacity_bytes and
    $s.used_bytes + $s.free_bytes <= $s.capacity_bytes; "the sample's byte figures are malformed or inconsistent")
| {capacity_bytes: $s.capacity_bytes, used_bytes: $s.used_bytes, free_bytes: $s.free_bytes,
   sample: $s, native_job_id: $job.id, native_step_id: $step.id,
   native_completed_at: $step.completed_at, native_completed_by: $step.completed_by,
   max_age_seconds: $max_age, source: "dated native mounted statfs"}
