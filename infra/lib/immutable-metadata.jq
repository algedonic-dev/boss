# Generic immutable source selection. Presence that cannot be verified is an error.
def immutable_view($envelope; $pointer; $version; $locators):
  . as $raw
  | if has($envelope) | not then .
    else .[$envelope] as $bound
      | if ($bound | type) != "object" or $bound.contract_version != $version
           or ._immutable_metadata[$envelope] != $bound
        then error("immutable source binding is unreadable")
        else ($bound | getpath($pointer)) as $source
          | if ($source | type) != "object" then error("immutable source is not an object")
            elif any($locators[]; . as $key | ($raw | has($key)) != ($source | has($key)) or $raw[$key] != $source[$key])
            then error("immutable source locator mismatch")
            else $source end
        end
    end;
def gate_source: immutable_view("gate_submission"; ["source"]; 1; ["branch", "sha", "agent_run", "worktree"]);
