# Sourced by bounded record readers. The caller supplies job_pages_get(query).
# Every original page and error remains under the requested prefix.
read_job_pages() (
    query=$1; prefix=$2; offset=0; expected=; pages=0
    printf '[]\n' >"$prefix.rows" || return 1
    while [ "$pages" -lt 1000 ]; do
        page="$prefix.page-$pages.json"
        job_pages_get "$query&limit=50&full=true&offset=$offset" >"$page" 2>"$prefix.page-$pages.stderr" || return 1
        shape=$(jq -r 'if (.data|type)=="array" and (.total|type)=="number" and .total>=0 and .total==(.total|floor) and all(.data[]; (.id|type)=="string" and .id!="") then "valid" else "invalid" end' "$page" 2>>"$prefix.page-$pages.stderr") || return 1
        [ "$shape" = valid ] || return 1
        total=$(jq -r .total "$page"); count=$(jq -r '.data|length' "$page")
        [ -z "$expected" ] && expected=$total
        [ "$total" = "$expected" ] || return 1
        jq -s '.[0] + .[1].data' "$prefix.rows" "$page" >"$prefix.next" || return 1
        mv "$prefix.next" "$prefix.rows" || return 1
        unique=$(jq -r '([.[].id]|unique|length)==length' "$prefix.rows") || return 1
        [ "$unique" = true ] || return 1
        offset=$((offset + count)); pages=$((pages + 1))
        [ "$offset" -le "$total" ] || return 1
        [ "$offset" -ne "$total" ] || { cat "$prefix.rows" || return 1; return 0; }
        [ "$count" -gt 0 ] || return 1
    done
    return 1
)
