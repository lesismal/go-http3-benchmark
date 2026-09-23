#!/usr/bin/env bash
# script/docker_benchmark.sh with the image built from mirrors reachable from
# mainland China: Docker Hub, Debian apt, the Go module proxy and crates.io. Only
# the build downloads anything; the benchmark runs with --network none either
# way, so the numbers are the same as script/docker_benchmark.sh's.
#
# Every option and flag is script/docker_benchmark.sh's; see its --help. Each
# mirror below can be overridden, or set to an empty value to use the upstream:
#   DOCKER_BENCH_APT_MIRROR= bash script/docker_benchmark_cn.sh --smoke
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# ${VAR-default}, not ${VAR:-default}: an empty value is kept, to go direct.
export DOCKER_BENCH_GO_IMAGE=${DOCKER_BENCH_GO_IMAGE-docker.m.daocloud.io/library/golang:1.27-bookworm}
export DOCKER_BENCH_RUST_IMAGE=${DOCKER_BENCH_RUST_IMAGE-docker.m.daocloud.io/library/rust:1-bookworm}
export DOCKER_BENCH_CARGO_MIRROR=${DOCKER_BENCH_CARGO_MIRROR-sparse+https://rsproxy.cn/index/}
export DOCKER_BENCH_APT_MIRROR=${DOCKER_BENCH_APT_MIRROR-https://mirrors.aliyun.com}
export DOCKER_BENCH_GOPROXY=${DOCKER_BENCH_GOPROXY-https://goproxy.cn}

exec bash "$script_dir/docker_benchmark.sh" "$@"
