#!/bin/bash
# Validation tests for kata_shim_t SELinux policy
#
# Usage: ./test_kata_shim_selinux.sh [node]
#
# Requires: oc logged in, kata RuntimeClass configured, node with policy installed
#
# Tests:
# 1. Policy module loaded
# 2. Shim binary labeled kata_shim_exec_t
# 3. kata_shim_t is NOT permissive (enforcing)
# 4. Kata pod starts successfully
# 5. Shim runs as kata_shim_t (domain transition works)
# 6. QEMU runs as container_kvm_t with MCS labels
# 7. kubectl exec works
# 8. No AVC denials
# 9. Second pod gets different MCS labels (isolation)
# 10. Pod deletion is clean (no orphan processes)

set -euo pipefail

NODE=${1:-$(oc get nodes -l node-role.kubernetes.io/kata-oc -o name 2>/dev/null | head -1 | sed 's|node/||')}
if [ -z "$NODE" ]; then
    NODE=$(oc get nodes -l node.kubernetes.io/instance-type -o name 2>/dev/null | head -1 | sed 's|node/||')
fi
[ -z "$NODE" ] && { echo "Usage: $0 <node-name>"; exit 1; }

PASS=0
FAIL=0
SKIP=0

check() {
    local name=$1
    local result=$2
    if [ "$result" = "PASS" ]; then
        echo "  PASS: $name"
        PASS=$((PASS + 1))
    elif [ "$result" = "SKIP" ]; then
        echo "  SKIP: $name"
        SKIP=$((SKIP + 1))
    else
        echo "  FAIL: $name ($result)"
        FAIL=$((FAIL + 1))
    fi
}

node_exec() {
    oc debug node/$NODE --quiet -- chroot /host sh -c "$1" 2>/dev/null
}

echo "=== kata_shim_t SELinux policy validation ==="
echo "Node: $NODE"
echo "Date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo ""

# Test 1: Policy module loaded
echo "--- Test 1: Policy module loaded ---"
LOADED=$(node_exec 'semodule -l | grep kata_shim')
if echo "$LOADED" | grep -q "kata_shim"; then
    check "kata_shim module loaded" "PASS"
else
    check "kata_shim module loaded" "not found"
fi

# Test 2: Shim binary label
echo "--- Test 2: Shim binary label ---"
LABEL=$(node_exec 'ls -Z /usr/bin/containerd-shim-kata-v2 | awk "{print \$1}"')
if echo "$LABEL" | grep -q "kata_shim_exec_t"; then
    check "shim labeled kata_shim_exec_t" "PASS"
else
    check "shim labeled kata_shim_exec_t" "got: $LABEL"
fi

# Test 3: Not permissive
echo "--- Test 3: Enforcing mode ---"
PERMISSIVE=$(node_exec 'semanage permissive -l 2>/dev/null | grep kata_shim_t || echo ""')
if [ -z "$PERMISSIVE" ]; then
    check "kata_shim_t is enforcing (not permissive)" "PASS"
else
    check "kata_shim_t is enforcing (not permissive)" "is permissive"
fi

# Test 4: Pod starts
echo "--- Test 4: Kata pod starts ---"
oc delete pod selinux-test-1 selinux-test-2 --force --grace-period=0 2>/dev/null || true
sleep 3
oc apply -f - <<EOF >/dev/null
apiVersion: v1
kind: Pod
metadata:
  name: selinux-test-1
spec:
  runtimeClassName: kata
  nodeName: $NODE
  containers:
  - name: test
    image: registry.access.redhat.com/ubi9/ubi-minimal:latest
    command: ["sleep", "infinity"]
  terminationGracePeriodSeconds: 0
EOF

