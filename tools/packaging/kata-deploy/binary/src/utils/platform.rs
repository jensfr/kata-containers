// Copyright (c) 2025 Red Hat, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Platform detection and configuration for kata-deploy.
//!
//! This module provides platform-aware defaults for host component paths,
//! allowing kata-deploy to use host-provided components (QEMU, virtiofsd, kernel)
//! on platforms like RHCOS and RHEL, while falling back to bundled components
//! on generic Linux distributions.

use anyhow::Result;
use std::env;
use std::path::Path;

/// Supported platforms for kata-deploy
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Platform {
    /// Red Hat Enterprise Linux CoreOS (OpenShift nodes)
    Rhcos,
    /// Red Hat Enterprise Linux 9/10
    Rhel,
    /// Generic Linux (uses bundled components)
    Generic,
}

impl Platform {
    /// Detect the current platform from the environment or /etc/os-release
    pub fn detect() -> Self {
        // Check for explicit override via environment variable
        if let Ok(platform) = env::var("PLATFORM") {
            return Self::from_env_value(&platform);
        }

        // Auto-detect from /etc/os-release
        Self::detect_from_os_release("/etc/os-release")
            .unwrap_or(Platform::Generic)
    }

    /// Parse platform from environment variable value
    fn from_env_value(value: &str) -> Self {
        match value.to_lowercase().as_str() {
            "rhcos" | "coreos" => Platform::Rhcos,
            "rhel" => Platform::Rhel,
            _ => Platform::Generic,
        }
    }

    /// Detect platform from os-release file content
    fn detect_from_os_release<P: AsRef<Path>>(path: P) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        Self::parse_os_release(&content)
    }

    /// Parse os-release content to determine platform
    fn parse_os_release(content: &str) -> Option<Self> {
        // Check for RHCOS (Red Hat Enterprise Linux CoreOS)
        if content.contains("CoreOS") && content.contains("Red Hat") {
            return Some(Platform::Rhcos);
        }

        // Check for RHCOS by ID
        if content.lines().any(|line| {
            line.starts_with("ID=") && (line.contains("rhcos") || line.contains("coreos"))
        }) {
            return Some(Platform::Rhcos);
        }

        // Check for RHEL
        if content.contains("Red Hat Enterprise Linux") {
            return Some(Platform::Rhel);
        }

        // Check for RHEL by ID
        if content.lines().any(|line| line.starts_with("ID=") && line.contains("rhel")) {
            return Some(Platform::Rhel);
        }

        None
    }

    /// Check if this platform uses host components (QEMU, virtiofsd, kernel)
    pub fn uses_host_components(&self) -> bool {
        matches!(self, Platform::Rhcos | Platform::Rhel)
    }
}

/// Platform-aware path configuration
#[derive(Debug, Clone)]
pub struct PlatformPaths {
    platform: Platform,
    dest_dir: String,
}

impl PlatformPaths {
    /// Create a new PlatformPaths with the given platform and destination directory
    pub fn new(platform: Platform, dest_dir: &str) -> Self {
        Self {
            platform,
            dest_dir: dest_dir.to_string(),
        }
    }

    /// Get the QEMU binary path
    ///
    /// Returns the path specified by QEMU_PATH env var, or platform default:
    /// - RHCOS/RHEL: /usr/libexec/qemu-kvm
    /// - Generic: {dest_dir}/bin/qemu-system-{arch}
    pub fn qemu_path(&self) -> String {
        env::var("QEMU_PATH").unwrap_or_else(|_| self.default_qemu_path())
    }

    fn default_qemu_path(&self) -> String {
        match self.platform {
            Platform::Rhcos | Platform::Rhel => "/usr/libexec/qemu-kvm".to_string(),
            Platform::Generic => {
                let arch = std::env::consts::ARCH;
                let qemu_arch = match arch {
                    "x86_64" => "x86_64",
                    "aarch64" => "aarch64",
                    "s390x" => "s390x",
                    "powerpc64" => "ppc64",
                    _ => arch,
                };
                format!("{}/bin/qemu-system-{}", self.dest_dir, qemu_arch)
            }
        }
    }

