#!/bin/bash

# Guarded, so that config.sh's checks on BENCH_ROLE, BENCH_REPORT_SORT and
# BENCH_FRAMEWORKS actually stop a run. They each print and "return 1", but a
# sourced script's return only sets $? in its caller, so without this an entry
# point would get 0 back from the function definition at the end of this file
# and run the whole benchmark on a value it had already rejected. Every caller
# of env.sh guards it the same way; docker_benchmark.sh guards config.sh
# directly.
. ./script/config.sh || return 1

# Which half of the benchmark this machine runs; see BENCH_ROLE in config.sh.
# The drivers ask through these rather than each testing the variable.
bench_runs_servers() { [ "$BENCH_ROLE" != client ]; }
bench_runs_clients() { [ "$BENCH_ROLE" != server ]; }
# The servers are ours to stop only when we are the machine that started them.
bench_owns_servers() { [ "$BENCH_ROLE" = both ]; }

# Only a single-node run has two halves to divide the CPUs between. On a node
# that runs one of them, pinning it to half a machine nothing else is using
# would leave the other half idle, so the default there is the whole node; an
# explicit list still pins it, for a node that shares its CPUs.
split_cpus=true
if ! bench_runs_servers || ! bench_runs_clients; then
    if [ -z "${BENCH_SERVER_CPU_LIST:-}" ] && [ -z "${BENCH_CLIENT_CPU_LIST:-}" ]; then
        split_cpus=false
    fi
fi

