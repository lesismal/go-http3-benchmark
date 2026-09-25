#!/bin/bash

# Starts every framework's server and leaves them running, for a server node
# (BENCH_ROLE=server), whose client on another machine cannot start or stop
# them. A single-node run does not use this: script/clients.sh starts each
# server just before its client and stops it right after.

# . ./script/env.sh
# . ./script/config.sh

# Flags for every server, set by the driver that sources this: the ones the
# servers define, such as -streams, with the benchmark client's own filtered
# out. Read from a variable rather than from "$@" because `source file` with no
# arguments leaves the caller's positional parameters in place, which is how
# the client's flags would otherwise reach the servers.
if [ -z "${server_flags+set}" ]; then
    # Invoked directly rather than sourced: take our own arguments.
    . ./script/env.sh || { return 1 2>/dev/null || exit 1; }
    server_flags="$*"
fi

servers_status=0
for f in ${frameworks[@]}; do
    echo
    start_server "$f" $server_flags || servers_status=1
done

return "$servers_status" 2>/dev/null || exit "$servers_status"
