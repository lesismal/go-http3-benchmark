#!/bin/bash

# A million QUIC connections per framework, each on a UDP socket of its own
# on the client side. Needs the system settings in the README's "before
# running the test", on both nodes of a two-node run.

. ./script/env.sh || { return 1 2>/dev/null || exit 1; }

echo $line

. ./script/killall.sh

echo $line

. ./script/clean.sh

echo $line

# The subset this script measures, in framework-name order like every other
# framework list; see script/config.sh. BENCH_FRAMEWORKS narrows it the same
# way it narrows the full list.
if [ -z "${BENCH_FRAMEWORKS:-}" ]; then
    frameworks=(
        "fib"
        "gin"
        "quicgo"
        "quiche"
    )
fi

print_env

echo $line

. ./script/build.sh || { return 1 2>/dev/null || exit 1; }

echo $line

# The servers and the benchmark client take different flags, and this script
# takes the client's. Forward only what a server actually defines.
server_flags=""
for arg in "$@"; do
    case "$arg" in
        -b=*|-m=*|-streams=*|-idle=*) server_flags="${server_flags} ${arg}" ;;
    esac
done

# Before any server binds its ports, and before any client could take one.
if bench_runs_servers; then
    reserve_server_ports
    echo $line
fi

# A server node starts every server and leaves them up for the client node. A
# single-node run starts each one only for its own turn, in script/clients.sh.
if ! bench_runs_clients; then
    . ./script/servers.sh || { return 1 2>/dev/null || exit 1; }

    echo $line
    echo "servers are up and left running. On the client node:"
    echo "  BENCH_ROLE=client BENCH_SERVER_HOST=<this host> bash script/1m_conns_benchmark.sh"
    echo "Stop them here afterwards with: bash script/killall.sh"
    echo $line
    return 0 2>/dev/null || exit 0
fi

. ./script/clients.sh -c=1000000 -en=2000000 -b=1024 -rr=1 -preffix=1m_connections_ "$@" || { return 1 2>/dev/null || exit 1; }

# The report step reads BENCH_REPORT_SORT for the row order of its tables; see
# script/config.sh.
. ./script/report.sh -preffix=1m_connections_ "$@"

echo $line