if [ "$split_cpus" = true ] && command -v taskset >/dev/null 2>&1; then
    if [ -n "${BENCH_SERVER_CPU_LIST:-}" ] && [ -n "${BENCH_CLIENT_CPU_LIST:-}" ]; then
        # Docker supplies lists from the daemon's effective cpuset. This avoids
        # selecting host CPUs that are not available inside the container.
        server_cpu_list=$BENCH_SERVER_CPU_LIST
        client_cpu_list=$BENCH_CLIENT_CPU_LIST
    else
        total_cpu_num=$(getconf _NPROCESSORS_ONLN)
        server_cpu_num=$((total_cpu_num / 2 - 1))
        client_cpu_num=$((server_cpu_num + 1))
        server_cpu_list="0-${server_cpu_num}"
        client_cpu_list="${client_cpu_num}-$((total_cpu_num - 1))"

        if command -v lscpu >/dev/null 2>&1; then
            topology=$(lscpu -p=CPU,CORE,SOCKET,NODE | awk -F, '$1 !~ /^#/ {print $1 "," $2 "," $3 "," $4}')
            socket_count=$(printf '%s\n' "$topology" | awk -F, '$3 >= 0 {seen[$3] = 1} END {print length(seen)}')
            node_count=$(printf '%s\n' "$topology" | awk -F, '$4 >= 0 {seen[$4] = 1} END {print length(seen)}')
            server_topology_cpus=""
            client_topology_cpus=""
            server_topology_count=0
            client_topology_count=0

            append_cpu_group() {
                target=$1
                cpus=$2
                count=$(printf '%s\n' "$cpus" | awk -F, '{print NF}')
                if [ "$target" = server ]; then
                    server_topology_cpus="${server_topology_cpus}${server_topology_cpus:+,}${cpus}"
                    server_topology_count=$((server_topology_count + count))
                else
                    client_topology_cpus="${client_topology_cpus}${client_topology_cpus:+,}${cpus}"
                    client_topology_count=$((client_topology_count + count))
                fi
            }

            if [ "$socket_count" -ge 2 ]; then
                mapfile -t groups < <(printf '%s\n' "$topology" | awk -F, '$3 >= 0 {print $3}' | sort -n -u)
                group_column=3
            elif [ "$node_count" -ge 2 ]; then
                mapfile -t groups < <(printf '%s\n' "$topology" | awk -F, '$4 >= 0 {print $4}' | sort -n -u)
                group_column=4
            else
                mapfile -t groups < <(printf '%s\n' "$topology" | awk -F, '{print $3 ":" $2}' | sort -t: -k1,1n -k2,2n -u)
                group_column=core
            fi

            for group in "${groups[@]}"; do
                if [ "$group_column" = core ]; then
                    socket=${group%%:*}
                    core=${group#*:}
                    group_cpus=$(printf '%s\n' "$topology" | awk -F, -v socket="$socket" -v core="$core" '$3 == socket && $2 == core {print $1}' | paste -sd, -)
                else
                    group_cpus=$(printf '%s\n' "$topology" | awk -F, -v column="$group_column" -v group="$group" '$column == group {print $1}' | paste -sd, -)
                fi
                if [ "$server_topology_count" -le "$client_topology_count" ]; then
                    append_cpu_group server "$group_cpus"
                else
                    append_cpu_group client "$group_cpus"
                fi
            done

            if [ -n "$server_topology_cpus" ] && [ -n "$client_topology_cpus" ]; then
                server_cpu_list=$server_topology_cpus
                client_cpu_list=$client_topology_cpus
            fi
        fi
    fi

    limit_cpu_server="taskset -c ${server_cpu_list}"
    limit_cpu_client="taskset -c ${client_cpu_list}"
fi

# debug
# echo "limit_cpu_server: ${server_cpu_list}, ${limit_cpu_server}"
# echo "limit_cpu_client: ${client_cpu_list}, ${limit_cpu_client}"

line=$(printf "%0.s-" {1..62})

clean() {
    rm -rf ./output
    for f in ${frameworks[@]}; do
        killall -9 "${f}.server" 1>/dev/null 2>&1
    done
}

# Where script/server.sh sends a framework's server log and records its pid.
server_log_file() { echo "./output/log/${1}.log"; }
server_pid_file() { echo "./output/run/${1}.server.pid"; }

# start_server <framework> [server flags]: starts the server and returns once
# it has bound every benchmark port, which each server logs as
# "server: listening on" - its control port comes up before them, so it is
# no sign of readiness. Fails if the server exits first or takes longer than
# ServerStartTimeout. A server that exits because one of its ports is in use
# is started again, up to ServerStartRetries times, ServerStartRetryDelay
# seconds apart: where reserve_server_ports could not reserve them, a client
# socket may still hold one for a while, such as a control connection in
# TIME_WAIT.
start_server() {
    local f=$1
    shift
    local pid log tick attempt
    log=$(server_log_file "$f")
    for ((attempt = 0; ; attempt++)); do
        ./script/server.sh "$f" "$@" || return 1
        pid=$(cat "$(server_pid_file "$f")" 2>/dev/null)
        for ((tick = 0; tick < ServerStartTimeout * 10; tick++)); do
            if grep -q "server: listening on" "$log" 2>/dev/null; then
                echo "${f} server is up, pid ${pid}"
                return 0
            fi
            if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then
                break
            fi
            sleep 0.1
        done
        if [ "$tick" -ge $((ServerStartTimeout * 10)) ]; then
            break
        fi
        echo "${f} server exited before it was listening; the end of ${log}:" >&2
        tail -n 20 "$log" >&2
        if [ "$attempt" -ge "$ServerStartRetries" ] || ! grep -qi "in use" "$log"; then
            rm -f "$(server_pid_file "$f")"
            return 1
        fi
        echo "a port is in use: start ${f} server again in ${ServerStartRetryDelay}s" >&2
        sleep "$ServerStartRetryDelay"
    done
    echo "${f} server not listening after ${ServerStartTimeout}s; the end of ${log}:" >&2
    tail -n 20 "$log" >&2
    stop_server "$f"
    return 1
}

# stop_server <framework>: SIGINT, so that the server logs its final
# statistics, then waits for it to exit, with SIGKILL after ServerStopTimeout.
# By the pid script/server.sh recorded rather than by name, which would also
# reach a sibling benchmark's server of the same name on this machine.
stop_server() {
    local f=$1 pid_file pid tick
    pid_file=$(server_pid_file "$f")
    pid=$(cat "$pid_file" 2>/dev/null)
    if [ -z "$pid" ]; then
        . ./script/killone.sh "${f}.server"
        return
    fi
    echo "stop ${f} server, pid ${pid} ..."
    kill -INT "$pid" 2>/dev/null
    for ((tick = 0; tick < ServerStopTimeout * 10; tick++)); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.1
    done
    if kill -0 "$pid" 2>/dev/null; then
        echo "${f} server still running after ${ServerStopTimeout}s, SIGKILL"
        kill -9 "$pid" 2>/dev/null
        for ((tick = 0; tick < 50; tick++)); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
    fi
    rm -f "$pid_file"
    echo "stop ${f} server done"
}

# ports_covered <list> <first> <last>: whether a list in the kernel's
# ip_local_reserved_ports format, such as "80,3001-3200,3201-3351", covers
# every port from first to last.
ports_covered() {
    printf '%s\n' "$1" | tr ',' '\n' \
        | awk -F- 'NF { print $1, (NF > 1 ? $2 : $1) }' | sort -n \
        | awk -v need="$2" -v last="$3" '
            $1 <= need && $2 >= need { need = $2 + 1 }
            END { exit !(need > last) }'
}

# Keeps the kernel from giving a server port to any client socket; see
# ReservedPorts in script/config.sh. It never stops a run: where it cannot
# reserve them it says how to, and start_server still retries a server whose
# port was taken.
reserve_server_ports() {
    local first=${ReservedPorts%-*} last=${ReservedPorts#*-}
    case "$(uname -s)" in
        Linux)
            local file=/proc/sys/net/ipv4/ip_local_reserved_ports current
            if ! current=$(cat "$file" 2>/dev/null); then
                echo "warning: cannot read ${file}; server ports ${ReservedPorts} are not reserved" >&2
                return 0
            fi
            if ports_covered "$current" "$first" "$last"; then
                echo "server ports ${ReservedPorts} reserved: ip_local_reserved_ports=${current}"
                return 0
            fi
            # Added to what is there, which is someone else's to keep.
            if { echo "${current:+${current},}${ReservedPorts}" >"$file"; } 2>/dev/null \
                && ports_covered "$(cat "$file")" "$first" "$last"; then
                echo "server ports ${ReservedPorts} reserved: ip_local_reserved_ports=$(cat "$file")"
            else
                echo "warning: server ports ${ReservedPorts} are not reserved, so a client socket may hold one;" >&2
                echo "  as root: sysctl -w net.ipv4.ip_local_reserved_ports=${current:+${current},}${ReservedPorts}" >&2
            fi
            ;;
        Darwin)
            local ephemeral_first high_first
            ephemeral_first=$(sysctl -n net.inet.ip.portrange.first 2>/dev/null || echo 0)
            high_first=$(sysctl -n net.inet.ip.portrange.hifirst 2>/dev/null || echo 0)
            if [ "$ephemeral_first" -gt "$last" ] && [ "$high_first" -gt "$last" ]; then
                echo "server ports ${ReservedPorts} are below the ephemeral ports, ${ephemeral_first} and up"
            else
                echo "warning: the ephemeral ports start at ${ephemeral_first} (high: ${high_first}), so a client socket may hold a server port in ${ReservedPorts};" >&2
                echo "  sudo sysctl -w net.inet.ip.portrange.first=49152 net.inet.ip.portrange.hifirst=49152" >&2
            fi
            ;;
    esac
}