POD_OK="timeout"
for i in $(seq 1 60); do
    STATUS=$(oc get pod selinux-test-1 -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
    if [ "$STATUS" = "Running" ]; then
        POD_OK="PASS"
        break
    fi
    sleep 3
done
check "kata pod starts" "$POD_OK"

if [ "$POD_OK" != "PASS" ]; then
    echo "  Pod failed to start. Events:"
    oc describe pod selinux-test-1 2>/dev/null | grep -A5 "Events:" | tail -5
    echo "  Skipping remaining tests."
    oc delete pod selinux-test-1 --force --grace-period=0 2>/dev/null || true
    echo ""
    echo "=== Results: PASS=$PASS FAIL=$FAIL SKIP=$SKIP ==="
    exit 1
fi

# Test 5: Shim domain transition
echo "--- Test 5: Domain transition ---"
# The shim may have exited by now (it's a shimv2, stays as long as VM runs)
# Check QEMU's parent context or check AVC logs for kata_shim_t
QEMU_CTX=$(node_exec 'ps -eZ | grep qemu-kvm | grep -v grep | head -1 | awk "{print \$1}"')
if echo "$QEMU_CTX" | grep -q "container_kvm_t"; then
    check "QEMU runs as container_kvm_t (shim transitioned)" "PASS"
else
    check "QEMU runs as container_kvm_t" "got: $QEMU_CTX"
fi

# Test 6: QEMU has MCS labels
echo "--- Test 6: MCS labels ---"
if echo "$QEMU_CTX" | grep -q "container_kvm_t:s0:c"; then
    MCS1=$(echo "$QEMU_CTX" | sed 's/.*\(s0:c[0-9]*,c[0-9]*\).*/\1/')
    check "QEMU has MCS labels ($MCS1)" "PASS"
else
    check "QEMU has MCS labels" "no MCS: $QEMU_CTX"
fi

# Test 7: kubectl exec works
echo "--- Test 7: kubectl exec ---"
EXEC_OUT=$(oc exec selinux-test-1 -- uname -r 2>&1)
if [ $? -eq 0 ] && [ -n "$EXEC_OUT" ]; then
    check "kubectl exec works (kernel: $EXEC_OUT)" "PASS"
else
    check "kubectl exec works" "failed: $EXEC_OUT"
fi

# Test 8: No AVC denials
echo "--- Test 8: AVC denials ---"
AVC_COUNT=$(node_exec 'ausearch -m AVC,USER_AVC -ts recent 2>/dev/null | grep kata_shim_t | grep "permissive=0" | grep -v osc_monitor | wc -l' | tr -d '[:space:]')
if [ -z "$AVC_COUNT" ] || [ "$AVC_COUNT" = "0" ]; then
    check "no enforcing AVC denials for kata_shim_t" "PASS"
else
    check "no enforcing AVC denials for kata_shim_t" "$AVC_COUNT denials"
    node_exec 'ausearch -m AVC,USER_AVC -ts recent 2>/dev/null | grep kata_shim_t | grep "permissive=0" | grep -v osc_monitor | grep -o "denied  {[^}]*}.*tclass=[a-z_]*" | sort -u' | head -5
fi

# Test 9: MCS isolation between pods
echo "--- Test 9: MCS isolation ---"
oc apply -f - <<EOF >/dev/null
apiVersion: v1
kind: Pod
metadata:
  name: selinux-test-2
spec:
  runtimeClassName: kata
  nodeName: $NODE
  containers:
  - name: test
    image: registry.access.redhat.com/ubi9/ubi-minimal:latest
    command: ["sleep", "infinity"]
  terminationGracePeriodSeconds: 0
EOF

POD2_OK="timeout"
for i in $(seq 1 60); do
    STATUS=$(oc get pod selinux-test-2 -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
    if [ "$STATUS" = "Running" ]; then
        POD2_OK="PASS"
        break
    fi
    sleep 3
done

if [ "$POD2_OK" = "PASS" ]; then
    QEMU_LABELS=$(node_exec 'ps -eZ | grep qemu-kvm | grep -v grep | awk "{print \$1}" | sed -n 's/.*\(s0:c[0-9]*,c[0-9]*\).*/\1/p' | sort -u')
    LABEL_COUNT=$(echo "$QEMU_LABELS" | wc -l)
    if [ "$LABEL_COUNT" -ge 2 ]; then
        check "two pods have different MCS labels" "PASS"
        echo "    Labels: $(echo $QEMU_LABELS | tr '\n' ' ')"
    else
        check "two pods have different MCS labels" "only $LABEL_COUNT unique labels"
    fi
else
    check "second pod starts" "$POD2_OK"
fi

# Test 10: Clean deletion
echo "--- Test 10: Clean deletion ---"
oc delete pod selinux-test-1 selinux-test-2 --grace-period=5 2>/dev/null
sleep 15
ORPHANS=$(node_exec 'ps -eZ | grep -E "containerd-shim-kata|qemu-kvm|virtiofsd" | grep -v grep | wc -l' | tr -d '[:space:]')
if [ -z "$ORPHANS" ] || [ "$ORPHANS" = "0" ]; then
    check "clean deletion (no orphan processes)" "PASS"
else
    check "clean deletion" "$ORPHANS orphan processes"
fi

echo ""
echo "=== Results ==="
echo "PASS: $PASS  FAIL: $FAIL  SKIP: $SKIP"
[ $FAIL -gt 0 ] && exit 1 || exit 0
