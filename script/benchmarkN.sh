#!/bin/bash

# Every framework through the Connections x BodySize x BenchTime matrix in
# script/config.sh, one report per combination, each named by its suffix:
# output/report/BenchEcho_<conns>_<payload>_<times>.md and so on.

. ./script/env.sh || { return 1 2>/dev/null || exit 1; }

echo $line
. ./script/clean.sh

echo $line

print_env

echo $line

. ./script/build.sh || { return 1 2>/dev/null || exit 1; }

echo $line

. ./script/killall.sh
sleep 1
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
# single-node run starts each one only for its own turn, below.
if ! bench_runs_clients; then
    . ./script/servers.sh || { return 1 2>/dev/null || exit 1; }
    echo $line
    echo "servers are up and left running. On the client node:"
    echo "  BENCH_ROLE=client BENCH_SERVER_HOST=<this host> bash script/benchmarkN.sh"
    echo "Stop them here afterwards with: bash script/killall.sh"
    return 0 2>/dev/null || exit 0
fi
echo $line

# One server per framework, up for all of that framework's combinations: it
# is started just before the first, given ServerReadyDelay once it is
# listening, and stopped - and waited for - after the last. SleepTime
# separates two client runs, and nothing sleeps after the last one.
# A copy: script/client.sh sources env.sh, which sets frameworks afresh.
matrix_frameworks=("${frameworks[@]}")
matrix_first=true
for f in "${matrix_frameworks[@]}"; do
    if [ "$matrix_first" = true ]; then
        matrix_first=false
    else
        bench_sleep
    fi
    if bench_owns_servers; then
        start_server "$f" $server_flags || { return 1 2>/dev/null || exit 1; }
        sleep "$ServerReadyDelay"
    fi
    combo_first=true
    for c in ${Connections[@]}; do
        for b in ${BodySize[@]}; do
            for n in ${BenchTime[@]}; do
                if [ "$combo_first" = true ]; then
                    combo_first=false
                else
                    bench_sleep
                fi
                suffix="_${c}_${b}_${n}"
                echo "run client to ${f} at ${BENCH_SERVER_HOST}: ${c} connections, ${b} payload, ${n} times"
                if ! . ./script/client.sh -f=$f -ip=${BENCH_SERVER_HOST} -c=$c -b=$b -en=$n -suffix=${suffix} -rate=true "$@"; then
                    bench_owns_servers && stop_server "$f"
                    return 1 2>/dev/null || exit 1
                fi
            done
        done
    done
    if bench_owns_servers; then
        stop_server "$f"
    fi
done

# The report step reads BENCH_REPORT_SORT for the row order of its tables; see
# script/config.sh.
for c in ${Connections[@]}; do
    for b in ${BodySize[@]}; do
        for n in ${BenchTime[@]}; do
            suffix="_${c}_${b}_${n}"
            . ./script/report.sh -suffix=${suffix} "$@"
        done
    done
done
