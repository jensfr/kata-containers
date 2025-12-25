# Upstream Design: Platform-Aware kata-deploy

This document describes the proposed upstream changes to support platform-specific operations (RHCOS/HCP DaemonSet mode) with minimal changes that don't break existing functionality.

## Research Summary

### Industry Best Practices for Plugin/Extension Architectures

| Approach | Pros | Cons | Used By |
|----------|------|------|---------|
| **Trait-based + Feature Flags** | Compile-time safety, no ABI issues, KISS | No runtime plugins | Trustee, containerd |
| **NRI (Node Resource Interface)** | Runtime flexibility, k8s-native | Complex, heavyweight | containerd 2.0, CRI-O |
| **OCI Hooks** | Standard, runtime-agnostic | Performance overhead, limited scope | CRI-O, Podman |
| **Hook Scripts** | Simple, no code changes | Shell execution overhead, less safe | Git, many tools |
| **Dynamic Libraries** | Runtime plugins | ABI instability in Rust | Bevy (with caveats) |

### Recommended: Trait-Based + Feature Flags (Trustee Pattern)

Based on our research, the **Trustee pattern** is the best fit for kata-deploy because:

1. **KISS Principle**: No runtime plugin loading, no ABI issues
2. **Compile-time Safety**: Type-checked platform implementations
3. **Proven Pattern**: Successfully used in confidential-containers/trustee
4. **Minimal Change**: ~100 lines of upstream code addition
5. **No Breakage**: Generic platform is default, existing functionality unchanged

## Proposed Architecture

### Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                      kata-deploy binary                          │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │  PlatformBackend trait (interface)                          │ │
│  │  - pre_install() -> Result<()>                              │ │
│  │  - post_install() -> Result<()>                             │ │
│  │  - pre_uninstall() -> Result<()>                            │ │
│  │  - post_uninstall() -> Result<()>                           │ │
│  │  - get_component_paths() -> PlatformPaths                   │ │
│  └─────────────────────────────────────────────────────────────┘ │
│                              │                                   │
│         ┌────────────────────┼────────────────────┐              │
│         ▼                    ▼                    ▼              │
│  ┌────────────┐      ┌────────────┐       ┌────────────┐        │
│  │  Generic   │      │   Rhcos    │       │   Rhel     │        │
│  │  (default) │      │  (feature) │       │  (feature) │        │
│  └────────────┘      └────────────┘       └────────────┘        │
│  - Uses bundled      - rpm-ostree QEMU    - rpm-based QEMU      │
│    components        - kata-osbuilder     - kata-osbuilder      │
│  - Pre-built initrd  - Runtime initrd     - Runtime initrd      │
│  - No SELinux setup  - SELinux policy     - SELinux policy      │
└─────────────────────────────────────────────────────────────────┘
```

### Trait Definition (~30 lines)

```rust
// src/platform/mod.rs

use anyhow::Result;

/// Platform backend trait for platform-specific operations.
///
/// Implementations provide platform-specific hooks that run during
/// kata-deploy install/uninstall. The default (Generic) implementation
/// does nothing, preserving existing behavior.
#[async_trait::async_trait]
pub trait PlatformBackend: Send + Sync {
    /// Called before copying artifacts. Use for pre-requisites.
    fn pre_install(&self, _config: &Config) -> Result<()> { Ok(()) }

    /// Called after copying artifacts. Use for runtime setup.
    fn post_install(&self, _config: &Config) -> Result<()> { Ok(()) }

    /// Called before removing artifacts.
    fn pre_uninstall(&self, _config: &Config) -> Result<()> { Ok(()) }

    /// Called after removing artifacts. Use for cleanup.
    fn post_uninstall(&self, _config: &Config) -> Result<()> { Ok(()) }

    /// Get platform-specific component paths.
    fn get_paths(&self, dest_dir: &str) -> PlatformPaths;
}

/// Generic platform - uses bundled components, no special setup.
pub struct GenericPlatform;

impl PlatformBackend for GenericPlatform {
    fn get_paths(&self, dest_dir: &str) -> PlatformPaths {
        PlatformPaths::new(Platform::Generic, dest_dir)
    }
}
```

### RHCOS Implementation (~100 lines, behind feature flag)

```rust
// src/platform/rhcos.rs (only compiled with `rhcos` feature)

#[cfg(feature = "rhcos")]
pub struct RhcosPlatform;

