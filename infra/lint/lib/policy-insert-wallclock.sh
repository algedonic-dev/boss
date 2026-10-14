#!/usr/bin/env bash
# The moved owning PgPolicy adapter retains its existing operational row
# timestamps. Match only its two INSERT column/value mappings: NOW belongs
# to created_at/updated_at, never a business date, expiry input, or scope.
# No whole-file policy-client exemption; an added leak in this file is loud.
policy_operational_insert_allowed() {
  local logical="$1" source="$2" line="$3" context
  [[ "$logical" == 'crates/core/boss-policy-client/src/postgres.rs' ]] || return 1
  context=$(awk -v target="$line" 'NR >= target - 2 && NR <= target { print }' "$source") || return 3
  context=$(printf '%s' "$context" | tr -d '\\[:space:]') || return 3
  case "$context" in
    '"INSERTINTOpolicy_rules(id,role,resource,action,scope,active,created_at,created_by,updated_at,updated_by)VALUES($1,$2,$3,$4,$5,$6,NOW(),$7,NOW(),$7){conflict}"') return 0 ;;
    '"INSERTINTOpolicy_user_overrides(id,user_id,resource,action,scope,reason,expires_at,created_at,created_by)VALUES($1,$2,$3,$4,$5,$6,$7,NOW(),$8){conflict}"') return 0 ;;
  esac
  return 1
}
