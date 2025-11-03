#!/bin/bash

echo "🧪 Testing NATS Integration"
echo "=============================="
echo

# Start subscriber in background
echo "1️⃣  Starting subscriber..."
cargo run --example test_nats_subscriber &
SUB_PID=$!

# Wait for subscriber to be ready
sleep 4

# Run publisher
echo
echo "2️⃣  Running publisher..."
cargo run --example test_nats_publisher

# Wait a bit for message delivery
sleep 2

# Check if subscriber is still running
if ps -p $SUB_PID > /dev/null; then
    echo
    echo "3️⃣  Stopping subscriber..."
    kill $SUB_PID
fi

wait $SUB_PID 2>/dev/null

echo
echo "✅ Test complete!"
