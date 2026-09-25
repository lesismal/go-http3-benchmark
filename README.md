# go-http3-benchmark

HTTP/3 server benchmark, built the same way as
[go-http1-benchmark](https://github.com/lesismal/go-http1-benchmark): the same
scripts, the same three-phase client, and the same report format. The client
is written in Rust on [cloudflare/quiche](https://github.com/cloudflare/quiche).

| Framework | Package | Server |
| --- | --- | --- |
| `fib` | [github.com/lesismal/fib](https://github.com/lesismal/fib) | one fib UDP engine bound to every port, HTTP/3 handler from `fib/http3` |
| `gin` | [github.com/gin-gonic/gin](https://github.com/gin-gonic/gin) | a `gin.Engine` (`gin.New`, no middleware) on the same `http3.Server`s as `quicgo`, the server gin's own `RunQUIC` starts |
| `quicgo` | [github.com/quic-go/quic-go/http3](https://github.com/quic-go/quic-go) | one `http3.Server` per port, all sharing one `ServeMux` |
| `quiche` | [github.com/cloudflare/quiche](https://github.com/cloudflare/quiche) (Rust) | `quiche::h3` over mio, one thread per CPU, each owning an equal share of the ports and every connection on them |

Every server answers `POST /echo` with the request body, byte for byte, with a
`Content-Length`. Each one listens on 50 UDP ports (see `config.Ports`), and
the client gives every connection a UDP socket of its own, so that a million
connections from one address do not run out of ephemeral ports toward any one
port. `/init`, `/ps` and, for the Go servers, `/debug/pprof/` are on a
separate TCP control server on the port after the last benchmark port. That
way the framework being measured serves nothing but `/echo`.

Only frameworks that serve HTTP/3 themselves are here. chi, httprouter,
beego, echo, gorilla/mux and goji have no HTTP/3 or QUIC code, so they are
left out. gin is included because of `RunQUIC`.

All the servers get the same transport settings from the same flags:
`-streams` concurrent request streams per connection (100) and an `-idle`
timeout (120s). Each makes a self-signed ECDSA P-256 certificate when it
starts, and the client does not verify it.

The `quiche` server is quiche's protocol code with the smallest event loop
that serves many connections on many cores: quiche does no I/O and runs no
threads of its own, so it is measured with the loop in
[`frameworks/quiche`](frameworks/quiche/src/server.rs) rather than a
production one. It has no pprof routes; `-ep` and `-rp` skip it.

`-ep` and `-rp`, which fetch a CPU and a heap profile from a Go server while
BenchEcho or BenchMultiplex runs, are off by default. A CPU profile costs the
server it is taken from a share of its throughput, and only the Go servers can
be profiled, so turning it on handicaps them against `quiche`. Turn it on to
look into a server, not to compare servers. The Summary table says whether a
run had them on, as `Echo Pprof` and `Rate Pprof`.

## What is measured

The client, [`benchcli-rust`](benchcli-rust), written in Rust on quiche, is
what every run uses by default. It runs three benchmarks one after another on
the same QUIC connections:

| Benchmark | What it does | TPS is |
| --- | --- | --- |
| `Connections` | dials `-c` QUIC connections, `-dc` handshakes at a time, and sends one `GET /echo` on each | connections established and answered per second |
| `BenchEcho` | `-en` request/response round trips: a `POST /echo` with `-b` random bytes, then the response is read back, with one request in flight per connection and `-ec` connections busy at once | round trips per second |
| `BenchMultiplex` | HTTP/3 multiplexing for `-rd` seconds: each connection gets `-rr` requests a second, `-rb` request streams opened together (or, with `-rb=0`, as many as fit in `-rbs` bytes) without waiting for the responses to earlier ones | responses read back per second |

`Connections` sends a request on each connection because a finished
handshake is not yet a connection the server's HTTP/3 layer is serving. That
GET is the equivalent of the WebSocket upgrade.

`BenchMultiplex` is `BenchPipeline` of the HTTP/1 benchmark in HTTP/3's terms:
where HTTP/1.1 writes a batch of requests back to back on one connection,
HTTP/3 opens a batch of streams at once. It limits each connection to four
unanswered batches, and to the streams the server's MAX_STREAMS leaves it.
When the server falls behind, the client skips that connection for a tick
instead of queueing more requests, so a slow server is measured by what it
answered, not by how deep a backlog the client built. When the duration is up,
the client waits up to one more tick for the last batch, then counts. `-rb`
has to divide `-rr`, so that the batch goes out a whole number of times a
second. The default, `-rb=0`, uses the most requests whose bodies fit in `-rbs`
bytes (16KB) and divide `-rr`, which is 10 for the default 1KB payload and 200
requests a second:

```sh
bash script/benchmark.sh -rr=200 -rb=50   # 50 streams at a time, 4 times a second
```

The client runs `-t` event-loop threads (one per CPU it may run on by
default), each driving its share of the connections with quiche and mio.
`-check=true` compares every response body with the request that was sent.
A BenchEcho request that finds its connection's flow control or congestion
window full waits on that connection, as an HTTP/1 client blocks in its write.

`EER` is throughput per percent of a CPU core: `TPS / CPU Avg`. The server's
CPU and memory are sampled every `-pi` ms. `-ps=auto` (the default) samples the
server process from the client side when it runs on the same machine, and asks
the server's `/ps` route when it does not; `local` and `remote` force one or
the other. Each benchmark reads only the samples taken while it ran. A
benchmark shorter than one sampling interval has no samples, and its CPU, MEM
and EER columns read 0; the client logs a message when that happens.

## Run

Go 1.27 or later, and a Rust toolchain with cmake and a C++ compiler for the
BoringSSL that quiche builds (libclang too, on Linux). From the repository
root:

```sh
# all frameworks, 10k connections, 1k payload
bash script/benchmark.sh

# a subset, with client flags
BENCH_FRAMEWORKS=quicgo,quiche bash script/benchmark.sh -c=10000 -en=2000000 -b=1024

# every framework through the Connections x BodySize x BenchTime matrix in script/config.sh
bash script/benchmarkN.sh

# 1m connections (needs the system settings below)
bash script/1m_conns_benchmark.sh
```

Change the defaults in [`script/config.sh`](script/config.sh). `BENCH_CLIENT`
picks the client, the one in `benchcli-<name>`; it defaults to `rust`, which is
`benchcli-rust`. The client
writes one JSON file per framework and benchmark to `output/report`, and
[`benchreport`](benchreport) turns them into `Summary.md`, `Connections.md`,
`BenchEcho.md` and `BenchMultiplex.md`. Server logs are in `output/log`.
`benchmark.sh` forwards only `-b`, `-m`, `-streams` and `-idle` to the
servers. Every flag goes to the client; run
`./output/bin/bench.client -h` for the list.

The first build compiles quiche and BoringSSL, which takes a few minutes;
`cargo` keeps them in `target/` for the builds after it.

On Linux, `script/env.sh` pins the servers and the client to separate halves
of the CPUs with `taskset`, and splits by socket, NUMA node or core when
`lscpu` can tell them apart. Without `taskset` (macOS, for example), nothing is
pinned.

The scripts stop servers and the client by process name (`fib.server`,
`bench.client` and so on), which the HTTP/1 and HTTP/2 benchmarks use as well.
Do not run two of them on one machine at once.

### Docker

The Docker runner reads the CPU set and memory exposed by the Docker daemon,
uses about 75% of its CPUs and 80% of its memory, pins separate CPU groups for
the servers and the client, and copies reports, logs, console output and the
resource plan to `output/docker/<timestamp>`. The image carries both
toolchains, and builds quiche and BoringSSL in a layer of their own.

```sh
# short quicgo-only validation
bash script/docker_benchmark.sh --smoke

# full benchmark
bash script/docker_benchmark.sh

# focused run with explicit resource limits
BENCH_FRAMEWORKS=quicgo,quiche \
DOCKER_BENCH_CPUS=8 DOCKER_BENCH_MEMORY=12g \
bash script/docker_benchmark.sh -c=10000 -en=2000000 -b=1024
```

Run `bash script/docker_benchmark.sh --help` for all overrides. From mainland
China, use `script/docker_benchmark_cn.sh` instead. It takes the same options
and builds the image from mirrors (DaoCloud for Docker Hub, Aliyun for apt,
goproxy.cn for Go modules, rsproxy.cn for crates). Only the build downloads
anything, and the benchmark itself runs with `--network none`.

The [Docker benchmark workflow](.github/workflows/docker-benchmark.yml) runs
the same script on every push to `main`, or by hand from the Actions tab, and
writes the tables to the job summary. The container gets 8 CPUs when the
runner has at least 8, and 4 otherwise, so every CI run is one of those two
sizes. The standard GitHub-hosted runner has 4 CPUs; to get 8, set the
repository variable `DOCKER_BENCH_RUNNER` to the label of a larger runner. A
runner with fewer than 4 CPUs fails the job.

## Report format

The reports have the same layout as go-http1-benchmark's: a Summary table of
the run's parameters, each with a description of what it means and the flag
that sets it, then one table per benchmark. The Summary's first row,
`Project`, names the benchmark: `GO-HTTP3-BENCHMARK`.

- `Lang`, right after `Framework`, is the language the server is written in
  (`go`, `rust`, ...), from `config.FrameworkLangs`.
- Rows are ranked best first by `TPS`. In `BenchEcho` and `BenchMultiplex`, a
  tie is broken by `EER`. The ranked columns carry `[↓1]` and `[↓2]` in their
  titles.
- Every ranked column shows each row's share of the best value in that
  column, floored so that only the best row reads `100%`.
- Parameters shared by every row (`Client`, `Client Threads`, `Conns`,
  `Payload`, each benchmark's concurrency, `Echo Total`, `Rate Duration`,
  `Rate SendRate`, `Rate Batch`, and `Echo Pprof` and `Rate Pprof`, whether
  the client profiled the Go servers) are in the Summary table instead of the
  columns. A parameter the rows disagree on lists each value with its
  frameworks, for example `20000 (fib); 19998 (quicgo)`.
- The JSON files keep every field, including `TP50`, `TP75`, `TP90`,
  `CPU Min` and `MEM Min`, which the tables leave out.

`BENCH_REPORT_SORT=framework` (or `-sort=framework`) keeps
`config.FrameworkList` order instead, so that a framework is on the same row
in every table and across runs. Neither order changes the numbers, so
re-running the report step alone is enough:

```sh
bash script/report.sh -sort=framework
```

## Two nodes

To run the servers and the client on separate machines, set `BENCH_ROLE` on
each and give the client side the server's address. The servers bind every
interface, so nothing needs configuring on their side.

```sh
# On the server node: builds the servers, starts them, leaves them running.
BENCH_ROLE=server bash script/benchmark.sh

# On the client node: builds the client, runs it against the servers, reports.
BENCH_ROLE=client BENCH_SERVER_HOST=10.0.0.2 bash script/benchmark.sh

# Back on the server node, when the run is over.
bash script/killall.sh
```

A node that runs one half gets the whole machine. The client cannot stop
servers it did not start, so every server stays up for the whole run. Use
`BENCH_FRAMEWORKS` to run a subset if the idle servers would disturb the one
being measured. The CPU and MEM columns come from the servers' own `/ps`
route, because the client cannot see the server process from another machine.

## Before running the test

Set the system limits on every machine the benchmark runs on. The client
holds a UDP socket per connection, so it needs the port range and file
descriptor limits as much as the server does, and QUIC wants larger UDP
buffers than the kernel's defaults (quic-go asks for 7MB, and logs a warning
when it cannot have them):

```sh
sysctl -w net.ipv4.ip_local_port_range="1024 65535"
sysctl -w fs.file-max=2000500
sysctl -w fs.nr_open=2000500
ulimit -n 2000500
sysctl -w net.core.rmem_max=7500000
sysctl -w net.core.wmem_max=7500000
sysctl -w net.core.netdev_max_backlog=2048
```

In Docker, `net.core.rmem_max` and `wmem_max` belong to the host, not the
container: set them on the host (or in the Docker Desktop VM) before a large
run.