    /// Get the virtiofsd binary path
    ///
    /// Returns the path specified by VIRTIOFSD_PATH env var, or platform default:
    /// - RHCOS/RHEL: /usr/libexec/virtiofsd
    /// - Generic: {dest_dir}/libexec/virtiofsd
    pub fn virtiofsd_path(&self) -> String {
        env::var("VIRTIOFSD_PATH").unwrap_or_else(|_| self.default_virtiofsd_path())
    }

    fn default_virtiofsd_path(&self) -> String {
        match self.platform {
            Platform::Rhcos | Platform::Rhel => "/usr/libexec/virtiofsd".to_string(),
            Platform::Generic => format!("{}/libexec/virtiofsd", self.dest_dir),
        }
    }

    /// Get the kernel path
    ///
    /// Returns the path specified by KERNEL_PATH env var, or platform default:
    /// - RHCOS/RHEL: /lib/modules (use host kernel)
    /// - Generic: {dest_dir}/share/kata-containers/vmlinuz.container
    pub fn kernel_path(&self) -> String {
        env::var("KERNEL_PATH").unwrap_or_else(|_| self.default_kernel_path())
    }

    fn default_kernel_path(&self) -> String {
        match self.platform {
            Platform::Rhcos | Platform::Rhel => "/lib/modules".to_string(),
            Platform::Generic => {
                format!("{}/share/kata-containers/vmlinuz.container", self.dest_dir)
            }
        }
    }

    /// Check if initrd should be built at runtime
    ///
    /// Returns the value specified by BUILD_INITRD env var, or:
    /// - RHCOS/RHEL (non-CC): true (build with host kernel modules)
    /// - RHCOS/RHEL (CC): false (use pre-built measured initrd)
    /// - Generic: false (use pre-built initrd)
    pub fn should_build_initrd(&self) -> bool {
        if let Ok(value) = env::var("BUILD_INITRD") {
            return value == "true" || value == "1";
        }

        // Default: build at runtime for host-component platforms (non-CC)
        // CC workloads should set BUILD_INITRD=false explicitly
        self.platform.uses_host_components()
    }

