#!/usr/bin/env bash
#
# audit-deposit-keys.sh — reads the authorized_keys lines that grant a
# deposit key one of boss-gcp's root doors, and says which of them give
# that key more than its receiver.
#
#   audit-deposit-keys.sh [--prepared-key <key>]... [--accept-env <patterns>] <authorized_keys file>...
#   audit-deposit-keys.sh --keys-files-of <account>     (as root)
#
# The second form prints, one per line, `--accept-env`, the AcceptEnv
# patterns sshd applies to <account> (an empty line for none), and the
# files sshd reads <account>'s keys from — the arguments the converge
# then hands to the first (below, WHICH FILES and ACCEPTENV).
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
# names_root_door) — and every line carrying a PREPARED key (below). No
# other key line is judged for what it allows: the account's own login
# keys are its owner's business, and a key this cannot tie to a receiver
# line or a prepare packet is one it cannot tell from his.
#
# A PREPARED KEY IS A RECEIVER KEY, WHATEVER ITS LINE SAYS (backlog
# 77920d6a; review 728c6f76 of car 3cd92f7a). `--prepared-key <public
# key>`, once per key and before the files, names a key a prepare packet
# recorded on its install step (public_key): a deposit key, never a
# login. Recognising a receiver line by its text alone missed the shell:
# sshd hands the forced command to the account's shell, which reads
# `lib'exec'`, `bo\ss`, `$L` and `libexe?` as the root door while the
# text match read no door at all, so such a line with no restrict was
# clean while sshd granted a pty, forwarding and ~/.ssh/rc — and the
# prepared key on a bare line with no confined twin read as one of the
# owner's own keys. A line carrying a prepared key is judged by that key,
# compared by fingerprint like every key here: with no forced command it
# is a shell, with one this audit does not read as a root door it is a
# command the account's shell runs, and each is a finding; with one it
# does, it is judged as every receiver line is. A prepared key the key
# parser reads no key from is UNVERIFIED: the lines it was to find
# cannot be found.
#
# A PREPARED KEY IS BOUND TO ITS OWN RECEIVER (backlog c422cf68). Judged
# only for forcing SOME root door, the runner's deposit key forced to
# the machine-token receiver — or the other way round — read clean,
# confined and restricted, while it reached a door it was never prepared
# or enrolled for; ops-credential-recv serves two keys by its argument,
# so not even the door alone tells them apart. So a --prepared-key value
# may be the whole line the key was prepared with, as the broker joins it
# onto the enroll step (`authorized_keys_line`: `command="<the rule's
# forced_command>",restrict <key>`), read by this audit's own split: then
# every line carrying that key must force exactly that command, and one
# forcing any other is a finding naming both. A key given alone is judged
# as above and bound to nothing. A value whose options force no command
# binds its key to nothing, and one key given two different commands
# cannot be judged against either: each is UNVERIFIED (exit 2), never a
# key judged by fingerprint alone.
#
# A receiver line runs that receiver AND NOTHING ELSE: sshd hands a
# forced command to the account's shell, so a command that merely names
# a root door (`echo …; : /usr/local/libexec/boss/x`) runs whatever is
# around it. `[exec ][sudo -n ]<ROOT_DOORS><name>[ <word>…]` is the
# shape every installer prints; any other command naming a root door is
# a finding (review 1fa0d093, N1). The one sudo flag is `-n`, the one
# every installer prints: the shape used to take sudo with ANY run of
# flags and leave sudoers alone to bound them, and `-E` hands the
# receiver the caller's environment while `-s` and `-i` run it through a
# shell (backlog e85c4dbd; review 1fa0d093, N5).
#
# A receiver line sets no environment. With PermitUserEnvironment an
# `environment=` option sets what the account's shell runs the forced
# command with — BASH_ENV alone has bash source a file of the line
# author's choosing before the receiver starts — and no installer
# prints one, so on a receiver line it is a finding, whatever sshd's
# config says today (backlog e85c4dbd; review 1fa0d093, N4).
#
# ACCEPTENV IS THE SAME ROUTE FROM THE OTHER END (backlog 54104a05). A
# client asks sshd to set variables, and sshd sets each one its
# AcceptEnv admits — `restrict` closes no part of it — before it hands
# the forced command to the account's shell, so an AcceptEnv admitting
# BASH_ENV lets whoever holds a deposit key choose what bash runs ahead
# of the receiver, no line of the file required. `--keys-files-of`
# reads it from the same `sshd -T -C user=<account>` the files come
# from and prints it as `--accept-env <patterns>` ahead of them; beside
# a line reaching a root door, a pattern admitting any name in
# SHELL_READS (below) is a finding, matched as sshd matches it. Without
# the option the verdict says AcceptEnv was not read.
#
# WHICH FILES. The ones sshd reads for the deposit account, which the
# converge asks sshd for (`--keys-files-of`, as root: `sshd -T` loads the
# host keys) rather than assuming the default pair under the home — a
# host whose sshd reads another AuthorizedKeysFile was audited on files
# sshd never opens, and read clean (backlog e85c4dbd; review dd8f18e0,
# N7). `-C user=<account>` applies a Match User block; a Match on Host or
# Address is read as not matching, since which address a sender comes
# from is not this host's to know. Each pattern is expanded as sshd
# expands it (%h, %u, %U, %%; a relative path under the home), and a
# list that cannot be read — sshd refusing, no line, `none`, a token
# sshd would refuse — is UNVERIFIED, never the default pair.
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
# EXIT 0 clean (an absent file beside one read is passed over, and
# named); 1 at least one finding; 2 nothing can be said — no file given,
# a file argument that is not an absolute path (an option placed after a
# file is one, below), no file of those named present, a path that is not
# a readable file, a NUL byte, no key parser, or a --prepared-key it reads
# no single key from. Never 0 for a file it did not read.
#
# OPTIONS COME BEFORE THE FILES, AND A FILE IS AN ABSOLUTE PATH (backlog
# 26173f40; review c403f23e of car 5cc2731a). The options are read up to
# the first word that is not one, and every word after it was taken as a
# file: `<file> --prepared-key <key>` read as two more files, both
# absent, and an absent file was clean — measured, a file holding the
# prepared key bare (a shell to whoever holds it) read "clean — 0
# line(s)" with the key never judged. Every caller hands absolute paths
# (--keys-files-of prints them so), so a word after the first file that
# is not one — an option in the wrong place, its value, a relative path
# — is UNVERIFIED and named, before any file is read. And a set in which
# every file is absent read no line at all: that is UNVERIFIED too,
# never "clean — 0 line(s) in (none present)".
set +x
set -uo pipefail
export LC_ALL=C

