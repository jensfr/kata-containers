# kata_shim_t SELinux Policy

Dedicated SELinux domain for the Kata Containers shimv2 process, providing
least-privilege confinement and lateral movement prevention between pods.

## What it does

Without this policy, the Kata shim runs as `container_runtime_t` (the same
domain as CRI-O). A compromised shim would have CRI-O-level permissions.

With this policy:
- The shim transitions to `kata_shim_t` on exec (type enforcement)
- QEMU/virtiofsd run as `container_kvm_t` with per-pod MCS labels (MCS isolation)
- The shim can only access its own sandbox, devices, and configuration
- Three confinement layers: VM boundary + SELinux type enforcement + MCS isolation

## Files

| File | Description |
|------|-------------|
| `kata_shim.cil` | Base policy (works on any SELinux + container-selinux system) |
| `kata_shim_osc.cil` | OpenShift Sandboxed Containers overlay (osc-monitor access) |
| `test_kata_shim_selinux.sh` | Validation test suite (10 tests) |

## Requirements

- `container-selinux` package (provides `container_runtime_t`, `container_kvm_t`, etc.)
- systemd-based host
- Kata Containers 3.x with shimv2

## Install

```bash
# Install the base policy
semodule -i kata_shim.cil

# On OpenShift, also install the OSC overlay
semodule -i kata_shim_osc.cil

# Label the shim binary
semanage fcontext -a -t kata_shim_exec_t /usr/bin/containerd-shim-kata-v2
restorecon -v /usr/bin/containerd-shim-kata-v2
```

## Verify

```bash
# Check policy is loaded
semodule -l | grep kata_shim

# Check shim label
ls -Z /usr/bin/containerd-shim-kata-v2
# Expected: system_u:object_r:kata_shim_exec_t:s0

# Start a kata pod, then check processes
ps -eZ | grep qemu
# Expected: system_u:system_r:container_kvm_t:s0:cX,cY

# Run the test suite
./test_kata_shim_selinux.sh <node-name>
```

## Key finding: dbus send_msg

The most critical rules in this policy are the dbus `send_msg` permissions
between `init_t` and `kata_shim_t`. The shim creates cgroups via systemd's
`StartTransientUnit` dbus call. Without bidirectional `send_msg`, dbus
silently drops systemd's response and the shim deadlocks. This is not
fixed by setting `kata_shim_t` to permissive, because the response is
blocked on `init_t`'s side (which remains enforcing).

## Tested configurations

- OCP 4.20 / OSC 1.8 / Kata 3.21.0 on RHCOS 9.6
- Basic pod lifecycle (create, exec, logs, delete)
- Multiple simultaneous pods with MCS isolation
- NetworkPolicy

## Known gaps

- GPU passthrough (VFIO): needs expanded `vfio_device_t` permissions
- Confidential Containers (local SNP/TDX): untested, likely needs minor additions
- Per-pod MCS on sandbox directories: requires CRI-O code change (setfilecon)