    /// Get the initrd path (for pre-built initrd)
    pub fn initrd_path(&self) -> String {
        env::var("INITRD_PATH").unwrap_or_else(|_| {
            format!("{}/share/kata-containers/kata-initrd.img", self.dest_dir)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================
    // FAILING TESTS - TDD Phase 1
    // These tests define the expected behavior for platform detection
    // and path configuration. They should fail initially.
    // ============================================================

    mod platform_detection {
        use super::*;

        #[test]
        fn test_detect_rhcos_from_os_release() {
            let os_release = r#"
NAME="Red Hat Enterprise Linux CoreOS"
VERSION="414.92.202401031435-0"
ID="rhcos"
ID_LIKE="rhel fedora"
VERSION_ID="4.14"
PLATFORM_ID="platform:el9"
PRETTY_NAME="Red Hat Enterprise Linux CoreOS 414.92.202401031435-0 (Plow)"
ANSI_COLOR="0;31"
CPE_NAME="cpe:/o:redhat:enterprise_linux:9::coreos"
HOME_URL="https://www.redhat.com/"
DOCUMENTATION_URL="https://docs.openshift.com/container-platform/4.14/"
BUG_REPORT_URL="https://bugzilla.redhat.com/"
REDHAT_BUGZILLA_PRODUCT="OpenShift Container Platform"
REDHAT_BUGZILLA_PRODUCT_VERSION="4.14"
REDHAT_SUPPORT_PRODUCT="OpenShift Container Platform"
REDHAT_SUPPORT_PRODUCT_VERSION="4.14"
OPENSHIFT_VERSION="4.14"
RHEL_VERSION="9.2"
OSTREE_VERSION='414.92.202401031435-0'
"#;
            let platform = Platform::parse_os_release(os_release);
            assert_eq!(platform, Some(Platform::Rhcos));
        }

        #[test]
        fn test_detect_rhel9_from_os_release() {
            let os_release = r#"
NAME="Red Hat Enterprise Linux"
VERSION="9.3 (Plow)"
ID="rhel"
ID_LIKE="fedora"
VERSION_ID="9.3"
PLATFORM_ID="platform:el9"
PRETTY_NAME="Red Hat Enterprise Linux 9.3 (Plow)"
ANSI_COLOR="0;31"
LOGO="fedora-logo-icon"
CPE_NAME="cpe:/o:redhat:enterprise_linux:9::baseos"
HOME_URL="https://www.redhat.com/"
DOCUMENTATION_URL="https://access.redhat.com/documentation/en-us/red_hat_enterprise_linux/9"
BUG_REPORT_URL="https://bugzilla.redhat.com/"
"#;
            let platform = Platform::parse_os_release(os_release);
            assert_eq!(platform, Some(Platform::Rhel));
        }

        #[test]
        fn test_detect_rhel10_from_os_release() {
            let os_release = r#"
NAME="Red Hat Enterprise Linux"
VERSION="10.0 (Coughlan)"
ID="rhel"
ID_LIKE="fedora"
VERSION_ID="10.0"
PLATFORM_ID="platform:el10"
PRETTY_NAME="Red Hat Enterprise Linux 10.0 (Coughlan)"
"#;
            let platform = Platform::parse_os_release(os_release);
            assert_eq!(platform, Some(Platform::Rhel));
        }

        #[test]
        fn test_detect_generic_from_ubuntu() {
            let os_release = r#"
PRETTY_NAME="Ubuntu 22.04.3 LTS"
NAME="Ubuntu"
VERSION_ID="22.04"
VERSION="22.04.3 LTS (Jammy Jellyfish)"
VERSION_CODENAME=jammy
ID=ubuntu
ID_LIKE=debian
HOME_URL="https://www.ubuntu.com/"
SUPPORT_URL="https://help.ubuntu.com/"
BUG_REPORT_URL="https://bugs.launchpad.net/ubuntu/"
"#;
            let platform = Platform::parse_os_release(os_release);
            assert_eq!(platform, None); // Generic
        }

        #[test]
        fn test_platform_from_env_rhcos() {
            let platform = Platform::from_env_value("rhcos");
            assert_eq!(platform, Platform::Rhcos);
        }

        #[test]
        fn test_platform_from_env_rhcos_uppercase() {
            let platform = Platform::from_env_value("RHCOS");
            assert_eq!(platform, Platform::Rhcos);
        }

        #[test]
        fn test_platform_from_env_coreos() {
            let platform = Platform::from_env_value("coreos");
            assert_eq!(platform, Platform::Rhcos);
        }

        #[test]
        fn test_platform_from_env_rhel() {
            let platform = Platform::from_env_value("rhel");
            assert_eq!(platform, Platform::Rhel);
        }

        #[test]
        fn test_platform_from_env_unknown() {
            let platform = Platform::from_env_value("unknown");
            assert_eq!(platform, Platform::Generic);
        }

        #[test]
        fn test_uses_host_components_rhcos() {
            assert!(Platform::Rhcos.uses_host_components());
        }

        #[test]
        fn test_uses_host_components_rhel() {
            assert!(Platform::Rhel.uses_host_components());
        }

        #[test]
        fn test_uses_host_components_generic() {
            assert!(!Platform::Generic.uses_host_components());
        }
    }

    mod platform_paths {
        use super::*;

        fn cleanup_env() {
            std::env::remove_var("QEMU_PATH");
            std::env::remove_var("VIRTIOFSD_PATH");
            std::env::remove_var("KERNEL_PATH");
            std::env::remove_var("BUILD_INITRD");
            std::env::remove_var("INITRD_PATH");
        }

        #[test]
        fn test_qemu_path_rhcos_default() {
            cleanup_env();
            let paths = PlatformPaths::new(Platform::Rhcos, "/opt/kata");
            assert_eq!(paths.qemu_path(), "/usr/libexec/qemu-kvm");
        }

        #[test]
        fn test_qemu_path_rhel_default() {
            cleanup_env();
            let paths = PlatformPaths::new(Platform::Rhel, "/opt/kata");
            assert_eq!(paths.qemu_path(), "/usr/libexec/qemu-kvm");
        }

        #[test]
        fn test_qemu_path_generic_default() {
            cleanup_env();
            let paths = PlatformPaths::new(Platform::Generic, "/opt/kata");
            // Should use bundled QEMU
            assert!(paths.qemu_path().starts_with("/opt/kata/bin/qemu-system-"));
        }

        #[test]
        fn test_qemu_path_env_override() {
            cleanup_env();
            std::env::set_var("QEMU_PATH", "/custom/qemu");
            let paths = PlatformPaths::new(Platform::Rhcos, "/opt/kata");
            assert_eq!(paths.qemu_path(), "/custom/qemu");
            cleanup_env();
        }

        #[test]
        fn test_virtiofsd_path_rhcos_default() {
            cleanup_env();
            let paths = PlatformPaths::new(Platform::Rhcos, "/opt/kata");
            assert_eq!(paths.virtiofsd_path(), "/usr/libexec/virtiofsd");
        }

        #[test]
        fn test_virtiofsd_path_generic_default() {
            cleanup_env();
            let paths = PlatformPaths::new(Platform::Generic, "/opt/kata");
            assert_eq!(paths.virtiofsd_path(), "/opt/kata/libexec/virtiofsd");
        }

        #[test]
        fn test_virtiofsd_path_env_override() {
            cleanup_env();
            std::env::set_var("VIRTIOFSD_PATH", "/custom/virtiofsd");
            let paths = PlatformPaths::new(Platform::Rhcos, "/opt/kata");
            assert_eq!(paths.virtiofsd_path(), "/custom/virtiofsd");
            cleanup_env();
        }

        #[test]
        fn test_kernel_path_rhcos_default() {
            cleanup_env();
            let paths = PlatformPaths::new(Platform::Rhcos, "/opt/kata");
            assert_eq!(paths.kernel_path(), "/lib/modules");
        }

        #[test]
        fn test_kernel_path_generic_default() {
            cleanup_env();
            let paths = PlatformPaths::new(Platform::Generic, "/opt/kata");
            assert_eq!(
                paths.kernel_path(),
                "/opt/kata/share/kata-containers/vmlinuz.container"
            );
        }

        #[test]
        fn test_should_build_initrd_rhcos_default() {
            cleanup_env();
            let paths = PlatformPaths::new(Platform::Rhcos, "/opt/kata");
            // Default for RHCOS: build initrd at runtime with host kernel
            assert!(paths.should_build_initrd());
        }

        #[test]
        fn test_should_build_initrd_generic_default() {
            cleanup_env();
            let paths = PlatformPaths::new(Platform::Generic, "/opt/kata");
            // Default for generic: use pre-built initrd
            assert!(!paths.should_build_initrd());
        }

        #[test]
        fn test_should_build_initrd_env_override_false() {
            cleanup_env();
            std::env::set_var("BUILD_INITRD", "false");
            let paths = PlatformPaths::new(Platform::Rhcos, "/opt/kata");
            // Env override for CC workloads
            assert!(!paths.should_build_initrd());
            cleanup_env();
        }

        #[test]
        fn test_should_build_initrd_env_override_true() {
            cleanup_env();
            std::env::set_var("BUILD_INITRD", "true");
            let paths = PlatformPaths::new(Platform::Generic, "/opt/kata");
            assert!(paths.should_build_initrd());
            cleanup_env();
        }

        #[test]
        fn test_initrd_path_default() {
            cleanup_env();
            let paths = PlatformPaths::new(Platform::Rhcos, "/opt/kata");
            assert_eq!(
                paths.initrd_path(),
                "/opt/kata/share/kata-containers/kata-initrd.img"
            );
        }

        #[test]
        fn test_initrd_path_env_override() {
            cleanup_env();
            std::env::set_var("INITRD_PATH", "/custom/initrd.img");
            let paths = PlatformPaths::new(Platform::Rhcos, "/opt/kata");
            assert_eq!(paths.initrd_path(), "/custom/initrd.img");
            cleanup_env();
        }
    }
}
