# Changes by Component for PR Preparation

This document organizes all changes made for DaemonSet/HCP support by component, for preparing separate pull requests.

## Summary of Components

| Component | Repo | PR Type | Status |
|-----------|------|---------|--------|
| kata-deploy Rust binary | kata-containers | Upstream | Ready for review |
| Platform detection module | kata-containers | Upstream | Ready for review |
| Dockerfile.full | kata-containers | Downstream (RH) | Ready for review |
| Configuration files | kata-containers | Downstream (RH) | Ready for review |
| SELinux policy | kata-containers | Downstream (RH) | Ready for review |

---

## Component 1: Platform Detection Module (Upstream)

**Files:**
- `tools/packaging/kata-deploy/binary/src/utils/platform.rs` (NEW)
- `tools/packaging/kata-deploy/binary/src/utils/mod.rs` (MODIFIED)

**Purpose:** Platform-aware path configuration for QEMU, virtiofsd, kernel paths.

**Changes:**

### platform.rs (NEW - 465 lines)
```rust
// Key types:
pub enum Platform { Rhcos, Rhel, Generic }

pub struct PlatformPaths {
    // Provides platform-aware defaults:
    // - qemu_path(): /usr/libexec/qemu-kvm (RHCOS) vs bundled (Generic)
    // - virtiofsd_path(): /usr/libexec/virtiofsd (RHCOS) vs bundled (Generic)
    // - should_build_initrd(): true (RHCOS) vs false (Generic)
}

// Auto-detection from /etc/os-release
// Environment variable overrides (PLATFORM, QEMU_PATH, etc.)
// Comprehensive test suite
```

### mod.rs (MODIFIED - 1 line)
```rust
pub mod platform;  // Add this line
```

**Why Upstream:** This is a generic abstraction that benefits all users. Other distros can extend `Platform` enum.

---

## Component 2: Install/Uninstall Operations (Upstream with Feature Flag)

**Files:**
- `tools/packaging/kata-deploy/binary/src/artifacts/install.rs` (MODIFIED)

**Purpose:** Platform-specific operations during install/uninstall.

**Changes (behind `#[cfg(feature = "rhcos")]`):**

### New Functions (~450 lines)
```rust
// RHCOS-specific operations (feature-gated):

fn install_qemu_via_rpm_ostree() -> Result<()>
// Copies RPMs to /tmp, runs rpm-ostree install --apply-live via nsenter

fn install_selinux_policy(host_install_dir: &str) -> Result<()>
// Installs osc_monitor.cil policy, sets container_use_devices boolean

fn set_selinux_contexts(dir: &str) -> Result<()>
// Sets bin_t on executables, lib_t on libraries

fn build_initrd_with_modules(dir: &str) -> Result<()>
// Runs kata-osbuilder.sh via nsenter on host

fn uninstall_qemu_via_rpm_ostree() -> Result<()>
fn uninstall_selinux_policy() -> Result<()>
```

### Integration Points (~20 lines)
```rust
// In install_artifacts():
set_selinux_contexts(&config.host_install_dir)?;
build_initrd_with_modules(&config.host_install_dir)?;
install_qemu_via_rpm_ostree()?;
install_selinux_policy(&config.host_install_dir)?;

// In remove_artifacts():
uninstall_selinux_policy()?;
uninstall_qemu_via_rpm_ostree()?;
```

**Why Upstream:** The trait-based design allows other distros to add their own platform support.

---

## Component 3: Dockerfile.full (Red Hat Downstream)

**Files:**
- `tools/packaging/kata-deploy/Dockerfile.full` (NEW)

**Purpose:** Multi-stage build that:
1. Builds kata-deploy Rust binary from source
2. Extracts shim, agent, osbuilder from kata-containers RPM (version 3.17.0)
3. Collects QEMU RPMs from RHCOS extension container
4. Packages everything for DaemonSet deployment

