# Integrating ExEx into eth-docker Build

Your eth-docker is building Reth from source. To add the ExEx, you need to modify the Dockerfile to build your ExEx code alongside Reth.

## Current Setup

Your `reth.yml` shows:
```yaml
build:
  context: ./reth
  dockerfile: ${RETH_DOCKERFILE}
  args:
    - BUILD_TARGET=${RETH_SRC_BUILD_TARGET:-main}
    - SRC_REPO=${RETH_SRC_REPO:-https://github.com/paradigmxyz/reth}
```

This builds Reth from the official repo. We need to add your ExEx code.

## Solution: Modify Reth Dockerfile

### Step 1: Locate the Dockerfile

On your eth-docker server:

```bash
cd ~/eth-docker/reth
ls -la
```

You should see a Dockerfile (the name is in `${RETH_DOCKERFILE}` variable).

### Step 2: Find the Dockerfile Name

Check your `.env` file:

```bash
cd ~/eth-docker
grep RETH_DOCKERFILE .env
```

Common values:
- `Dockerfile.source` - builds from source
- `Dockerfile.binary` - uses pre-built binary

### Step 3: Modify the Dockerfile

You have two options:

## Option A: Add ExEx to Existing Reth Build (Recommended)

This modifies the Dockerfile to build your ExEx alongside Reth.

**Edit** `~/eth-docker/reth/Dockerfile.source` (or whatever the filename is):

```dockerfile
# ... existing Reth build stages ...

# Add this stage BEFORE the final stage
FROM builder AS exex-builder

# Clone your ExEx repository
RUN git clone https://github.com/Sgas17/reth-exex-liquidity.git /build/exex

# Build the ExEx
WORKDIR /build/exex
RUN cargo build --release

# In the final stage, copy the ExEx binary
FROM ubuntu:24.04

# ... existing COPY commands ...

# Add this line to copy your ExEx
COPY --from=exex-builder /build/exex/target/release/exex /usr/local/bin/exex

# ... rest of Dockerfile ...
```

### Step 4: Update entrypoint in reth.yml

Change the entrypoint to use your ExEx binary instead of `reth`:

```yaml
entrypoint:
  - docker-entrypoint.sh
  - exex  # Changed from 'reth' to 'exex'
  - node
  - --datadir
  # ... rest stays the same ...
```

## Option B: Use Remote ExEx (Alternative)

If you don't want to modify the Dockerfile, you can run the ExEx as a separate container.

### 1. Create ExEx Dockerfile

Create `~/eth-docker/exex/Dockerfile`:

```dockerfile
FROM rust:1.81-bookworm AS builder

WORKDIR /build

# Clone and build ExEx
RUN git clone https://github.com/Sgas17/reth-exex-liquidity.git .
RUN cargo build --release

FROM ubuntu:24.04

RUN apt-get update && apt-get install -y libssl3 ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/exex /usr/local/bin/exex

ENTRYPOINT ["/usr/local/bin/exex"]
```

### 2. Add ExEx Service to reth.yml

```yaml
services:
  # ... existing execution service ...

  execution-exex:
    build:
      context: ./exex
      dockerfile: Dockerfile
    image: reth-exex:local
    restart: "unless-stopped"
    depends_on:
      - execution
    volumes:
      - reth-el-data:/var/lib/reth
      - /var/run/docker.sock:/var/run/docker.sock  # For Docker communication
    networks:
      default:
        aliases:
          - exex
    command:
      - node
      - --datadir
      - /var/lib/reth
      # Add your ExEx-specific flags here
```

**Note**: This option is more complex and requires additional socket configuration.

## Recommended Approach: Option A

**Option A is simpler** - it builds your ExEx into the same container as Reth.

### Complete Modified Dockerfile Example

Here's a complete example of what your `Dockerfile.source` should look like:

```dockerfile
# Multi-stage build for Reth + ExEx

# Stage 1: Build Reth
FROM rust:1.81-bookworm AS reth-builder

ARG SRC_REPO=https://github.com/paradigmxyz/reth
ARG BUILD_TARGET=main

WORKDIR /build

RUN git clone ${SRC_REPO} . && \
    git checkout ${BUILD_TARGET} && \
    cargo build --release --bin reth

# Stage 2: Build ExEx (NEW)
FROM rust:1.81-bookworm AS exex-builder

WORKDIR /build

# Clone your ExEx repo
RUN git clone https://github.com/Sgas17/reth-exex-liquidity.git .

# Build the ExEx
RUN cargo build --release

# Stage 3: Final image
FROM ubuntu:24.04

# Install runtime dependencies
RUN apt-get update && \
    apt-get install -y libssl3 ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Copy Reth binary (not used, but kept for compatibility)
COPY --from=reth-builder /build/target/release/reth /usr/local/bin/reth

# Copy ExEx binary (this is what we'll run)
COPY --from=exex-builder /build/target/release/exex /usr/local/bin/exex

# Copy entrypoint
COPY docker-entrypoint.sh /usr/local/bin/
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

# Create reth user
RUN useradd -m -u 10001 reth

# Create data directory
RUN mkdir -p /var/lib/reth && chown -R reth:reth /var/lib/reth

USER reth

EXPOSE 8545 8546 8551 30303 30303/udp 30304/udp 6060

ENTRYPOINT ["docker-entrypoint.sh"]
```

### Updating reth.yml entrypoint

**Change this**:
```yaml
entrypoint:
  - docker-entrypoint.sh
  - reth
  - node
```

**To this**:
```yaml
entrypoint:
  - docker-entrypoint.sh
  - exex
  - node
```

## Rebuild and Deploy

### Step 1: Rebuild the Image

```bash
cd ~/eth-docker

# Stop Reth
./ethd stop execution

# Rebuild the image
docker-compose -f reth.yml build execution

# Or if using multiple compose files:
docker-compose -f docker-compose.yml -f reth.yml build execution
```

### Step 2: Start with New ExEx

```bash
./ethd start execution

# Or
./ethd up
```

### Step 3: Verify

Check logs for the ExEx startup:

```bash
./ethd logs execution -f | grep -E "ExEx|Liquidity|NATS"
```

You should see:
```
INFO Liquidity ExEx started
INFO ✅ Connected to NATS at nats://localhost:4222
INFO Subscribed to NATS subject: whitelist.pools.ethereum.minimal
```

## Environment Variables for ExEx

Add to your `.env` file:

```bash
# ExEx Configuration
NATS_URL=nats://nats:4222
SOCKET_PATH=/var/run/exex.sock
```

Then update `reth.yml` to pass them:

```yaml
environment:
  - NATS_URL=${NATS_URL:-nats://localhost:4222}
  - SOCKET_PATH=${SOCKET_PATH:-/var/run/exex.sock}
```

## Adding NATS to eth-docker

Your ExEx needs NATS running. Add it to your docker-compose:

Create `~/eth-docker/nats.yml`:

```yaml
services:
  nats:
    image: nats:latest
    restart: unless-stopped
    ports:
      - ${NATS_PORT:-4222}:4222
      - ${NATS_HTTP_PORT:-8222}:8222
    networks:
      default:
        aliases:
          - nats
    command:
      - "-js"  # Enable JetStream
      - "-m"   # Enable monitoring
      - "8222"
    volumes:
      - nats-data:/data

volumes:
  nats-data:
```

Then start it:

```bash
cd ~/eth-docker
docker-compose -f nats.yml up -d
```

## Troubleshooting

### Build fails with "permission denied"

Make sure docker has permissions:
```bash
sudo usermod -aG docker $USER
newgrp docker
```

### ExEx not starting

Check the binary exists:
```bash
docker exec -it reth-execution-1 ls -la /usr/local/bin/exex
docker exec -it reth-execution-1 /usr/local/bin/exex --version
```

### NATS connection fails

Check NATS is running:
```bash
docker ps | grep nats
docker logs nats
```

## Next Steps

Once the ExEx is running with eth-docker:

1. ✅ ExEx receives blocks from Reth
2. ✅ ExEx connects to NATS
3. ✅ Run dynamicWhitelist to publish pool whitelist
4. ✅ ExEx receives differential updates

---

**Summary**: Modify the Reth Dockerfile to build your ExEx, update the entrypoint in reth.yml to run `exex` instead of `reth`, rebuild, and restart.
