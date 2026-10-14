#!/usr/bin/env bash
#
# audit-deposit-keys.sh — reads the authorized_keys lines that grant a
# deposit key one of boss-gcp's root doors, and says which of them give
# that key more than its receiver.
#
#   audit-deposit-keys.sh <authorized_keys file>...
#
# WHY IT EXISTS (backlog dea2236f; N5 of car c1cdb202's review). On the
# first live enrolment of the machine token deposit key (packet
# 96a0f7bb, 2026-10-07) the bare public key went into
# ~david/.ssh/authorized_keys where the line forcing the receiver
# belonged, and for ten to fifteen minutes a key minted for one receiver
# command opened that account's shell. The senders now wait for the
# receiver's greeting before they send a byte (cars c1cdb202 and
# aff9c8c2), so a bare key ALONE is refused at the next push. What a
# greeting cannot see, because the greeting arrives either way:
#
#   * a line that forces the receiver and lacks `restrict` — the key
#     still runs only the receiver, but sshd also hands it a pty,
#     forwarding and the account's ~/.ssh/rc, none of which the
#     installers' printed line allows;
#   * a line that has `restrict` and then re-opens one of those
#     (`restrict,pty`), which sshd honours in order;
#   * the same key ALSO on a bare line (the 96a0f7bb shape beside its
#     repair: the confined line added, the bare one never removed).
#     sshd takes whichever line it reaches first, so today's push may
#     be greeted while the key still opens a shell.
#
# Only a read of the file shows any of these, and boss-gcp's converge
# runs there every half hour: it calls this AS the deposit account, the
# way sshd reads the file, and carries the verdict to its packet, where
# a finding is filed by rule rather than redding the tick.
#
# WHAT IS A RECEIVER LINE. One whose forced command runs something under
# ROOT_DOORS, the directory both receiver installers put their root-owned
# copies in (install-ops-credential-receiver.sh,
# install-runner-credential-receiver.sh; audit_deposit_keys_sh.rs holds
# the three equal), however the path is spelled — `//` and `/./` and
# `..` are read as the kernel reads them (review dd8f18e0, N6; see
# names_root_door). No other key line is judged for what it allows: the
# account's own login keys are its owner's business, and a key this
# cannot tie to a receiver line is one it cannot tell from his.
#
# A receiver line runs that receiver AND NOTHING ELSE: sshd hands a
# forced command to the account's shell, so a command that merely names
# a root door (`echo …; : /usr/local/libexec/boss/x`) runs whatever is
# around it. `[exec ][sudo -<flags> ]<ROOT_DOORS><name>[ <word>…]` is
# the shape every installer prints; any other command naming a root door
# is a finding (review 1fa0d093, N1).
#
# A KEY IS WHAT ssh-keygen READS, COMPARED BY FINGERPRINT. sshd decodes
# a line's key and compares KEYS, and one key has several spellings it
# accepts: review ee6589a9 (B1', B1'') stood a real sshd -i behind an
# RSA bare twin whose e or n carried a leading zero byte, an ed25519
# twin whose blob names its type `ED25519`, and a twin typed
# `rsa-sha2-256` or `rsa-sha2-512`, and each opened the shell ahead of a
# confined receiver line while this audit, comparing base64 TEXT, read
# the file as clean — as it did an unrestricted receiver line typed
# `rsa-sha2-256`, from which its own type check read no key at all.
# Review 1fa0d093 (B1) had found the first members of the same class
# (\r, \v, \f that sshd's base64 decoder skips). So no line's key is
# read here: `ssh-keygen -l` is sshd's own key parser (sshkey_read; its
# advance past options is sshd's but for the blank run after them,
# below), each line is handed to it, and two
# lines carry one key exactly when it prints one fingerprint for both.
#
# OPTIONS ARE SPLIT THE WAY sshd SPLITS THEM: a line has none when the
# key parser reads a key from its first two fields (sshkey_read from the
# line's first byte); otherwise they run to the first space or tab
# outside double quotes, a backslash-quote stepped over as a pair
# (sshkey_advance_past_options), and the key is what the parser reads
# from the two fields after them — after the WHOLE run of spaces and
# tabs that follows, which sshd steps over (skip_space) and ssh-keygen
# does not. Option names match without case and a comma inside quotes
# does not end one. A line the parser reads a key from whose key this
# split does not reach is a FINDING, never clean: the split follows
# ssh-keygen's own loop, so they cannot differ today, and the day a
# release makes them differ the audit cannot say what that line grants.
#
# ssh-keygen -l IS NOT sshd ON THE WHOLE LINE (review dd8f18e0, B3).
# Fed a line with options, it steps over exactly ONE blank after them,
# so `command="…",restrict,pty  <key>` — two spaces, or a tab and a
# space — reads as no key to it while sshd honours it, pty and all. This
# header said a line it reads no key from grants nothing to sshd "either
# — one parser, one release", and on that claim four such lines, each
# alone in its file, read clean while sshd granted a pty and forwarding.
# So the whole-line read is not the last word: where it reads nothing,
# the option split above runs anyway and the key is the one the parser
# reads past the blank run, as sshd reads it. A line grants nothing to
# sshd only when neither read yields a key (an unbalanced quote in its
# options is one: sshd refuses that as an invalid option string).
#
# Such a line is named, not judged; but beside a line that forces a root
# door it is a finding (review ee6589a9, N2): a file that grants a root
# door is judged whole or not called clean. A file carrying a NUL byte
# is UNVERIFIED: sshd stops reading a line there and bash's `read` drops
# the byte and joins what follows, so a verdict over it would be about a
# file sshd never reads.
# Without the key parser nothing can be said, and that is UNVERIFIED.
#
# WHAT IS PRINTED: one `UNCONFINED <file>:<line> — <why>` per finding,
# each naming the line to act on, then one verdict line, last. A key is
# named by its type and fingerprint — never a byte of the line beyond
# its comment and forced command.
#
# EXIT 0 clean (an absent file is clean, and named); 1 at least one
# finding; 2 nothing can be said — no file given, a path that is not a
# readable file, a NUL byte, or no key parser. Never 0 for a file it did
# not read.
set +x
set -uo pipefail
export LC_ALL=C