# The pause between two client runs.
bench_sleep() {
    local s
    for ((s = 1; s <= SleepTime; s++)); do
        echo "sleep $s ..."
        sleep 1
    done
}

print_env() {
    echo "os:"
    echo
    if [ -r /etc/issue ]; then cat /etc/issue; else uname -srm; fi
    echo $line
    echo "cpu model:"
    echo
    if [ -r /proc/cpuinfo ]; then
        grep "model name" /proc/cpuinfo | uniq
    else
        sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown
    fi
    echo $line
    echo "processors: $(getconf _NPROCESSORS_ONLN)"
    echo $line
    if command -v free >/dev/null 2>&1; then free; else echo "memory: $(sysctl -n hw.memsize 2>/dev/null || echo unknown) bytes"; fi
    echo $line
    echo "server cpus: ${server_cpu_list:-unbound}"
    echo "client cpus: ${client_cpu_list:-unbound}"
    echo $line
    echo "role: ${BENCH_ROLE} (servers: $(bench_runs_servers && echo here || echo elsewhere), clients: $(bench_runs_clients && echo here || echo elsewhere))"
    echo "server host: ${BENCH_SERVER_HOST}"
    echo "client: benchcli-${BENCH_CLIENT}"
    echo $line
    echo "frameworks: ${frameworks[*]}"
    echo $line
    echo "report sort: ${BENCH_REPORT_SORT} (result = best first, framework = config.FrameworkList order)"
    echo $line
    echo "go env:"
    echo
    go env
    echo $line
    echo "rust:"
    echo
    rustc --version 2>/dev/null || echo "rustc: not found"
    cargo --version 2>/dev/null || echo "cargo: not found"
}
