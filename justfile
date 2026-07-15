set shell := ["bash", "-euo", "pipefail", "-c"]

project_dir := "/home/sam-sullivan/reth-exex-liquidity"
deployment_compose := "/home/sam-sullivan/defi_arb_rust/deployment/eth-docker/compose.sh"
rollback_dir := "/home/sam-sullivan/rollback/ITE-54/latest"

default:
    @just --list

reth-latest:
    curl -s https://api.github.com/repos/paradigmxyz/reth/releases/latest | rg 'tag_name|published_at|name'

set-reth-version version:
    [[ "{{version}}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || (echo "Version must look like v2.4.0" && exit 1)
    perl -i -pe 's#^(reth(?:-[a-z-]+)?\s*=\s*\{[^}]*tag\s*=\s*")v\d+\.\d+\.\d+(".*)$#$1{{version}}$2#' Cargo.toml
    rg -n '^reth|^reth-' Cargo.toml

verify-rollback:
    test "$(stat -Lc '%a:%u:%g' {{rollback_dir}})" = "700:$(id -u):$(id -g)"
    test -x {{rollback_dir}}/exex.v2.3.0.rollback
    test -s {{rollback_dir}}/exex.provenance.txt
    sha256sum -c {{rollback_dir}}/exex.v2.3.0.sha256
    test "$(stat -Lc '%a:%u:%g' {{rollback_dir}}/eth-docker.env.pre-v2.4.0)" = "600:$(id -u):$(id -g)"
    test "$(docker image inspect --format '{{"{{"}}.Id{{"}}"}}' reth:ite54-pre-v2.4.0)" = "$(cat {{rollback_dir}}/reth-image-pre-v2.4.0.id)"

build-exex: verify-rollback
    service_source="$(mktemp -d)"; trap 'rm -rf "$service_source"' EXIT; \
      git -C /home/sam-sullivan/defi_arb_rust archive HEAD | tar -x -C "$service_source"; \
      docker run --rm --network host \
        -v {{project_dir}}:/workspace \
        -v "$service_source":/defi_arb_rust:ro \
        -w /workspace \
        ubuntu:22.04 \
        bash -c "timeout 180s apt-get -o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 update -qq && timeout 300s apt-get install -y -qq curl build-essential pkg-config libssl-dev git libclang-dev m4 && curl --connect-timeout 15 --max-time 300 --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && . \$HOME/.cargo/env && CARGO_TARGET_DIR=/workspace/target-user cargo build --release && mkdir -p /workspace/target/release && install -m 0755 /workspace/target-user/release/exex /workspace/target/release/.exex.ite54-new && mv -f /workspace/target/release/.exex.ite54-new /workspace/target/release/exex"

build-deployment-image: verify-rollback
    ETH_DOCKER_APPLY=1 {{deployment_compose}} build-execution

restart-execution: verify-rollback
    ETH_DOCKER_APPLY=1 {{deployment_compose}} apply-execution

verify-websocket:
    node -e 'const ws = new WebSocket("ws://127.0.0.1:8546"); const timer = setTimeout(() => process.exit(1), 10000); ws.onopen = () => ws.send(JSON.stringify({jsonrpc:"2.0",method:"web3_clientVersion",params:[],id:1})); ws.onmessage = ({data}) => { const result = JSON.parse(data).result || ""; console.log(result); clearTimeout(timer); ws.close(); if (!result.startsWith("reth/v2.4.0-")) process.exitCode = 1; }; ws.onerror = () => process.exit(1);'

verify-exex: verify-websocket
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
