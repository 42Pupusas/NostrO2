#!/bin/bash
# Script to monitor and compare memory/CPU usage of both implementations

echo "=== Memory/CPU Comparison Test ==="
echo ""

# Function to monitor a process
monitor_process() {
    local name=$1
    local cmd=$2

    echo "Starting $name..."
    $cmd &
    local pid=$!

    echo "Monitoring PID: $pid"
    echo "Time(s) | CPU% | MEM(MB) | VSZ(MB) | RSS(MB)"
    echo "--------|------|---------|---------|--------"

    for i in {1..30}; do
        if ps -p $pid > /dev/null; then
            # Get CPU%, Memory%, VSZ, RSS
            stats=$(ps -p $pid -o %cpu,%mem,vsz,rss --no-headers)
            cpu=$(echo $stats | awk '{print $1}')
            mem=$(echo $stats | awk '{print $2}')
            vsz=$(echo $stats | awk '{print $3/1024}')
            rss=$(echo $stats | awk '{print $4/1024}')

            printf "%7d | %4s | %7.1f | %7.1f | %7.1f\n" $i "$cpu" "$mem" "$vsz" "$rss"
        else
            echo "Process ended"
            break
        fi
        sleep 1
    done

    # Cleanup
    kill $pid 2>/dev/null
    wait $pid 2>/dev/null
    echo ""
}

# Build both examples
echo "Building examples..."
cargo build --release --example memory_test --package ring-relay-client
cargo build --release --example memory_test --package nostro2-relay
echo ""

# Test Ring Relay
echo "================================"
echo "Testing Ring Relay (OS Threads)"
echo "================================"
monitor_process "Ring Relay" "cargo run --release --example memory_test --package ring-relay-client"

# Test Async Relay
echo "==============================="
echo "Testing Async Relay (Tokio)"
echo "==============================="
monitor_process "Async Relay" "cargo run --release --example memory_test --package nostro2-relay"

echo "=== Comparison Complete ==="
echo ""

# Large Payload Benchmarks
echo "============================================"
echo "Large Payload Benchmarks (~20KB per message)"
echo "============================================"
echo ""
echo "Running large_payload benchmark (ring buffer vs async channels)..."
echo "This sends 10K events with ~20KB payloads through each implementation."
echo ""
cargo bench --bench large_payload --package ring-relay-client 2>&1 | grep -E "(large_payload|time:|thrpt:)"
echo ""
echo "=== Large Payload Benchmark Complete ==="