**Key Sections:**
```dockerfile
# Stage 1: Build Rust binary
FROM rust:1.75 AS rust-builder
WORKDIR /kata
COPY . .
RUN cd tools/packaging/kata-deploy/binary && cargo build --release --features rhcos

# Stage 2: Extract from kata-containers RPM
FROM registry.redhat.io/rhcos-4-for-x86_64/extensions-rhel9:latest AS extensions-source
FROM centos:stream9 AS rpm-collector
COPY --from=extensions-source /rpms/qemu*.rpm /rpms/
RUN rpm2cpio /rpms/kata-containers-*.rpm | cpio -idmv

# Stage 3: Runtime image
FROM registry.access.redhat.com/ubi9/ubi-minimal
COPY --from=rust-builder /kata/target/release/kata-deploy /usr/bin/
COPY --from=rpm-collector /kata-osbuilder/ /opt/kata-artifacts/
COPY --from=rpm-collector /rpms/ /opt/kata-artifacts/rpms/
```

**Why Downstream:** Uses Red Hat specific base images and RPMs.

---

## Component 4: Configuration Files (Red Hat Downstream)

**Files:**
- `tools/packaging/kata-deploy/configuration-qemu.toml` (NEW)
- `tools/packaging/kata-deploy/configuration-qemu-tdx.toml` (NEW)
- `tools/packaging/kata-deploy/configuration-qemu-snp.toml` (NEW)
- `tools/packaging/kata-deploy/configuration-qemu-se.toml` (NEW)

**Purpose:** Kata configuration optimized for DaemonSet mode.

**Key Settings (configuration-qemu.toml):**
```toml
[hypervisor.qemu]
path = "/usr/libexec/qemu-kvm"                    # Host QEMU
kernel = "/var/cache/kata-containers/vmlinuz.container"  # Host kernel
initrd = "/var/cache/kata-containers/kata-containers-initrd.img"  # Runtime-built
disable_guest_selinux = true                      # Critical for RHCOS

[agent.kata]
dial_timeout = 45                                 # Increased for osbuilder

[runtime]
create_container_timeout = 60                     # Increased for first boot
```

**Why Downstream:** Paths and timeouts are RHCOS-specific.

---

## Component 5: SELinux Policy (Red Hat Downstream)

**Files:**
- `tools/packaging/kata-deploy/osc_monitor.cil` (NEW)

**Purpose:** Grant QEMU access to QMP sockets in `/run/vc/`.

**Content:**
```cil
; SELinux policy for kata-monitor and QEMU access to /run/vc/
(typeattributeset cil_gen_require container_kvm_t)
(typeattributeset cil_gen_require container_var_run_t)
(allow container_kvm_t container_var_run_t (dir (search)))
(allow container_kvm_t container_var_run_t (sock_file (write)))
```

**Why Downstream:** RHCOS-specific SELinux contexts.

---

## PR Strategy

### PR 1: Platform Detection (Upstream)
- `src/utils/platform.rs`
- `src/utils/mod.rs`
- Tests included
- No feature flag needed (always available)

### PR 2: Platform Backend Trait (Upstream)
- Define `PlatformBackend` trait
- `GenericPlatform` default implementation
- Integration points in `install.rs`
- Feature flag: none (Generic is default)

### PR 3: RHCOS Backend (Upstream with Feature)
- RHCOS-specific functions
- Feature flag: `rhcos`
- Depends on PR 1 and PR 2

### PR 4: Downstream Build (Red Hat Only)
- `Dockerfile.full`
- Configuration files
- SELinux policy
- Internal RH repo only

---

## Testing

### Unit Tests
```bash
cd tools/packaging/kata-deploy/binary
cargo test
# All 27 platform tests pass
```

### Integration Tests (RHCOS)
```bash
# Deploy DaemonSet
oc apply -f kata-deploy-daemonset.yaml

# Verify
oc logs -n openshift-sandboxed-containers-operator -l app=kata-deploy

# Test kata pod
oc run kata-test --image=ubi9/ubi-minimal:latest \
  --overrides='{"spec":{"runtimeClassName":"kata-qemu"}}' \
  -- sleep 300
oc exec kata-test -- uname -r  # Should show host kernel
```

---

## Version Compatibility

| Component | Version | Source |
|-----------|---------|--------|
| containerd-shim-kata-v2 | 3.17.0 | kata-containers RPM |
| kata-agent | 3.17.0 | kata-containers RPM |
| kata-osbuilder | 3.17.0 | kata-containers RPM |
| QEMU | 8.2.x | RHCOS extension RPMs |
| virtiofsd | 1.10.x | RHCOS extension RPMs |

**Critical:** Shim and agent versions MUST match (3.17.0).
