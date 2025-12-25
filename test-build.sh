#!/bin/bash
set -e

echo "=== Red Hat Build Environment Test ==="
echo "Go version:"
go version

echo "=== Testing Go module resolution ==="
cd src/runtime
go mod download
echo "✅ Go modules downloaded successfully"

echo "=== Testing monitor build ==="
make clean
make monitor
echo "✅ kata-monitor built successfully"

echo "=== Build verification ==="
ls -la kata-monitor
file kata-monitor
./kata-monitor --version || echo "Version check completed"

echo "=== Build test completed successfully! ==="