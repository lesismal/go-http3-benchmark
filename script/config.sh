#!/bin/bash

# Where the servers are, as the clients should reach them: an address or a
# hostname, IPv6 included. The default keeps a single-node run on loopback.
# The servers always bind every interface, so a two-node run configures only
# this side.
# Override for one run with: BENCH_SERVER_HOST=10.0.0.2 bash script/benchmark.sh
BENCH_SERVER_HOST=${BENCH_SERVER_HOST:-127.0.0.1}

# Which half of the benchmark this machine runs:
#
#   both    (default) build everything, start the servers, run the clients
#           against them and write the report - one machine
#   server  build and start the servers, then leave them running. Nothing is
#           measured here; the client node does that
#   client  build the client only, run it against BENCH_SERVER_HOST and write
#           the report. Nothing is started or stopped here
#
# A two-node run is BENCH_ROLE=server on one machine and, once it reports the
# servers are up, BENCH_ROLE=client BENCH_SERVER_HOST=<that machine> on the
# other. "client" against a loopback host is also the way to run the clients
# again without restarting servers that are already up on this machine.
#
# Two things differ from a single-node run. The client cannot stop a server it
# did not start, so every framework's server stays up for the whole run rather
# than being killed after its turn: stop them on the server node afterwards
# with script/killall.sh, and use BENCH_FRAMEWORKS below if the idle ones
# holding memory would disturb the framework being measured. And each node
# gives the whole machine to its own half, since there is no longer anything
# to divide it with; BENCH_SERVER_CPU_LIST and BENCH_CLIENT_CPU_LIST still
# pin it where a node shares its CPUs with something else.
BENCH_ROLE=${BENCH_ROLE:-both}
case "$BENCH_ROLE" in
    both|server|client) ;;
    *) echo "Unsupported BENCH_ROLE: $BENCH_ROLE (want both, server or client)" >&2; return 1 ;;
esac

# Which benchmark client measures the run: the one in benchcli-<name>, built to
# output/bin/bench.client. A client directory with a Cargo.toml is built with
# cargo, as package benchcli-<name>; any other with go build.
#
#   rust  (default) benchcli-rust, on cloudflare/quiche
#
# Override for one run with: BENCH_CLIENT=rust bash script/benchmark.sh
BENCH_CLIENT=${BENCH_CLIENT:-rust}
if [ ! -d "./benchcli-${BENCH_CLIENT}" ]; then
    echo "Unsupported BENCH_CLIENT: $BENCH_CLIENT (want one of:$(for d in ./benchcli-*/; do d=${d%/}; printf ' %s' "${d#./benchcli-}"; done))" >&2
    return 1
fi

# The order the report tables put their rows in. Both orders carry the same
# rows and the same numbers; only the order differs:
#
#   result     (default) best first, ranked by the number each benchmark
#              answers with: TPS in all three - for BenchMultiplex the
#              responses the client read back off the server per second -
#              with CPU EER breaking a tie in BenchEcho and BenchMultiplex,
#              and MEM EER a tie on both (Connections samples no CPU or
#              memory, so it has neither). The rate test
#              opens request streams at a rate the client sets rather than to
#              completion, so what came back under that load is its result
#              there the way TPS is in the other two; Req Sent is the load
#              rather than the answer
#   framework  the order FrameworkList in config/config.go lists them in,
#              which is by framework name. It is what puts a framework on the
#              same row in every table and across runs, whatever it scored, so
#              two reports can be diffed. Only the Go list reaches a report;
#              the frameworks array below decides what is built and run, and is
#              kept in the same order so that the two read alike
#
# In either order every ranked column - TPS, CPU EER and MEM EER - shows each
# row's share of the best in that column after it, the best being 100%, and
# carries [↓1], [↓2] or [↓3] after its title for which key it is. Rows that
# tie keep the framework order between them, so two frameworks that scored the
# same - or a whole table from a benchmark that did not run, which leaves every
# row at zero - come out the same way on every run.
#
# Override for one run with: BENCH_REPORT_SORT=framework bash script/benchmark.sh
# or, without re-running the benchmark, by passing the client flag straight to
# the report step: bash script/report.sh -sort=framework
BENCH_REPORT_SORT=${BENCH_REPORT_SORT:-result}
case "$BENCH_REPORT_SORT" in
    result|framework) ;;
    *) echo "Unsupported BENCH_REPORT_SORT: $BENCH_REPORT_SORT (want result or framework)" >&2; return 1 ;;
