# Deployment data only: no command, credential or grant comes from these reads.
def nonempty: type == "string" and length > 0;
def object($kind; $version; $name; $namespace):
  type == "object" and .kind == $kind and .apiVersion == $version
  and .metadata.name == $name and .metadata.namespace == $namespace
  and (.metadata.uid | nonempty and test("^[A-Za-z0-9-]{1,128}$"))
  and (.metadata.resourceVersion | nonempty and test("^[1-9][0-9]*$"))
  and .metadata.deletionTimestamp == null;
def host:
  type == "string" and length <= 253
  and test("^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$")
  and (split(".") | all(.[]; length > 0 and length <= 63
    and test("^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?$")));
def endpoint:
  type == "string" and test("^https://[A-Za-z0-9.-]+:[0-9]{1,5}$")
  and (capture("^https://(?<host>[^:]+):(?<port>[0-9]+)$")
    | (.host | host) and (.port | tonumber | . > 0 and . <= 65535));
def target:
  type == "string" and test("^[a-z_][a-z0-9_-]{0,31}@[A-Za-z0-9.-]+$")
  and (split("@")[0] != "root") and (split("@")[1] | host);
def literal:
  has("value") and (.value | type == "string") and (has("valueFrom") | not);
def reference($key):
  (has("value") | not) and .valueFrom ==
    {configMapKeyRef:{name:"break-glass-deposit-known-hosts",key:$key}};
def refuse: error("deposit-config: incomplete, ambiguous or invalid deployment inputs");

if ($cm | length) != 1 or ($cron | length) != 1 then refuse else
  $cm[0] as $c | $cron[0] as $j |
  if ($c | object("ConfigMap"; "v1"; "break-glass-deposit-known-hosts"; $namespace) | not)
    or ($j | object("CronJob"; "batch/v1"; "boss-break-glass-deposit"; $namespace) | not)
    or ($c.data | type) != "object" then refuse else
    $j.spec.jobTemplate.spec.template.spec.containers as $containers |
    if ($containers | type) != "array" or ($containers | length) != 1
      or $containers[0].name != "chore" or ($containers[0].env | type) != "array"
      then refuse else
      [$containers[0].env[] | select(.name == "BOSS_DEPOSIT_API_SERVER")] as $server |
      [$containers[0].env[] | select(.name == "BOSS_DEPOSIT_TARGET")] as $destination |
      if ($server | length) != 1 or ($destination | length) != 1 then refuse else
        (if ($c.data | keys) == ["known_hosts"]
            and ($server[0] | literal) and ($destination[0] | literal) then
            {mode:"legacy-migration",data:{known_hosts:$c.data.known_hosts,
              api_server:$server[0].value,target:$destination[0].value}}
          elif ($c.data | keys) == ["api_server","known_hosts","target"]
            and ($server[0] | reference("api_server")) and ($destination[0] | reference("target")) then
            {mode:"config-map",data:$c.data}
          elif ($c.data | keys) == ["api_server","known_hosts","target"]
            and ($server[0] | literal) and ($destination[0] | literal)
            and $c.data.api_server == $server[0].value and $c.data.target == $destination[0].value then
            {mode:"resume-migration",data:$c.data}
          else refuse end) as $result |
        if ($result.data.api_server | endpoint | not) or ($result.data.target | target | not)
          or ($result.data.known_hosts | type) != "string" then refuse else
          ($result.data.target | split("@")[1]) as $host |
          # Exactly one clear-text host and one ed25519 pin, with at most
          # one final LF. Aliases, wildcards, extra keys and SSH options
          # are not the existing deposit's identity contract.
          if ($result.data.known_hosts | test("^[A-Za-z0-9.-]+ ssh-ed25519 [A-Za-z0-9+/]{68}\\n?$"))
            and ($result.data.known_hosts | split(" ")[0]) == $host then
            $result + {phase:"validated",config_map:{uid:$c.metadata.uid,resource_version:$c.metadata.resourceVersion},
              cronjob:{uid:$j.metadata.uid,resource_version:$j.metadata.resourceVersion},namespace:$namespace}
          else refuse end
        end
      end
    end
  end
end
