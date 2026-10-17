# Deployment selection uses the existing image's own complete input judgement.
# Return 0: arrived as data; 75: eligible data awaits native publication/equality;
# 1: unknown or software inputs retain the existing image build/roll path.
delivery_channel_stage() { # REPOSITORY FULL_HEAD LAST_IMAGE_TAG CLI_STORE [ROOT_READER]
    local repo="$1" head="$2" last="$3" store="$4" reader="${5:-}" image observation here
    here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
    . "$here/../estate/cli-compatible.sh"
    if ! image=$(readlink "$store/current"); then
        run_summary_field delivery_channel 'software: last-converged image link is unread'
        return 1
    fi
    if [ -z "$image" ] || [ "${image:0:7}" != "$last" ]; then
        run_summary_field delivery_channel 'software: no matching last-converged image CLI'
        return 1
    fi
    if ! observation=$(cli_image_observation "$store" "$image" "$head" "$repo" "$reader"); then
        run_summary_field delivery_channel 'software: image input or native eligibility unread'
        return 1
    fi
    run_summary_json_file delivery_evidence "$observation"
    run_summary_field delivery_evidence_original "$observation"
    run_summary_field delivery_evidence_sha256 "$(sha256sum "$observation" | cut -d' ' -f1)"
    if jq_doc_file "$observation" && jq -e '.plan.route == "data" and .compiler_inputs_equal == true and .delivery_evidence.selection_ready == true' "$observation" >/dev/null; then
        run_summary_field delivery_channel data
        if jq_doc_file "$observation" && jq -e '.compatible == true and .delivery_evidence.arrived == true' "$observation" >/dev/null; then
            echo "delivery channel: data arrived at $head; actual image remains $image; whole receipt: $observation"
            return 0
        fi
        echo "delivery channel: data publication/equality pending at $head; actual image remains $image; whole receipt: $observation"
        return 75
    fi
    run_summary_field delivery_channel 'software: complete lighter eligibility not proven'
    return 1
}
