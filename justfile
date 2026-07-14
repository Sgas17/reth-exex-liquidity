set shell := ["bash", "-euo", "pipefail", "-c"]

project_dir := "/home/sam-sullivan/reth-exex-liquidity"
deployment_compose := "/home/sam-sullivan/defi_arb_rust/deployment/eth-docker/compose.sh"

default:
    @just --list

reth-latest:
    curl -s https://api.github.com/repos/paradigmxyz/reth/releases/latest | rg 'tag_name|published_at|name'

set-reth-version version:
    [[ "{{version}}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || (echo "Version must look like v2.4.0" && exit 1)
    perl -i -pe 's#^(reth(?:-[a-z-]+)?\s*=\s*\{[^}]*tag\s*=\s*")v\d+\.\d+\.\d+(".*)$#$1{{version}}$2#' Cargo.toml
    rg -n '^reth|^reth-' Cargo.toml

build-exex:
    cd {{project_dir}}
    docker run --rm --network host \
      -v {{project_dir}}:/workspace \
      -v /home/sam-sullivan/defi_arb_rust:/defi_arb_rust:ro \
      -w /workspace \
      ubuntu:22.04 \
      bash -c "apt-get update -qq && apt-get install -y -qq curl build-essential pkg-config libssl-dev git libclang-dev m4 && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && . \$HOME/.cargo/env && CARGO_TARGET_DIR=/workspace/target-user cargo build --release && mkdir -p /workspace/target/release && install -m 0755 /workspace/target-user/release/exex /workspace/target/release/.exex.ite54-new && mv -f /workspace/target/release/.exex.ite54-new /workspace/target/release/exex"

build-deployment-image:
    ETH_DOCKER_APPLY=1 {{deployment_compose}} build-execution

restart-execution:
    ETH_DOCKER_APPLY=1 {{deployment_compose}} apply-execution

verify-exex:
    for _ in {1..24}; do logs=$({{deployment_compose}} logs execution --tail 200 | sed -r 's/\x1B\[[0-9;]*[A-Za-z]//g'); if grep -Eq 'Liquidity ExEx starting|Transfers ExEx starting|Balance Monitor ExEx starting|Pool Creations ExEx starting|NATS connected successfully' <<< "$logs" && grep -Eq 'connected_peers=[1-9]' <<< "$logs"; then grep -E 'Liquidity ExEx starting|Transfers ExEx starting|Balance Monitor ExEx starting|Pool Creations ExEx starting|NATS connected successfully|connected_peers=' <<< "$logs"; exit 0; fi; sleep 5; done; {{deployment_compose}} logs execution --tail 200; exit 1

rebuild-and-restart:
    just build-exex
    just build-deployment-image
    just restart-execution
    just verify-exex

deploy version:
    just set-reth-version {{version}}
    just build-exex
    just build-deployment-image
    just restart-execution
    just verify-exex
