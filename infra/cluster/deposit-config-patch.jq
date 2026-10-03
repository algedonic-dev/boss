# Produce explicit compare-and-set requests, never an apply annotation
# that a client may omit from its PATCH. The source is client-only typed
# conversion of the reviewed, image-pinned template, not live code.
if ($receipt | length) != 1 or ($source | length) != 1
  or ($receipt[0] | type) != "object" or ($source[0] | type) != "object"
  or $source[0].kind != "List" or ($source[0].items | type) != "array"
  then error("deposit-config: incomplete typed template") else
  $receipt[0] as $r |
  [$source[0].items[] | select(.kind == "CronJob" and .apiVersion == "batch/v1"
    and .metadata.name == "boss-break-glass-deposit" and .metadata.namespace == $namespace)] as $jobs |
  if ($jobs | length) != 1 or ($jobs[0].spec | type) != "object"
    or ($jobs[0].metadata.labels | type) != "object" then error("deposit-config: ambiguous typed template") else
    $jobs[0] as $job |
    {config_map:[
        {op:"test",path:"/metadata/uid",value:$r.config_map.uid},
        {op:"test",path:"/metadata/resourceVersion",value:$r.config_map.resource_version},
        {op:"add",path:"/data",value:$r.data}],
      cronjob:[
        {op:"test",path:"/metadata/uid",value:$r.cronjob.uid},
        {op:"test",path:"/metadata/resourceVersion",value:$r.cronjob.resource_version},
        {op:"add",path:"/metadata/labels",value:$job.metadata.labels},
        {op:"add",path:"/spec",value:$job.spec}]}
  end
end
