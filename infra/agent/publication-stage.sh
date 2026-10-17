# Optional agent publication follows core convergence. Full command streams
# remain private originals; its separate verdict rides the maintenance packet.
agent_image_publication_stage() { # publisher full-generation root-owned-state
    local publisher=$1 generation=$2 state=$3 attempt rc=0 record
    attempt=$(mktemp -d "$state/agent-publication.$generation.XXXXXXXX") || {
        run_summary_json agent_image_publication '{"schema":1,"state":"not-run","reason":"cannot retain publication originals"}'
        echo 'agent-image: NOT RUN — cannot retain publication originals' >&2
        return 0
    }
    if ! : > "$attempt/stdout" || ! : > "$attempt/stderr"; then
        run_summary_json agent_image_publication '{"schema":1,"state":"not-run","reason":"cannot retain publication streams"}'
        return 0
    fi
    record=$(/usr/bin/python3 -I - "$generation" "$attempt" <<'PYINITIAL'
import json,sys
from pathlib import Path
sha,directory=sys.argv[1:]
row={'schema':1,'generation':sha,'state':'unconfirmed','exit':None,
     'stdout':directory+'/stdout','stderr':directory+'/stderr','launch_ready':False,
     'reason':'publication invocation has no terminal receipt yet'}
Path(directory+'/receipt.json').write_text(json.dumps(row,indent=2)+'\n')
print(json.dumps(row))
PYINITIAL
    ) || {
        run_summary_json agent_image_publication '{"schema":1,"state":"not-run","reason":"cannot retain pre-invocation receipt"}'
        return 0
    }
    run_summary_json agent_image_publication "$record"
    /usr/bin/timeout --signal=TERM --kill-after=30s 1800 "$publisher" publish "$generation" > "$attempt/stdout" 2> "$attempt/stderr" || rc=$?
    record=$(/usr/bin/python3 -I - "$generation" "$rc" "$attempt" <<'PY'
import json,sys
from pathlib import Path
sha,code,directory=sys.argv[1:];code=int(code)
row={'schema':1,'generation':sha,'state':'published' if code==0 else 'failed','exit':code,'stdout':directory+'/stdout','stderr':directory+'/stderr','launch_ready':False}
if code==0:
    try:
        native=json.loads(Path(directory+'/stdout').read_text().splitlines()[-1])
        if native['generation']!=sha or native['kind']!='agent-image' or native['launch_ready'] is not False: raise ValueError('witness does not match publication')
        row['image']=native['image'];row['witness']=native
    except (ValueError,KeyError,IndexError):
        row['state']='invalid-receipt';row['reason']='publisher returned zero without an exact image-only witness'
Path(directory+'/receipt.json').write_text(json.dumps(row,indent=2)+'\n')
print(json.dumps(row))
PY
    ) || {
        record='{"schema":1,"state":"receipt-failed","reason":"publication original streams retained; receipt encoder failed"}'
    }
    run_summary_json agent_image_publication "$record"
    printf '%s\n' "agent-image publication: $record"
    if [ "$rc" != 0 ]; then
        cat "$attempt/stdout" "$attempt/stderr" >&2
    fi
    return 0
}