ME=deposit-key-audit
ROOT_DOORS='/usr/local/libexec/boss/'
# What `restrict` closes and a later option may re-open (sshd(8),
# AUTHORIZED_KEYS FILE FORMAT).
REOPENS='pty port-forwarding agent-forwarding x11-forwarding user-rc'
# What the account's shell reads from its environment before a forced
# command runs, or reads to find what it runs (ACCEPTENV above): a file
# — with bash, a command substitution — run first (BASH_ENV, ENV);
# xtrace and the prompt it expands (SHELLOPTS, BASHOPTS, PS4); a
# function run in place of the receiver line's `exec` or `sudo`
# (BASH_FUNC_<name>%%); where `sudo` is looked up (PATH); what the
# loader maps into the shell (LD_*).
SHELL_READS=(BASH_ENV ENV SHELLOPTS BASHOPTS PS4 'BASH_FUNC_exec%%' 'BASH_FUNC_sudo%%' PATH LD_PRELOAD LD_LIBRARY_PATH LD_AUDIT)

# sshd_glob <pattern> -> the bash pattern that matches what sshd's
# match_pattern matches: `*` and `?` are wildcards and every other byte
# is itself, so `[B]ASH_ENV` is a literal name to sshd and stays one here.
sshd_glob() {
    local p="$1" g="" i
    for ((i = 0; i < ${#p}; i++)); do
        case "${p:i:1}" in
            '*' | '?') g+="${p:i:1}" ;;
            *) g+="\\${p:i:1}" ;;
        esac
    done
    printf '%s' "$g"
}

# --keys-files-of <account> (WHICH FILES and ACCEPTENV above). stdout is
# the audit's arguments — `--accept-env`, the patterns, the files — or
# one UNVERIFIED line and exit 2, so the converge carries either whole.
if [ "${1:-}" = --keys-files-of ]; then
    unverified() {
        echo "$ME: UNVERIFIED — $1, so the files sshd reads the deposit key from cannot be named and none is audited"
        exit 2
    }
    [ "$#" -eq 2 ] || unverified "usage: --keys-files-of <account>"
    acct="$2"
    # A comma would end the -C spec; no account name carries one.
    case "$acct" in '' | -* | *[!A-Za-z0-9._-]*) unverified "'$acct' is not an account name" ;; esac
    pw="$(getent passwd "$acct")" || unverified "$acct has no passwd entry"
    IFS=: read -r _ _ uid _ _ home _ <<< "$pw"
    SSHD="${BOSS_DEPOSIT_AUDIT_SSHD:-$(command -v sshd || echo /usr/sbin/sshd)}"
    cfg="$("$SSHD" -T -C "user=$acct" 2>&1)" \
        || unverified "$SSHD -T -C user=$acct refused: $(printf '%s' "${cfg##*$'\n'}" | tr -d '\r' | tr -c '[:print:]' '?' | cut -c1-200)"
    pats="" seen=0 accept=()
    while IFS= read -r l; do
        case "$l" in
            "authorizedkeysfile "*) pats="${l#authorizedkeysfile }" seen=1 ;;
            # One line per pattern or all on one: either spelling reads
            # alike (ACCEPTENV above).
            "acceptenv "*)
                read -r -a ae <<< "${l#acceptenv }"
                accept+=("${ae[@]}")
                ;;
        esac
    done <<< "$cfg"
    [ "$seen" = 1 ] || unverified "$SSHD -T -C user=$acct names no authorizedkeysfile"
    [ "$pats" != none ] || unverified "sshd reads no AuthorizedKeysFile for $acct (none)"
    # expand_authorized_keys: percent_expand with h, u, U, then a path
    # that is not absolute is taken under the home. A token sshd's
    # percent_expand refuses, it refuses here.
    out=()
    read -r -a pat_list <<< "$pats"
    for p in "${pat_list[@]}"; do
        e=""
        for ((i = 0; i < ${#p}; i++)); do
            c="${p:i:1}"
            if [ "$c" != % ]; then
                e+="$c"
                continue
            fi
            i=$((i + 1))
            case "${p:i:1}" in
                %) e+=% ;;
                h) e+="$home" ;;
                u) e+="$acct" ;;
                U) e+="$uid" ;;
                *) unverified "the AuthorizedKeysFile pattern '$(printf '%s' "$p" | tr -c '[:print:]' '?')' carries a token sshd does not expand" ;;
            esac
        done
        [ "${e:0:1}" = / ] || e="$home/$e"
        out+=("$e")
    done
    [ "${#out[@]}" -gt 0 ] || unverified "sshd's authorizedkeysfile for $acct is empty"
    # The patterns ride AHEAD of the files as the audit's own option, so
    # the converge hands the list on whole: an empty line is "sshd admits
    # none", which the audit can say; no option at all is "not read".
    printf '%s\n' --accept-env "${accept[*]}" "${out[@]}"
    exit 0
