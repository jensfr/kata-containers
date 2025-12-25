# File-Copy Approach for DaemonSet Deployment

This directory contains the implementation of the **file-copy approach** for deploying Kata Containers on OpenShift HCP clusters via DaemonSet.

## Overview

In this approach:
1. Dockerfile extracts components from kata-containers RPM at **build time**
2. kata-deploy copies files to `/opt/kata/` on the host at **runtime**
3. SELinux contexts are set manually via `chcon`
4. QEMU is installed via `rpm-ostree install --apply-live`
5. Initrd is built at runtime using `kata-osbuilder.sh`

## Files

| File | Description |
|------|-------------|
| `Dockerfile.full` | Multi-stage build that extracts from kata-containers RPM |
| `configuration-qemu.toml` | Kata config for DaemonSet mode (paths to /opt/kata/) |
| `configuration-qemu-*.toml` | Configs for TDX, SNP, SE confidential computing |
| `osc_monitor.cil` | SELinux policy for QEMU QMP socket access |
| `README-DAEMONSET.md` | Deployment guide |
| `CHANGES-BY-COMPONENT.md` | PR organization document |
| `UPSTREAM-DESIGN.md` | Upstream architecture proposal |
| `binary-patches/` | Modified Rust source files |

## Modified Rust Files

The `binary-patches/` directory contains:

- `install.rs` - Added functions:
  - `install_qemu_via_rpm_ostree()` - Installs QEMU via rpm-ostree
  - `build_initrd_with_modules()` - Runs kata-osbuilder.sh via nsenter
  - `set_selinux_contexts()` - Sets bin_t/lib_t on copied files
  - `install_selinux_policy()` - Installs osc_monitor.cil

- `platform.rs` - Platform detection (RHCOS, RHEL, Generic)

- `mod.rs` - Adds `pub mod platform;`

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  kata-deploy container image                                 │
│  /opt/kata-artifacts/                                       │
│  ├── opt/kata/bin/containerd-shim-kata-v2  (extracted RPM) │
│  ├── opt/kata/libexec/.../osbuilder/       (extracted RPM) │
│  ├── opt/kata/usr/.../agent/               (extracted RPM) │
│  └── rpms/qemu-*.rpm                       (for rpm-ostree) │
└─────────────────────────────────────────────────────────────┘
                         │
                         ▼ copy_artifacts()
┌─────────────────────────────────────────────────────────────┐
│  Host: /opt/kata/                                           │
│  ├── bin/containerd-shim-kata-v2                           │
│  ├── libexec/kata-containers/osbuilder/                    │
│  └── share/defaults/kata-containers/*.toml                 │
└─────────────────────────────────────────────────────────────┘
                         │
                         ▼ rpm-ostree install --apply-live
┌─────────────────────────────────────────────────────────────┐
│  Host: /usr/libexec/                                        │
│  ├── qemu-kvm                                               │
│  └── virtiofsd                                              │
└─────────────────────────────────────────────────────────────┘
```

## How to Reproduce

1. Build the container image:
```bash
cd /path/to/kata-containers
podman build --platform=linux/amd64 \
  -f tools/packaging/kata-deploy/daemonset/filecopy/Dockerfile.full \
  -t quay.io/YOUR_USER/kata-deploy:filecopy .
```

2. Deploy to cluster:
```bash
# See README-DAEMONSET.md for full instructions
```

## Limitations

- Manual SELinux context management
- Files in non-standard paths (`/opt/kata/` instead of `/usr/`)
- CRI-O config must point to custom paths
- Shim/agent extracted separately from QEMU (mixed approach)

## See Also

- `../rpm-ostree/` - Alternative approach using rpm-ostree for all components