#[cfg(feature = "rhcos")]
impl PlatformBackend for RhcosPlatform {
    fn post_install(&self, config: &Config) -> Result<()> {
        // 1. Set SELinux contexts on binaries
        set_selinux_contexts(&config.host_install_dir)?;

        // 2. Build initrd with host kernel modules
        build_initrd_with_modules(&config.host_install_dir)?;

        // 3. Install QEMU via rpm-ostree
        install_qemu_via_rpm_ostree()?;

        // 4. Install SELinux policy
        install_selinux_policy(&config.host_install_dir)?;

        Ok(())
    }

    fn post_uninstall(&self, _config: &Config) -> Result<()> {
        uninstall_selinux_policy()?;
        uninstall_qemu_via_rpm_ostree()?;
        Ok(())
    }

    fn get_paths(&self, dest_dir: &str) -> PlatformPaths {
        PlatformPaths::new(Platform::Rhcos, dest_dir)
    }
}
```

### Feature Flags in Cargo.toml

```toml
[features]
default = []
rhcos = []  # Enable RHCOS/HCP DaemonSet support
rhel = []   # Enable RHEL support (shares code with rhcos)
```

### Integration Points in install.rs (~10 lines change)

```rust
// In install_artifacts():
pub async fn install_artifacts(config: &Config) -> Result<()> {
    let platform = get_platform_backend();  // Auto-detect or from env

    platform.pre_install(config)?;          // NEW: platform hook

    copy_artifacts(&artifact_src, &config.host_install_dir)?;
    set_executable_permissions(&config.host_install_dir)?;

    platform.post_install(config)?;         // NEW: platform hook

    // ... existing shim configuration code unchanged ...
}
```

## Files Changed

### 1. Upstream Changes (Minimal, ~150 lines total)

| File | Change Type | Lines | Description |
|------|-------------|-------|-------------|
| `src/platform/mod.rs` | NEW | ~50 | Trait definition + Generic impl |
| `src/platform/rhcos.rs` | NEW (feature) | ~100 | RHCOS-specific operations |
| `src/artifacts/install.rs` | MODIFIED | ~10 | Hook integration points |
| `Cargo.toml` | MODIFIED | ~5 | Feature flags |

### 2. Red Hat Specific (Downstream Only)

| File | Description |
|------|-------------|
| `Dockerfile.full` | Multi-stage build extracting from kata-containers RPM |
| `configuration-qemu.toml` | Config based on RPM with DaemonSet paths |
| `osc_monitor.cil` | SELinux policy for QEMU/QMP access |

## Build Configuration

### Upstream (Generic - default)

```bash
cargo build --release
# Uses bundled QEMU, pre-built initrd, no special setup
```

### Red Hat (RHCOS feature enabled)

```bash
cargo build --release --features rhcos
# Enables rpm-ostree, osbuilder, SELinux setup
```

## Environment Variables (All Platforms)

The design uses environment variables for runtime configuration, making it flexible:

| Variable | Default (Generic) | Default (RHCOS) | Description |
|----------|-------------------|-----------------|-------------|
| `PLATFORM` | auto-detect | auto-detect | Force platform: `rhcos`, `rhel`, `generic` |
| `QEMU_PATH` | `{dest}/bin/qemu-system-*` | `/usr/libexec/qemu-kvm` | QEMU binary path |
| `VIRTIOFSD_PATH` | `{dest}/libexec/virtiofsd` | `/usr/libexec/virtiofsd` | virtiofsd path |
| `BUILD_INITRD` | `false` | `true` | Build initrd at runtime |
| `KERNEL_PATH` | `{dest}/share/kata-containers/vmlinuz` | `/var/cache/kata-containers/vmlinuz.container` | Kernel path |

## Comparison with Alternatives

### Why Not OCI Hooks?

OCI hooks run at container start, not deployment time. We need deployment-time operations (rpm-ostree install, initrd building).

### Why Not NRI?

NRI is for modifying container configurations at runtime. Our needs are node setup, not container modification.

### Why Not Shell Script Hooks?

Shell scripts add:
- Runtime parsing overhead
- Shell injection risks
- Error handling complexity
- Testing difficulty

Trait-based approach is type-safe, tested, and compiled.

## Migration Path

1. **Phase 1**: Merge upstream changes with `rhcos` feature (disabled by default)
2. **Phase 2**: Red Hat builds with `--features rhcos`
3. **Phase 3**: Other distros can add their own platform implementations

## References

- [Trustee Plugin Architecture](https://github.com/confidential-containers/trustee/blob/main/kbs/src/plugins/)
- [NRI - Node Resource Interface](https://github.com/containerd/nri)
- [OCI Runtime Hooks](https://github.com/opencontainers/runtime-spec/blob/main/config.md#posix-platform-hooks)
- [Rust Plugin Patterns](https://nullderef.com/blog/plugin-tech/)
