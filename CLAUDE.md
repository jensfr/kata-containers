# Mission: Deploy Kata Containers on OpenShift HCP Clusters via DaemonSet

## Overview
Deploy Kata Containers on Hosted Control Plane (HCP) OpenShift clusters (ROSA, ARO, IBM Cloud ROKS)
using DaemonSet deployment mode instead of MachineConfig Operator (MCO) / rpm-ostree.

## Key Requirements

### Red Hat Product Requirements
- Build ALL components from source (no upstream pre-built binaries)
- Use Fedora or RHEL base images (never Ubuntu)
- No shortcuts without permission
- If upstream requires a specific version (e.g., Rust 1.90), use that version

### Architecture Decisions
- **kata-deploy**: Use the NEW Rust implementation (`tools/packaging/kata-deploy/binary`), NOT the old shell script
- **containerd-shim-kata-v2**: Use the Go implementation (`src/runtime`), NOT Rust runtime-rs
- **kata-agent**: Use Rust implementation (`src/agent`), statically linked with musl
- **Kernel**: Use host kernel (via /lib/modules), NOT a shipped kernel
- **initrd**: Generate at runtime using osbuilder, NOT shipped pre-built

### Components to Build from Source
1. `kata-deploy` (Rust) - from `tools/packaging/kata-deploy/binary`
2. `containerd-shim-kata-v2` (Go) - from `src/runtime`
3. `kata-agent` (Rust) - from `src/agent`
4. osbuilder scripts - from `tools/osbuilder`

### Environment Variables
- Use `SHIMS_X86_64` (not `SHIMS`) for the Rust kata-deploy binary
- Architecture-specific naming pattern

### Volume Mounts Required
- `/host` - for host filesystem access (to deploy artifacts)
- Runtime configuration at `/etc/crio/crio.conf.d/99-kata-deploy`

## Build Configuration
- Dockerfile: `tools/packaging/kata-deploy/Dockerfile.full`
- Go builder: `golang:1.24-bookworm`
- Rust builder: `rustlang/rust:nightly-bookworm` (for Rust 1.90+ requirement)
- Runtime base: `fedora:40`

## Container Registry
**IMPORTANT**: The quay.io username is `jensfr` (not `jfreiman`)

Available repositories:
- `quay.io/jensfr/kata-deploy-rust:full-osbuilder` - Current working image for DaemonSet mode
- `quay.io/jensfr/kata-deploy-rust:latest` - Alternative tag

Repository naming convention:
- Use `quay.io/jensfr/kata-deploy-rust` for kata-deploy images
- Repository must be PUBLIC for cluster to pull (no pull secrets configured)

## Commands Reference
```bash
# Build full image
podman build --platform=linux/amd64 -f tools/packaging/kata-deploy/Dockerfile.full \
  -t quay.io/jensfr/daemonset-controller:kata-deploy-full .

# Push image
podman push quay.io/jensfr/daemonset-controller:kata-deploy-full

# Test kata container
oc run hello-kata --image=quay.io/libpod/alpine --restart=Never \
  --overrides='{"spec":{"runtimeClassName":"kata-qemu"}}'
```

## Initrd Rebuild Hook Design (TODO)

When using host kernel, initrd must be rebuilt on kernel updates.
Implement this via extensible hooks in Rust kata-deploy:

### Proposed Module Structure
```
src/hooks/
├── mod.rs           # LifecycleHook trait, HookRegistry
├── initrd.rs        # InitrdRebuildHook for host-kernel platforms
└── systemd.rs       # Helpers for systemd service management
```

### LifecycleHook Trait
```rust
pub trait LifecycleHook: Send + Sync {
    fn install(&self) -> Result<()>;     // Called during kata-deploy install
    fn uninstall(&self) -> Result<()>;   // Called during cleanup
    fn run_now(&self) -> Result<()>;     // Run immediately for initial setup
}
```

### InitrdRebuildHook
- Installs systemd oneshot service at `/etc/systemd/system/kata-initrd-rebuild.service`
- Service runs before container runtime on boot
- Checks if kernel version changed (cached in /opt/kata/share/.kernel-version)
- If changed: runs dracut with kata-agent, updates kernel symlink
- Benefits: Survives reboots, catches kernel updates automatically

### QEMU Requirement
Host may not include QEMU. Options:
1. Use platform-specific extension/package
2. Extract from package and copy to host
3. Bundle QEMU in kata-deploy image (increases image size)
4. Use host QEMU if available from virtualization layer

## Current Status
- **QEMU**: Bundled in image with wrapper script (sets LD_LIBRARY_PATH for libpixman)
- **virtiofsd**: Bundled in image
- **SELinux**: Automated context setting (bin_t) via chcon in install.rs
- **Kernel symlink**: Automated creation in install.rs
- **Initrd**: Built at image build time using osbuilder, not yet implementing runtime rebuild hook

## Remaining Work
- Initrd rebuild hook not yet implemented (for automatic rebuild on host kernel updates)
- Testing QEMU wrapper with bundled libraries

## Update Policy
This file must be updated whenever the mission changes.