ME=deposit-key-audit
ROOT_DOORS='/usr/local/libexec/boss/'
# What `restrict` closes and a later option may re-open (sshd(8),
# AUTHORIZED_KEYS FILE FORMAT).
REOPENS='pty port-forwarding agent-forwarding x11-forwarding user-rc'

[ "$#" -ge 1 ] || { echo "$ME: UNVERIFIED — no authorized_keys file was named; usage: $(basename "$0") <file>..." >&2; exit 2; }

# sshd's key parser. Overridable for the tests alone, which stand a
# parser in for one that disagrees with the option split.
SSH_KEYGEN="${BOSS_DEPOSIT_AUDIT_SSH_KEYGEN:-ssh-keygen}"
command -v "$SSH_KEYGEN" >/dev/null 2>&1 \
    || { echo "$ME: UNVERIFIED — $SSH_KEYGEN, the key parser sshd uses and this audit reads every key with, is not on this host, so nothing about the keys any file grants can be said" >&2; exit 2; }

# A CRLF file's blank line is only bytes sshd reads no key from.
SKIPPED=$'\r\v\f'
# The one shape a receiver line's forced command may have (N1 above).
RECEIVER_RE='^(exec )?(sudo( -[A-Za-z]+)* )?'"$ROOT_DOORS"'[A-Za-z0-9_][A-Za-z0-9._/-]*( [A-Za-z0-9._-]+)*$'

# keyfp <text> -> K_FP K_TYPE; 1 when sshd's key parser reads no key
# from it. Fed on stdin, so a line naming a PRIVATE KEY is never taken
# as a key file to load.
keyfp() {
    local out
    out="$(printf '%s\n' "$1" | "$SSH_KEYGEN" -l -E sha256 -f - 2>/dev/null)" || return 1
    read -r _ K_FP _ <<< "$out"
    K_TYPE="${out##*(}"
    K_TYPE="${K_TYPE%)}"
    [ -n "$K_FP" ]
}