fi

[ "$#" -ge 1 ] || { echo "$ME: UNVERIFIED — no authorized_keys file was named; usage: $(basename "$0") [--prepared-key <public key>]... [--accept-env <patterns>] <file>..." >&2; exit 2; }

# sshd's key parser. Overridable for the tests alone, which stand a
# parser in for one that disagrees with the option split.
SSH_KEYGEN="${BOSS_DEPOSIT_AUDIT_SSH_KEYGEN:-ssh-keygen}"
command -v "$SSH_KEYGEN" >/dev/null 2>&1 \
    || { echo "$ME: UNVERIFIED — $SSH_KEYGEN, the key parser sshd uses and this audit reads every key with, is not on this host, so nothing about the keys any file grants can be said" >&2; exit 2; }

# A CRLF file's blank line is only bytes sshd reads no key from.
SKIPPED=$'\r\v\f'
# The one shape a receiver line's forced command may have (N1 above).
RECEIVER_RE='^(exec )?(sudo -n )?'"$ROOT_DOORS"'[A-Za-z0-9_][A-Za-z0-9._/-]*( [A-Za-z0-9._-]+)*$'

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

# --prepared-key <public key>... (A PREPARED KEY above) -> PREPARED, the
# fingerprints, space-delimited; NPREPARED, how many distinct. One key
# per value: a value of two lines would be read as its first key alone.
# PREPARED_VALUES keeps each value whole, so a prepared LINE is bound to
# its receiver once the line parser below is defined (A PREPARED KEY IS
# BOUND TO ITS OWN RECEIVER above).
PREPARED=" " NPREPARED=0 PREPARED_VALUES=()
# --accept-env <patterns> (ACCEPTENV above): sshd's AcceptEnv for the
# account, space-separated, as --keys-files-of prints it. ACCEPT_READ
# says whether it was given at all; an empty value is a read of none.
ACCEPT_ENV=() ACCEPT_READ=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --accept-env)
            [ "$#" -ge 2 ] || { echo "$ME: UNVERIFIED — --accept-env names no patterns (an empty argument is sshd admitting none)" >&2; exit 2; }
            read -r -a ae <<< "$2"
            ACCEPT_ENV+=("${ae[@]}")
            ACCEPT_READ=1
            shift 2
            ;;
        --prepared-key)
            [ "$#" -ge 2 ] || { echo "$ME: UNVERIFIED — --prepared-key names no key" >&2; exit 2; }
            [[ $2 != *$'\n'* ]] && keyfp "$2" \
                || { echo "$ME: UNVERIFIED — sshd's key parser (ssh-keygen) reads no single key from a --prepared-key value, so the lines carrying the keys this audit was told to judge cannot be found" >&2; exit 2; }
            if [[ $PREPARED != *" $K_FP "* ]]; then
                PREPARED+="$K_FP "
                NPREPARED=$((NPREPARED + 1))
            fi
            PREPARED_VALUES+=("$2")
            shift 2
            ;;
        --*) echo "$ME: UNVERIFIED — '$(printf '%s' "$1" | tr -c '[:print:]' '?')' is not an option this audit knows; usage: $(basename "$0") [--prepared-key <public key>]... [--accept-env <patterns>] <file>..." >&2; exit 2 ;;
        *) break ;;
    esac
