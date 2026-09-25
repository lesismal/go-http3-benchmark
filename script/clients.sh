#!/bin/bash

# . ./script/env.sh

# Runs the client against every framework in turn. On a single node each
# server is started just before its client, given ServerReadyDelay once it is
# listening, and stopped - and waited for - as soon as its client is done, so
# that only the server being measured is ever running. SleepTime separates two
# frameworks, and nothing sleeps after the last.
#
# The servers' flags come from server_flags, set by the driver that sources
# this; see script/servers.sh.

clients_status=0

if ! bench_owns_servers; then
    echo "servers are on ${BENCH_SERVER_HOST} and stay up for the whole run;"
    echo "stop them there with script/killall.sh when it is done"
fi

# A copy: script/client.sh sources env.sh, which sets frameworks afresh.
clients_frameworks=("${frameworks[@]}")
clients_first=true
for f in "${clients_frameworks[@]}"; do
    if [ "$clients_first" = true ]; then
        clients_first=false
    else
        bench_sleep
    fi
    echo
    # Only the machine that started a server can stop it, and only it should.
    if bench_owns_servers; then
        if ! start_server "$f" ${server_flags:-}; then
            clients_status=1
            continue
        fi
        sleep "$ServerReadyDelay"
    fi
    # echo "start bench ${f}" "$@"
    echo "run client to ${f} at ${BENCH_SERVER_HOST}, on cpu ${client_cpu_list:-unbound}"
    # -ip first, so a host given on the command line still wins: both clients
    # take the last value of a repeated flag.
    . ./script/client.sh -f=$f -ip=${BENCH_SERVER_HOST} "$@" || clients_status=$?
    if bench_owns_servers; then
        stop_server "$f"
    fi
done

return "$clients_status" 2>/dev/null || exit "$clients_status"
