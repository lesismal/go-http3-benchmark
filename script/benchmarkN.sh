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

if bench_runs_servers; then
    . ./script/servers.sh
    sleep 3
fi
echo $line

if ! bench_runs_clients; then
    echo "servers are up and left running. On the client node:"
    echo "  BENCH_ROLE=client BENCH_SERVER_HOST=<this host> bash script/benchmarkN.sh"
    echo "Stop them here afterwards with: bash script/killall.sh"
    return 0 2>/dev/null || exit 0
fi

for f in ${frameworks[@]}; do
    for c in ${Connections[@]}; do
        for b in ${BodySize[@]}; do
            for n in ${BenchTime[@]}; do
                suffix="_${c}_${b}_${n}"
                echo "run client to ${f} at ${BENCH_SERVER_HOST}: ${c} connections, ${b} payload, ${n} times"
                . ./script/client.sh -f=$f -ip=${BENCH_SERVER_HOST} -c=$c -b=$b -en=$n -suffix=${suffix} -rate=true "$@" || { return 1 2>/dev/null || exit 1; }
                sleep $SleepTime
            done
        done
    done
    if bench_owns_servers; then
        . ./script/killone.sh "${f}.server"
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