# parse_line <line, leading blanks removed> -> P_OPTS P_FP P_TYPE
# P_COMMENT. 1 when sshd's key parser reads no key from the line, whole
# or past its options; 2 when it reads one from the whole line that this
# option split does not reach (the header says why each is what it is).
parse_line() {
    local s="$1" t1 t2 rest whole="" i c q=0
    P_OPTS="" P_FP="" P_TYPE="" P_COMMENT=""
    if keyfp "$s"; then
        whole="$K_FP"
        P_FP="$K_FP"
        P_TYPE="$K_TYPE"
    fi
    IFS=$' \t' read -r t1 t2 rest <<< "$s"
    if [ -n "$whole" ] && keyfp "$t1 $t2"; then
        P_COMMENT="$rest"
    elif [ -z "$whole" ] && [[ $s != *[[:blank:]]* ]]; then
        # No blank, so no key past any options either; and the split
        # below walks a line a byte at a time, which bash makes
        # quadratic — a 100 KB blank-free line cost it 13 s.
        return 1
    else
        # sshkey_advance_past_options: a backslash-quote is stepped over
        # as a pair wherever it stands, inside the quotes or out.
        for ((i = 0; i < ${#s}; i++)); do
            c="${s:i:1}"
            if [ "$c" = '\' ] && [ "${s:i+1:1}" = '"' ]; then
                i=$((i + 1))
                continue
            fi
            if [ "$c" = '"' ]; then
                q=$((1 - q))
                continue
            fi
            if [ "$q" -eq 0 ] && [[ $c == [[:blank:]] ]]; then
                break
            fi
        done
        # No key from the whole line AND none past the options is no key
        # to sshd either; a key from the whole line that the split does
        # not reach is the finding the header names.
        if [ "$q" -ne 0 ]; then
            [ -z "$whole" ] && return 1
            return 2
        fi
        P_OPTS="${s:0:i}"
        # `read` steps over the whole run of spaces and tabs, as sshd's
        # skip_space does and ssh-keygen's one-byte step does not
        # (review dd8f18e0, B3).
        IFS=$' \t' read -r t1 t2 P_COMMENT <<< "${s:i}"
        if ! keyfp "$t1 $t2"; then
            [ -z "$whole" ] && return 1
            return 2
        fi
        if [ -z "$whole" ]; then
            P_FP="$K_FP"
            P_TYPE="$K_TYPE"
            return 0
        fi
    fi
    [ "$K_FP" = "$whole" ] || return 2
}

# names_root_door <forced command>: 0 when it names a root door — the
# text of ROOT_DOORS anywhere in it, or any absolute word of it that,
# read as a path (empty and `.` segments dropped, `..` stepping back),
# lies under ROOT_DOORS. `/usr/local//libexec/boss/x` is the same file
# to the kernel and to the sudo that runs it, and as text it read as no
# receiver line at all, so a line forcing it with no restrict was clean
# (review dd8f18e0, N6). Only which lines are judged reads the path;
# the shape check keeps the text, so a door in any spelling but the one
# the installers print is a finding.
names_root_door() {
    local nonpath='[^A-Za-z0-9._/+-]' w seg
    local -a words segs out
    [[ $1 != *"$ROOT_DOORS"* ]] || return 0
    read -r -a words <<< "${1//$nonpath/ }"
    for w in "${words[@]}"; do
        [ "${w:0:1}" = / ] || continue
        out=()
        IFS=/ read -r -a segs <<< "$w"
        for seg in "${segs[@]}"; do
            case "$seg" in
                '' | .) ;;
                ..) [ "${#out[@]}" -eq 0 ] || unset 'out[-1]' ;;
                *) out+=("$seg") ;;
            esac
        done
        [[ "/$(IFS=/; echo "${out[*]}")/" != "$ROOT_DOORS"* ]] || return 0
    done
    return 1
}

# split_opts: P_OPTS -> OPT_NAMES (" name name ", lower case), OPT_CMD,
# HAS_CMD.
opt_one() {
    local o="$1" name val
    [ -n "$o" ] || return 0
    name="${o%%=*}"
    name="${name,,}"
    OPT_NAMES+="$name "
    if [ "$name" = command ] && [ "$o" != "${o%%=*}" ]; then
        val="${o#*=}"
        val="${val#\"}"
        val="${val%\"}"
        OPT_CMD="${val//\\\"/\"}"
        HAS_CMD=1
    fi
}
split_opts() {
    local s="$P_OPTS" i c q=0 cur=""
    OPT_NAMES=" " OPT_CMD="" HAS_CMD=0
    for ((i = 0; i < ${#s}; i++)); do
        c="${s:i:1}"
        if [ "$q" -eq 1 ] && [ "$c" = '\' ] && [ "${s:i+1:1}" = '"' ]; then
            cur+='\"'
            i=$((i + 1))
            continue
        fi
        if [ "$q" -eq 0 ] && [ "$c" = , ]; then
            opt_one "$cur"
            cur=""
            continue
        fi
        [ "$c" != '"' ] || q=$((1 - q))
        cur+="$c"
    done
    opt_one "$cur"
}

FINDINGS=0
finding() { # <where> <why>
    FINDINGS=$((FINDINGS + 1))
    echo "$ME: UNCONFINED $1 — $2"
}
# Printable bytes, bounded: a comment is the file owner's free text.
shown() { printf '%s' "$1" | tr -c '[:print:]' '?' | cut -c1-120; }

declare -a L_WHERE=() L_FP=() L_CMD=() L_HASCMD=() L_RECV=() L_ID=() UNPARSED_AT=()
RECEIVERS=0 READ="" ABSENT=""

for f in "$@"; do
    if [ ! -e "$f" ] && [ ! -L "$f" ]; then
        ABSENT+=" $f"
        continue
    fi
    if [ ! -f "$f" ] || [ ! -r "$f" ]; then
        echo "$ME: UNVERIFIED — $f is not a readable file, so nothing about the keys it grants can be said" >&2
        exit 2
    fi
    if [ "$(tr -d '\000' <"$f" | wc -c)" -ne "$(wc -c <"$f")" ]; then
        echo "$ME: UNVERIFIED — $f carries a NUL byte: sshd stops reading a line there and this audit cannot, so nothing about the keys it grants can be said" >&2
        exit 2
    fi
    n=0
    while IFS= read -r line || [ -n "$line" ]; do
        n=$((n + 1))
        t="${line#"${line%%[![:blank:]]*}"}"
        # Blank, or only bytes sshd reads no key from (a CRLF file's
        # empty line): nothing to judge.
        [ -n "${t//[$SKIPPED]/}" ] || continue
        [ "${t:0:1}" != '#' ] || continue
        prc=0
        parse_line "$t" || prc=$?
        if [ "$prc" -eq 1 ]; then
            UNPARSED_AT+=("$f:$n")
            continue
        fi
        if [ "$prc" -ne 0 ]; then
            finding "$f:$n" "sshd's key parser (ssh-keygen) reads the key $P_TYPE $P_FP from this line, and this audit's option split does not reach that key, so what the line grants it cannot say. Read it by hand: remove it, or rewrite it as a plain key line or command=\"...\",restrict <key>."
            continue
        fi
        split_opts
        i=${#L_WHERE[@]}
        L_WHERE[i]="$f:$n"
        L_FP[i]="$P_FP"
        L_CMD[i]="$OPT_CMD"
        L_HASCMD[i]="$HAS_CMD"
        L_ID[i]="$P_TYPE $P_FP${P_COMMENT:+ ($(shown "$P_COMMENT"))}"
        L_RECV[i]=0
        if [ "$HAS_CMD" = 1 ] && names_root_door "$OPT_CMD"; then
            L_RECV[i]=1
            RECEIVERS=$((RECEIVERS + 1))
            if ! [[ $OPT_CMD =~ $RECEIVER_RE ]] || [[ $OPT_CMD == *..* ]]; then
                finding "$f:$n" "the key ${L_ID[i]} is forced to '$(shown "$OPT_CMD")', which names a root door under $ROOT_DOORS but is not that receiver alone, spelled as the installers print it: sshd hands a forced command to the account's shell, so whatever else it says runs too. Remove this line and place the authorized_keys_line on its enroll step whole."
            elif [[ $OPT_NAMES != *" restrict "* ]]; then
                finding "$f:$n" "the key ${L_ID[i]} is forced to '$(shown "$OPT_CMD")' WITHOUT restrict, so sshd also gives it a pty, forwarding and the account's ~/.ssh/rc. Remove this line and place the authorized_keys_line on its enroll step whole: command=\"...\",restrict <key>."
            else
                reopened=""
                for o in $REOPENS; do
                    [[ $OPT_NAMES != *" $o "* ]] || reopened+=" $o"
                done
                [ -z "$reopened" ] \
                    || finding "$f:$n" "the key ${L_ID[i]} is forced to '$(shown "$OPT_CMD")' with restrict and then re-opens:$reopened. Remove this line and place the authorized_keys_line on its enroll step whole."
            fi
        fi
    done < "$f"
    READ+=" $f"
done

# A RECEIVER'S KEY ON ANY OTHER LINE. The receiver line it belongs to is
# the first one carrying that key; a second receiver line with the same
# key and the same command adds nothing and is let be.
for ((j = 0; j < ${#L_WHERE[@]}; j++)); do
    for ((r = 0; r < ${#L_WHERE[@]}; r++)); do
        [ "$r" -ne "$j" ] && [ "${L_RECV[r]}" = 1 ] && [ "${L_FP[r]}" = "${L_FP[j]}" ] || continue
        if [ "${L_RECV[j]}" = 1 ]; then
            [ "${L_CMD[j]}" != "${L_CMD[r]}" ] && [ "$j" -gt "$r" ] || break
        fi
        if [ "${L_HASCMD[j]}" = 1 ]; then
            what="forced to '$(shown "${L_CMD[j]}")' instead"
        else
            what="with NO forced command, so whoever holds the deposit key gets this account's shell"
        fi
        finding "${L_WHERE[j]}" "the key ${L_ID[j]}, forced to '$(shown "${L_CMD[r]}")' on ${L_WHERE[r]}, is also on this line $what; sshd uses whichever of the two it reaches first. Remove this line."
        break
    done
done

# A LINE NO KEY WAS READ FROM, BESIDE A ROOT DOOR (review ee6589a9,
# N2). It grants nothing to the sshd whose parser read it; but a file set
# that grants a root door is judged whole or not called clean, so each is
# a finding there — the first ten named, the count of the rest given.
UNPARSED=${#UNPARSED_AT[@]}
tail_note="${ABSENT:+; absent:$ABSENT}"
if [ "$UNPARSED" -gt 0 ] && [ "$RECEIVERS" -gt 0 ]; then
    for where in "${UNPARSED_AT[@]:0:10}"; do
        finding "$where" "sshd's key parser (ssh-keygen) reads no key from this line, in a file set where a deposit key reaches a root door: what a line nobody can read grants cannot be said, so the files are not called clean. Remove it or make it a # comment."
    done
    [ "$UNPARSED" -le 10 ] || tail_note+="; $((UNPARSED - 10)) more line(s) no key was read from"
elif [ "$UNPARSED" -gt 0 ]; then
    tail_note+="; $UNPARSED line(s) sshd's key parser reads no key from, not judged:${UNPARSED_AT[*]:0:10}"
    [ "$UNPARSED" -le 10 ] || tail_note+=" and $((UNPARSED - 10)) more"
fi
[ -n "$READ" ] || READ=" (none present)"
if [ "$FINDINGS" -gt 0 ]; then
    echo "$ME: $FINDINGS finding(s) in$READ — a deposit key reaches more than its receiver, or a line beside one cannot be judged; each UNCONFINED line above names the line to act on (backlog dea2236f)$tail_note"
    exit 1
fi
echo "$ME: clean — $RECEIVERS line(s) in$READ force a root door under $ROOT_DOORS, each with restrict and nothing re-opened, and no key of theirs is on another line$tail_note"
exit 0
