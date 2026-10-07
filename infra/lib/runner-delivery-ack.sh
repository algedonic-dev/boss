# shellcheck source=infra/lib/jq.sh
. "$INFRA/lib/jq.sh"
# Shared typed runner delivery acknowledgment (approved104; native49de).
# The caller has proved its installed file through the actual resolver.
# Seven inputs are explicit; HTTP sender/header and body-file ownership stay
# with the caller. rc and DELIVERY_STATE are the existing result contract.
runner_delivery_ack() {
    local CRED="$1" HOST="$2" NEXT_FOR="$3" CUR="$4" DEST="$5"
    local DELIVERY_CONTEXT="$6" PKT_STEP="$7"
    local body l8 identity expected receipt attempt completed_receipt
    DELIVERY_RECEIPT=""
    l8="$(last8 "$CUR")"
    if ! body="$(jq -nec --arg c "$CRED" --arg job "$NEXT_FOR" \
        --argjson context "$DELIVERY_CONTEXT" '$context
        | select(.credential_id == $c and .job_id == $job
            and (.attempt | type) == "string" and (.attempt | length) == 36
            and (.secret_uid | type) == "string" and (.secret_uid | length) > 0)
        | {job_id: .job_id, attempt: .attempt, secret_uid: .secret_uid}')"; then
        DELIVERY_STATE="not recorded: delivery body did not serialize"
        rc=1
        return 0
    fi
    identity="$body"
    if ! attempt="$(jq -r '.attempt' <<< "$identity")"; then
        DELIVERY_STATE="not recorded: delivery attempt did not serialize"
        rc=1
        return 0
    fi
    api_get "/api/jobs/$NEXT_FOR/steps/$PKT_STEP/version" -H "$RC_HDR"
    if [ "$CODE" != 200 ] || ! jq_doc_file "$BODY" || ! expected="$(jq -ec '
        select((.step.status == "ready" or .step.status == "active" or .step.status == "completed")
            and (.step.assignee_id == null or .step.assignee_id == "automation:ops-runner")
            and (.version | type) == "string" and (.version | length) > 0)
        | {status: .step.status, assignee_id: .step.assignee_id, version: .version}' "$BODY")"; then
        DELIVERY_STATE="not recorded: delivered step version or holder refused (HTTP $CODE); no completion attempted"
        rc=1
        return 0
    fi
    completed_receipt="$(jq -c 'if .step.status == "completed" then .step.metadata.delivered_receipt else null end' "$BODY")" || {
        DELIVERY_STATE="not recorded: completed delivery receipt unreadable"
        rc=1
        return 0
    }
    CODE=""
    if ! CODE="$("$API_CURL" -sS -X POST "${HDRS[@]}" -H "$RC_HDR" \
        -H 'content-type: application/json' -d "$identity" -o "$BODY" -w '%{http_code}' \
        "$BOSS_JOBS_URL/api/credentials/$CRED/delivery" 2>/dev/null)"; then CODE=""; fi
    if [ "$CODE" != 202 ] || ! jq_doc_file "$BODY" || ! receipt="$(jq -ec --arg c "$CRED" --arg job "$NEXT_FOR" \
        --arg host "$HOST" --arg attempt "$attempt" '
        select(.recorded == true and (.observation.outcome == "recorded" or .observation.outcome == "replayed"))
        | .observation.receipt
        | select(.version == 1 and .credential_id == $c and .phase == "verified"
            and .actor == "automation:ops-runner" and .observation_id == ($attempt + ":delivered")
            and (.event_id | type) == "string" and (.timestamp | type) == "string")
        | select((.evidence_json | fromjson) as $e | $e.purpose == "authenticated-runner-delivery"
            and $e.job_id == $job and $e.host == $host and $e.attempt == $attempt)' "$BODY")"; then
        DELIVERY_STATE="not recorded: authenticated delivery owner refused or its original receipt is unavailable (HTTP $CODE)"
        rc=1
        return 0
    fi
    if [ "$(jq -r .status <<< "$expected")" = completed ]; then
        # A lost transport acknowledgment is recovered from two ORIGINAL
        # observations: the delivery owner replay and the completed step.
        # Never fabricate a new expected version or complete a second time.
        if ! jq -ne --argjson recorded "$completed_receipt" --argjson original "$receipt" '$recorded == $original' >/dev/null; then
            DELIVERY_STATE="not recorded: completed step does not conserve the original delivery receipt"
            rc=1
            return 0
        fi
        DELIVERY_STATE="recorded on ${NEXT_FOR:0:8}: original completed delivery observed; promotion not observed"
        DELIVERY_RECEIPT="$receipt"
        return 0
    fi
    if ! body="$(jq -nc --arg operation "$attempt" --argjson expected "$expected" \
        --argjson receipt "$receipt" --arg l8 "$l8" --arg to "$HOST:$DEST" \
        '{operation_id: $operation, expected: $expected,
          evidence: {delivered_last_eight: $l8, delivered_to: $to, delivered_receipt: $receipt}}')"; then
        DELIVERY_STATE="not recorded: completion body did not serialize; original delivery receipt retained"
        rc=1
        return 0
    fi
    CODE=""
    if ! CODE="$("$API_CURL" -sS -X POST "${HDRS[@]}" -H "$RC_HDR" \
        -H 'content-type: application/json' -d "$body" -o "$BODY" -w '%{http_code}' \
        "$BOSS_JOBS_URL/api/jobs/$NEXT_FOR/steps/$PKT_STEP/complete-if" 2>/dev/null)"; then CODE=""; fi
    if [ "$CODE" != 200 ] || ! jq_doc_file "$BODY" || ! jq -e '.outcome == "completed" or .outcome == "replayed"' "$BODY" >/dev/null 2>&1; then
        DELIVERY_STATE="not recorded: conditional completion refused (HTTP $CODE); original delivery receipt retained"
        rc=1
        return 0
    fi
    DELIVERY_STATE="recorded on ${NEXT_FOR:0:8}: delivered …$l8 (the jobs API resolves it to $SEEN)"
    DELIVERY_RECEIPT="$receipt"
}
