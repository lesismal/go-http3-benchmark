#!/bin/bash

. ./script/env.sh

framework=$1
shift

echo "run ${framework} server on cpu ${server_cpu_list:-unbound}"
mkdir -p ./output/log ./output/run
# "$@" rather than $2 through $9, however many flags the driver forwards.
nohup $limit_cpu_server "./output/bin/${framework}.server" "$@" \
    >"$(server_log_file "$framework")" 2>&1 &
# nohup and taskset both exec what they run, so this is the server's own pid,
# which stop_server in script/env.sh stops it by.
echo $! >"$(server_pid_file "$framework")"
