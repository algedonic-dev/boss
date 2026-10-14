#!/bin/bash
# Prepare only the durable private child; never repair existing evidence.
set -euo pipefail
refuse() { echo "conductor-diagnostics: refused: $*" >&2; exit 78; }
[[ $# == 1 ]] || refuse 'expected the mounted conductor home'
conductor_home=$1
[[ -d $conductor_home && ! -L $conductor_home ]] || refuse 'home is not a real mounted directory'
directory=$conductor_home/diagnostics
[[ ! -L $directory ]] || refuse 'journal child is a symlink'
if [[ ! -e $directory ]]; then
    # mode-bits-ok: newly owned durable directory, never an executable file.
    mkdir --mode=0700 -- "$directory" || refuse 'private child creation failed'
fi
[[ -d $directory && ! -L $directory ]] || refuse 'journal child is not a real directory'
owner=$(stat -c %u -- "$directory") || refuse 'child owner unreadable'
uid=$(id -u) || refuse 'running actor unreadable'
[[ $owner == "$uid" ]] || refuse 'journal child belongs to another actor'
mode=$(stat -c %a -- "$directory") || refuse 'child mode unreadable'
case $mode in
    700|2700) ;; # Inherited setgid gives no other actor access.
    *) refuse 'journal child is not private' ;;
esac
echo 'conductor-diagnostics: private durable child verified'