esac

# The matrix script/benchmarkN.sh runs every framework through.
Connections=(5000 50000)
BodySize=(512 1024)
BenchTime=(2000000)
# Seconds of quiet between two client runs, after one framework's server has
# exited and before the next one starts. Nothing sleeps after the last run.
SleepTime=5

# A single-node run starts each framework's server just before its client and
# stops it right after, so that only the server being measured is running.
#
#   ServerStartTimeout     seconds a server has to log that it is listening
#   ServerStartRetries     restarts of a server that exited with a port in use
#   ServerStartRetryDelay  seconds before each of those restarts
#   ServerReadyDelay       seconds between listening and starting the client
#   ServerStopTimeout      seconds a server has to exit on SIGINT before SIGKILL
ServerStartTimeout=30
ServerStartRetries=3
ServerStartRetryDelay=3
ServerReadyDelay=1
ServerStopTimeout=10

# Every server port, benchmark and control alike: fib 3001-3051, gin
# 3101-3151, quicgo 3201-3251 and quiche 3301-3351, as config.Ports lays them
# out (config.TestPortsMatch holds this to config.ReservedPorts). Before
# starting servers the drivers reserve it from the kernel's ephemeral ports -
# net.ipv4.ip_local_reserved_ports on Linux, which needs root or, in Docker,
# the --sysctl script/docker_benchmark.sh passes - so that no client socket,
# live or in TIME_WAIT, can hold a port a server is about to bind. On macOS
# they are already below the ephemeral range, which starts at 49152.
ReservedPorts="3001-3351"

# Which frameworks a run measures, and the order the servers are started and
# the clients run in. In framework-name order, like config.FrameworkList, so
# that a framework is in the same place in every list and a new one has one
# obvious place to go.
#
#   fib     github.com/lesismal/fib, its HTTP/3 server (fib/http3), Go
#   gin     github.com/gin-gonic/gin, served by quic-go/http3 as its RunQUIC
#           does, Go
#   quicgo  github.com/quic-go/quic-go/http3, Go
#   quiche  github.com/cloudflare/quiche, its quiche::h3 over mio, Rust
#
# A framework with a Cargo.toml in frameworks/<name> is a Rust server and is
# built with cargo; the others are Go packages built with go build.
frameworks=(
    "fib"
    "gin"
    "quicgo"
    "quiche"
)

# Optional comma-separated subset, used by the Docker smoke test and useful for
# focused local runs. Reject unknown names before they reach build paths.
if [ -n "${BENCH_FRAMEWORKS:-}" ]; then
    all_frameworks=("${frameworks[@]}")
    IFS=',' read -r -a requested_frameworks <<< "$BENCH_FRAMEWORKS"
    frameworks=()
    for requested_framework in "${requested_frameworks[@]}"; do
        framework_found=false
        for available_framework in "${all_frameworks[@]}"; do
            if [ "$requested_framework" = "$available_framework" ]; then
                framework_found=true
                break
            fi
        done
        if [ "$framework_found" != true ]; then
            echo "Unsupported framework in BENCH_FRAMEWORKS: $requested_framework" >&2
            return 1
        fi
        frameworks+=("$requested_framework")
    done
    if [ "${#frameworks[@]}" -eq 0 ]; then
        echo "BENCH_FRAMEWORKS must select at least one framework" >&2
        return 1
    fi
fi
