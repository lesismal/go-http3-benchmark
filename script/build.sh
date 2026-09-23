#!/bin/bash

# Also support invoking this script directly from the repository root.
if ! declare -p frameworks >/dev/null 2>&1 || ! declare -F bench_runs_servers >/dev/null 2>&1; then
    . ./script/env.sh || { return 1 2>/dev/null || exit 1; }
fi

# What cargo builds: every Rust member of the workspace in Cargo.toml goes
# through one invocation, so that quiche and its BoringSSL are compiled once
# and shared. --locked holds it to Cargo.lock, which also lets a container
# without a network build from the crates its image already fetched.
cargo_build() {
    local packages=()
    local p
    for p in "$@"; do
        packages+=(-p "$p")
    done
    echo "cargo build ${packages[*]} ..."
    cargo build --release --locked "${packages[@]}" || return 1
    echo "cargo build done"
    echo
}

build_benchmark() {
    . ./script/clean.sh
    mkdir -p ./output/bin ./output/log ./output/report || return 1

    local rust_packages=()
    local rust_servers=()

    # Each half only builds what it runs.
    if bench_runs_servers; then
        for f in "${frameworks[@]}"; do
            if [ -f "./frameworks/${f}/Cargo.toml" ]; then
                # Rust: package <name>-server, binary target/release/<name>-server.
                rust_packages+=("${f}-server")
                rust_servers+=("${f}")
                continue
            fi
            echo "build ${f} ..."
            go build -o "./output/bin/${f}.server" "./frameworks/${f}" || return 1
            echo "build ${f} done"
            echo
        done
    else
        echo "skip building the servers: they run on ${BENCH_SERVER_HOST}"
        echo
    fi

    local client="benchcli-${BENCH_CLIENT}"
    if bench_runs_clients && [ -f "./${client}/Cargo.toml" ]; then
        rust_packages+=("${client}")
    fi

    if [ "${#rust_packages[@]}" -gt 0 ]; then
        cargo_build "${rust_packages[@]}" || return 1
    fi
    for f in ${rust_servers[@]+"${rust_servers[@]}"}; do
        cp "./target/release/${f}-server" "./output/bin/${f}.server" || return 1
    done

    if bench_runs_clients; then
        if [ -f "./${client}/Cargo.toml" ]; then
            cp "./target/release/${client}" ./output/bin/bench.client || return 1
        else
            echo "build client: ${client} ..."
            go build -o ./output/bin/bench.client "./${client}" || return 1
            echo "build client done"
            echo
        fi
        echo "build report: benchreport ..."
        go build -o ./output/bin/bench.report ./benchreport || return 1
        echo "build report done"
    else
        echo "skip building the client: it runs elsewhere"
    fi
}

build_benchmark || { return 1 2>/dev/null || exit 1; }
