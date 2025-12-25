# Kata Containers DaemonSet Deployment for OpenShift HCP

This directory contains the components needed to deploy Kata Containers on OpenShift Hosted Control Plane (HCP) clusters (ROSA, ARO, IBM Cloud ROKS) using DaemonSet deployment mode.

## Overview

Traditional Kata Containers deployment on OpenShift uses the Machine Config Operator (MCO) to install components via rpm-ostree. However, HCP clusters don't have access to MCO, so we use a DaemonSet-based approach instead.

### Key Design Decisions

1. **Use kata-containers RPM components**: All kata components (shim, agent, osbuilder) come from the downstream kata-containers RPM to ensure version compatibility (3.17.0).

2. **Host kernel**: Uses the node's kernel (`/lib/modules`) instead of shipping a kernel, reducing image size and ensuring kernel compatibility.

3. **Runtime initrd generation**: Uses `kata-osbuilder.sh` (dracut-based) to build the initrd at install time on each node, incorporating the host's kernel modules.

4. **QEMU via rpm-ostree**: Installs QEMU and virtiofsd on the host via rpm-ostree from RHCOS extension container, ensuring proper library paths and BIOS file access.

5. **Rust kata-deploy binary**: Uses the new Rust implementation for the DaemonSet controller.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     kata-deploy DaemonSet                        │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  /opt/kata-artifacts/                                    │    │
│  │  ├── opt/kata/bin/containerd-shim-kata-v2  (from RPM)   │    │
│  │  ├── opt/kata/libexec/kata-containers/osbuilder/        │    │
│  │  │   └── kata-osbuilder.sh (dracut-based initrd builder)│    │
│  │  ├── opt/kata/usr/libexec/kata-containers/agent/        │    │
│  │  │   └── usr/bin/kata-agent  (from RPM, 3.17.0)         │    │
│  │  ├── opt/kata/share/defaults/kata-containers/           │    │
│  │  │   └── configuration-qemu.toml                         │    │
│  │  ├── rpms/  (QEMU RPMs for rpm-ostree install)          │    │
│  │  └── selinux/osc_monitor.cil                             │    │
│  └─────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                        Host Node                                 │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Installed via kata-deploy:                              │    │
│  │  /opt/kata/bin/containerd-shim-kata-v2                  │    │
│  │  /opt/kata/share/defaults/kata-containers/*.toml        │    │
│  │  /var/cache/kata-containers/vmlinuz.container (symlink) │    │
│  │  /var/cache/kata-containers/kata-containers-initrd.img  │    │
│  │  /etc/crio/crio.conf.d/99-kata-deploy                   │    │
│  └─────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Installed via rpm-ostree:                               │    │
│  │  /usr/libexec/qemu-kvm                                   │    │
│  │  /usr/libexec/virtiofsd                                  │    │
│  │  /usr/share/qemu/ (BIOS files)                           │    │
│  └─────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

## Components

### 1. Dockerfile.full
Multi-stage Dockerfile that:
- Builds the Rust kata-deploy binary
- Extracts shim, agent, and osbuilder from kata-containers RPM
- Collects QEMU RPMs from RHCOS extension container
- Packages everything for DaemonSet deployment

### 2. configuration-qemu.toml
Kata configuration based on the official kata-containers RPM config with modifications:
- `kernel` and `initrd` paths point to `/var/cache/kata-containers/`
- `disable_guest_selinux=true` in `[hypervisor.qemu]` section (critical!)
- Debug enabled for troubleshooting

### 3. Rust kata-deploy binary (binary/src/)
Modifications to support DaemonSet mode:
- Calls `kata-osbuilder.sh` via nsenter to build initrd on the host
- Installs QEMU via rpm-ostree
- Sets SELinux contexts on binaries
- Installs SELinux policy module

### 4. osc_monitor.cil
SELinux policy module granting QEMU access to `/run/vc/` for QMP sockets.

## Building

```bash
# From kata-containers repository root
cd /path/to/kata-containers

# Build the image
podman build --platform=linux/amd64 \
  -f tools/packaging/kata-deploy/Dockerfile.full \
  -t quay.io/YOUR_USER/kata-deploy-rust:full-osbuilder .

# Push to registry
podman push quay.io/YOUR_USER/kata-deploy-rust:full-osbuilder
```

## Deployment

### Prerequisites
1. OpenShift HCP cluster (ROSA, ARO, or ROKS)
2. Worker nodes with nested virtualization enabled
3. Public container registry (or configured pull secret)

### Deploy the DaemonSet

```yaml
apiVersion: apps/v1
kind: DaemonSet
metadata:
  name: kata-deploy
  namespace: openshift-sandboxed-containers-operator
spec:
  selector:
    matchLabels:
      app: kata-deploy
  template:
    metadata:
      labels:
        app: kata-deploy
    spec:
      serviceAccountName: kata-deploy
      hostPID: true
      nodeSelector:
        node-role.kubernetes.io/worker: ""
      containers:
      - name: kata-deploy
        image: quay.io/YOUR_USER/kata-deploy-rust:full-osbuilder
        securityContext:
          privileged: true
        env:
        - name: SHIMS_X86_64
          value: "kata-qemu"
        - name: DEBUG
          value: "true"
        volumeMounts:
        - name: host
          mountPath: /host
        - name: dbus
          mountPath: /var/run/dbus
        - name: systemd
          mountPath: /run/systemd
      volumes:
      - name: host
        hostPath:
          path: /
      - name: dbus
        hostPath:
          path: /var/run/dbus
      - name: systemd
        hostPath:
          path: /run/systemd
```

### Create RuntimeClass

```yaml
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: kata-qemu
handler: kata-qemu
scheduling:
  nodeSelector:
    katacontainers.io/kata-runtime: "true"
```

### Test

```bash
# Create a test pod
oc run kata-test --image=registry.access.redhat.com/ubi9/ubi-minimal:latest \
  --restart=Never \
  --overrides='{"spec":{"runtimeClassName":"kata-qemu","nodeSelector":{"katacontainers.io/kata-runtime":"true"},"terminationGracePeriodSeconds":0}}' \
  -- sleep 300

# Verify it's running in a VM
oc exec kata-test -- uname -a
oc exec kata-test -- cat /proc/version
```

## Troubleshooting

### Check kata-deploy logs
```bash
oc logs -n openshift-sandboxed-containers-operator -l app=kata-deploy
```

### Check CRI-O/kata shim logs on node
```bash
oc debug node/<node-name> -- chroot /host journalctl -u crio -n 100
```

### Verify initrd was built
```bash
oc debug node/<node-name> -- chroot /host ls -la /var/cache/kata-containers/
```

### Verify QEMU is installed
```bash
oc debug node/<node-name> -- chroot /host /usr/libexec/qemu-kvm --version
```

### Common Issues

1. **"EINVAL: Invalid argument" during CreateContainer**
   - Check that `disable_guest_selinux=true` is in `[hypervisor.qemu]` section, NOT `[runtime]`

2. **"CreateContainerRequest timed out"**
   - Increase `dial_timeout` and `create_container_timeout` in config
   - Check VM is booting (verify initrd and kernel exist)

3. **QEMU fails to start**
   - Verify SELinux contexts: `chcon -t bin_t /opt/kata/bin/*`
   - Check SELinux policy is installed: `semodule -l | grep osc_monitor`

## Version Compatibility

| Component | Version | Source |
|-----------|---------|--------|
| containerd-shim-kata-v2 | 3.17.0 | kata-containers RPM |
| kata-agent | 3.17.0 | kata-containers RPM |
| kata-osbuilder | 3.17.0 | kata-containers RPM |
| QEMU | 8.2.x | RHCOS extension RPMs |
| virtiofsd | 1.10.x | RHCOS extension RPMs |

**Important**: Shim and agent versions must match for protocol compatibility.

## Files Changed from Upstream

See the patch files in this directory for exact changes:
- `0001-kata-deploy-daemonset-dockerfile.patch`
- `0002-kata-deploy-rust-osbuilder-integration.patch`
- `0003-configuration-qemu-daemonset.patch`
