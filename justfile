set shell := ["bash", "-euo", "pipefail", "-c"]

project_dir := "/home/sam-sullivan/reth-exex-liquidity"
eth_docker_dir := "/home/sam-sullivan/eth-docker"

default:
    @just --list

reth-latest:
    curl -s https://api.github.com/repos/paradigmxyz/reth/releases/latest | rg 'tag_name|published_at|name'

set-reth-version version:
    [[ "{{version}}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || (echo "Version must look like v1.11.2" && exit 1)
    perl -i -pe 's#^(reth(?:-[a-z-]+)?\s*=\s*\{[^}]*tag\s*=\s*")v\d+\.\d+\.\d+(".*)$#$1{{version}}$2#' Cargo.toml
    rg -n '^reth|^reth-' Cargo.toml

build-exex:
    cd {{project_dir}}
    docker run --rm --network host \
      -v {{project_dir}}:/workspace \
      -w /workspace \
      ubuntu:22.04 \
      bash -c "apt-get update -qq && apt-get install -y -qq curl build-essential pkg-config libssl-dev git libclang-dev && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && . \$HOME/.cargo/env && CARGO_TARGET_DIR=/workspace/target-user cargo build --release"

restart-execution:
    cd {{eth_docker_dir}} && ./ethd restart execution

verify-exex:
    cd {{eth_docker_dir}} && for _ in {1..24}; do logs=$(./ethd logs execution --tail 200 | sed -r 's/\x1B\[[0-9;]*[A-Za-z]//g'); if grep -Eq 'Liquidity ExEx starting|Transfers ExEx starting|Balance Monitor ExEx starting|Pool Creations ExEx starting|NATS connected successfully' <<< "$logs" && grep -Eq 'connected_peers=[1-9]' <<< "$logs"; then grep -E 'Liquidity ExEx starting|Transfers ExEx starting|Balance Monitor ExEx starting|Pool Creations ExEx starting|NATS connected successfully|connected_peers=' <<< "$logs"; exit 0; fi; sleep 5; done; ./ethd logs execution --tail 200; exit 1

rebuild-and-restart:
    just build-exex
    just restart-execution
    just verify-exex

deploy version:
    just set-reth-version {{version}}
    just build-exex
    just restart-execution
    just verify-exex