done
[ "$#" -ge 1 ] || { echo "$ME: UNVERIFIED — no authorized_keys file was named after the prepared keys" >&2; exit 2; }
# Every word left is a file, and a file is an absolute path (OPTIONS COME
# BEFORE THE FILES above): one that is not was read as an absent file,
# and the set as clean.
for f in "$@"; do
    case "$f" in
        /*) ;;
        -*) echo "$ME: UNVERIFIED — '$(printf '%s' "$f" | tr -c '[:print:]' '?' | cut -c1-120)' stands among the files, where it would be read as an absent file and the set called clean; options go before the first file: $(basename "$0") [--prepared-key <public key>]... [--accept-env <patterns>] <file>..." >&2; exit 2 ;;
        *) echo "$ME: UNVERIFIED — '$(printf '%s' "$f" | tr -c '[:print:]' '?' | cut -c1-120)' is not an absolute path, so it is no file sshd reads keys from and would be read as an absent one; options go before the first file: $(basename "$0") [--prepared-key <public key>]... [--accept-env <patterns>] <file>..." >&2; exit 2 ;;
    esac
done
prepared() { [[ $PREPARED == *" $1 "* ]]; }

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

# BOUND: fingerprint -> the forced command its prepared line names (A
# PREPARED KEY IS BOUND TO ITS OWN RECEIVER above), read by the same
# split every line of the files is read by, so the two commands compared
# are decoded alike.
declare -A BOUND=()
for v in "${PREPARED_VALUES[@]}"; do
    prc=0
    parse_line "${v#"${v%%[![:blank:]]*}"}" || prc=$?
    [ "$prc" -eq 0 ] \
        || { echo "$ME: UNVERIFIED — this audit's option split does not reach the key sshd's key parser reads from a --prepared-key value, so which receiver that key was prepared for cannot be said" >&2; exit 2; }
    [ -n "$P_OPTS" ] || continue
    split_opts
    [ "$HAS_CMD" = 1 ] && [ -n "$OPT_CMD" ] \
        || { echo "$ME: UNVERIFIED — the --prepared-key line for $P_TYPE $P_FP carries options and forces no command, so it binds that key to no receiver; hand the audit the key alone, or the authorized_keys_line its prepare packet recorded" >&2; exit 2; }
    if [ -n "${BOUND[$P_FP]+set}" ] && [ "${BOUND[$P_FP]}" != "$OPT_CMD" ]; then
        echo "$ME: UNVERIFIED — $P_TYPE $P_FP is given as prepared for both '$(shown "${BOUND[$P_FP]}")' and '$(shown "$OPT_CMD")', so no line carrying it can be judged against the receiver it was prepared for" >&2
        exit 2
    fi
    BOUND[$P_FP]="$OPT_CMD"
done

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
        # A prepared key on a line this audit would not read as a
        # receiver's (A PREPARED KEY above): one finding, and not also a
        # twin of a receiver line, since the chore judge files one item
        # per finding line (backlog d1b03874).
        if prepared "$P_FP" && { [ "$HAS_CMD" != 1 ] || ! names_root_door "$OPT_CMD"; }; then
            RECEIVERS=$((RECEIVERS + 1))
            pid="$P_TYPE $P_FP${P_COMMENT:+ ($(shown "$P_COMMENT"))}"
            if [ "$HAS_CMD" != 1 ]; then
                finding "$f:$n" "the key $pid is a prepared deposit key on a line with NO forced command, so whoever holds it gets this account's shell. Remove this line and place the authorized_keys_line on its enroll step whole."
            else
                finding "$f:$n" "the key $pid is a prepared deposit key forced to '$(shown "$OPT_CMD")', which is no root door under $ROOT_DOORS as the installers spell one: sshd hands a forced command to the account's shell, which runs a door spelled through quotes, escapes, variables or globs, or anything else, with whatever this line's options leave open. Remove this line and place the authorized_keys_line on its enroll step whole."
            fi
            continue
        fi
        # A prepared key behind a root door, but not the one it was
        # prepared for (A PREPARED KEY IS BOUND TO ITS OWN RECEIVER
        # above): one finding, and the line is no receiver line of that
        # key's, so the key's own line beside it is not taken for its twin.
        if [ -n "${BOUND[$P_FP]+set}" ] && [ "$OPT_CMD" != "${BOUND[$P_FP]}" ]; then
            RECEIVERS=$((RECEIVERS + 1))
            finding "$f:$n" "the key $P_TYPE $P_FP${P_COMMENT:+ ($(shown "$P_COMMENT"))} is a prepared deposit key forced to '$(shown "$OPT_CMD")', but it was prepared for '$(shown "${BOUND[$P_FP]}")': it reaches a root door it was never prepared or enrolled for. Remove this line and place the authorized_keys_line on its own enroll step whole."
            continue
        fi
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
                finding "$f:$n" "the key ${L_ID[i]} is forced to '$(shown "$OPT_CMD")', which names a root door under $ROOT_DOORS but is not that receiver alone, spelled as the installers print it (sudo takes -n and no other flag): sshd hands a forced command to the account's shell, so whatever else it says runs too. Remove this line and place the authorized_keys_line on its enroll step whole."
            elif [[ $OPT_NAMES == *" environment "* ]]; then
                finding "$f:$n" "the key ${L_ID[i]} is forced to '$(shown "$OPT_CMD")' and sets environment=, which no installer prints: with PermitUserEnvironment it sets what the account's shell runs the receiver with (BASH_ENV alone runs a file first). Remove this line and place the authorized_keys_line on its enroll step whole."
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
# No file read is no line judged (OPTIONS COME BEFORE THE FILES above).
[ -n "$READ" ] || { echo "$ME: UNVERIFIED — none of the files named is present (absent:$ABSENT), so no key line was read and nothing about what they grant can be said" >&2; exit 2; }

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

# WHAT THE CLIENT MAY SET (ACCEPTENV above). Judged only beside a line
# that reaches a root door, as every option here is: the owner's logins
# are his business. One finding per pattern that admits a name in
# SHELL_READS, its text carrying no key and no count — the chore judge
# keys a finding that names no key on its whole text (chore_file_reds.rs,
# finding_key), so a count in it would re-file the item each time a
# receiver line came or went, the defect backlog 54104a05 found in the
# converge's reading of this audit's verdict line.
ACCEPT_FOUND=0
if [ "$RECEIVERS" -gt 0 ]; then
    for pat in "${ACCEPT_ENV[@]}"; do
        g="$(sshd_glob "$pat")"
        admitted=""
        for name in "${SHELL_READS[@]}"; do
            [[ $name != $g ]] || admitted+=" $name"
        done
        [ -n "$admitted" ] || continue
        ACCEPT_FOUND=1
        finding "sshd AcceptEnv $(shown "$pat")" "sshd sets$admitted from whatever the client sends — restrict does not close it — before it hands a forced command to the account's shell, so a deposit key forced to a root door also chooses what that shell runs first (BASH_ENV alone has bash run a command of the sender's choosing). Remove $(shown "$pat") from the AcceptEnv sshd reads for this account, or narrow it to names the shell does not read."
    done
fi

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
[ "$NPREPARED" -eq 0 ] || tail_note+="; $NPREPARED prepared deposit key(s) judged by fingerprint on every line that carries one"
[ "${#BOUND[@]}" -eq 0 ] || tail_note+=", ${#BOUND[@]} of them against the receiver each was prepared for"
if [ "$ACCEPT_READ" -eq 0 ]; then
    tail_note+="; sshd's AcceptEnv was not read"
elif [ "$RECEIVERS" -gt 0 ] && [ "$ACCEPT_FOUND" -eq 0 ]; then
    shown_ae="${ACCEPT_ENV[*]}"
    tail_note+="; sshd's AcceptEnv ($(shown "${shown_ae:-none}")) admits no name the account's shell reads before a receiver"
fi
if [ "$FINDINGS" -gt 0 ]; then
    echo "$ME: $FINDINGS finding(s) in$READ — a deposit key reaches more than its receiver, or a line beside one cannot be judged; each UNCONFINED line above names the line to act on (backlog dea2236f)$tail_note"
    exit 1
fi
echo "$ME: clean — $RECEIVERS line(s) in$READ force a root door under $ROOT_DOORS, each with restrict and nothing re-opened, and no key of theirs is on another line$tail_note"
exit 0
