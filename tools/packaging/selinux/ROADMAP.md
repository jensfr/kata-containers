# kata_shim_t SELinux Policy -- Roadmap

## Current state: Tech Preview (v3)

The v3 policy provides a dedicated `kata_shim_t` SELinux domain for the Kata shimv2 process, replacing the broad `container_runtime_t` that the shim currently runs as. The policy is tested in enforcing mode on OCP 4.20 / Kata 3.21.0 / RHCOS 9.6.

### What was tested

- Pod create and delete (clean shutdown, no orphan processes)
- kubectl exec and kubectl logs
- MCS isolation (two pods with different QEMU categories)
- NetworkPolicy
- Full pod lifecycle with zero enforcing AVC denials

### Key technical finding: dbus send_msg deadlock

The most critical discovery during development: the shim creates cgroups via systemd's `StartTransientUnit` dbus call. This requires `dbus send_msg` permission in BOTH directions:

- `kata_shim_t -> init_t` (shim sends request)
- `init_t -> kata_shim_t` (systemd sends response)

Without the return direction, dbus-broker silently drops systemd's response and the shim deadlocks. Setting `kata_shim_t` to permissive mode does NOT fix this, because `init_t` remains in enforcing mode and blocks the response. This is not documented in any SELinux reference we found and will affect anyone creating a custom domain that communicates with systemd via dbus.

### Files

| File | Description |
|------|-------------|
| `kata_shim.cil` | Base policy (generic, works on any SELinux + container-selinux system) |
| `kata_shim_osc.cil` | OpenShift overlay (osc-monitor access) |
| `test_kata_shim_selinux.sh` | 10-test validation suite |
| `demo-kata-shim-t.cast` | Asciinema recording of enforcing mode demo |

## Known gaps

### High severity

**Per-pod MCS on shim sandbox directories**

The shim runs as `kata_shim_t:s0` (no MCS categories). QEMU gets per-pod MCS (`container_kvm_t:s0:cX,cY`), but the sandbox directories under `/run/kata-containers/shared/sandboxes/` are created by the shim with `s0` and have no per-pod isolation. A compromised shim A can access shim B's sandbox directories, QMP sockets, and shared mount points.

Fix: call `selinux.SetFSCreateLabel()` in the shim before creating sandbox directories, using the MCS categories from the OCI spec's `Process.SelinuxLabel`. Approximately 10 lines of Go. This would be an upstream kata-containers PR.

**Test coverage**

Only basic pods (sleep infinity) have been tested. The following need testing before GA:
- GPU passthrough (VFIO)
- Confidential Containers (kata-snp, kata-tdx, kata-se)
- Volume mounts (PVC, ConfigMap as files, downward API)
- Multi-container pods (init containers, sidecars)
- Node drain / pod eviction
- OOM conditions
- Pod with resource limits

### Medium severity

**Mount namespace for the shim**

The shim requires `CAP_SYS_ADMIN` for bind mount operations. Adding `unshare(CLONE_NEWNS)` at shim startup would scope `sys_admin` to a private mount namespace. A compromised shim could not create mounts visible to the host. Approximately 5 lines of Go. Upstream kata PR, could be combined with the MCS change.

**var_run_t access too broad**

The policy allows the shim to create files, sockets, and directories in any `var_run_t` location (all of `/run/`). The shim only needs access to `/run/containerd/s/` (ttrpc socket) and `/run/vc/` (sandbox state). A type transition rule could give shim-created objects a dedicated type. Alternatively, defer this to seitan for path-based filtering.

**Propose to container-selinux**

The long-term home for `kata_shim_t` is Dan Walsh's container-selinux package, where `container_kvm_t` and `container_runtime_t` already live. Filing an issue and getting review from the container-selinux team is required for GA.

**Operator deployment**

The policy is currently installed manually via `semodule -i`. For production, it needs to be deployed via the OSC operator, either through RHCOS layered images (MachineOSConfig) or the operator's DaemonSet install flow.

### Low severity

**etc_t access too broad**

The shim can read any file under `etc_t` (all of `/etc/`). It only needs `/etc/kata-containers/`. A dedicated `kata_config_t` type with file context rules for `/etc/kata-containers/` would narrow this. Alternatively, defer to seitan for path-based filtering.

**device_t access too broad**

The policy allows chr_file access to `device_t`, which covers any device not assigned a specific type. The shim only needs `/dev/vhost-net` and `/dev/vhost-vsock`. Defer to seitan for device path filtering.

## Architecture decision: SELinux + seitan

We decided to stop SELinux policy tightening at the v3 level and use seitan (Stefano Brivio's seccomp-notify tool) for fine-grained argument-level restrictions. The rationale:

**SELinux is the right tool for:**
- Type enforcement (kata_shim_t domain isolation)
- Cross-process access control (which types can interact)
- MCS pod isolation
- Structural neverallow guards

**seitan is the right tool for:**
- "mount() only to /run/kata-containers/*" (SELinux cannot inspect syscall arguments)
- "open() only /etc/kata-containers/*" (seitan checks file paths)
- "only access /dev/kvm, /dev/vhost-net, /dev/vhost-vsock" (seitan checks device paths)

The division is clean: SELinux handles "who can talk to whom" (label-based), seitan handles "with what arguments" (parameter-level). They don't overlap. This avoids creating many dedicated SELinux types (kata_config_t, kata_var_run_t, etc.) that add policy complexity without improving the security model.

## GA requirements (priority order)

1. Per-pod MCS on shim (closes lateral movement gap)
2. Test coverage (GPU, CoCo, volumes)
3. Operator deployment (layered image or DaemonSet)
4. Propose to container-selinux
5. seitan integration (collaboration with Stefano Brivio)
6. Mount namespace for shim (scopes sys_admin)

Items 1-2 make it GA-ready from a security perspective.
Items 3-4 make it GA-ready from a product perspective.
Items 5-6 are defense-in-depth improvements.

## Capability justifications

Every capability in the policy is documented with the specific operation that requires it:

| Capability | Justification |
|---|---|
| kill | Send signals to QEMU/virtiofsd child processes |
| sys_admin | mount/bind-mount sandbox dirs, resolv.conf, container rootfs (Linux requires CAP_SYS_ADMIN for mount() in init mount namespace) |
| net_admin | Create tap devices, configure network via netlink |
| chown | ChownToParent() for virtiofsd socket (virtiofsd.go:95) |
| dac_override | Access sandbox files created by CRI-O with different ownership (confirmed needed by testing removal) |
| dac_read_search | Traverse CRI-O storage directories not owned by the shim (confirmed needed by testing removal) |
| fowner | File operations on sandbox files with different ownership |
| fsetid | Preserve setuid/setgid bits during chown |
| setuid/setgid | virtiofsd process runs as different user |
| ipc_lock | mlock for QEMU memory backing (memory-backend-file) |
| sys_resource | setrlimit for QEMU process resource limits |

`dac_override` and `dac_read_search` were tested by removal: the shim fails immediately without them when accessing CRI-O's overlay storage.

## neverallow rules

The policy includes neverallow rules preventing:
- Writing to /etc (etc_t)
- TCP socket creation (shim uses Unix and vsock only)
- Raw socket creation
- Loading kernel modules (sys_module)
- Changing system clock (sys_time)
- Ptrace (sys_ptrace)
- Accessing user home directories
