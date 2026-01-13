// Copyright (c) 2025 Red Hat, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! rpm-ostree based installation functions for kata-deploy.
//!
//! This module contains functions for OSTree-based Linux distributions
//! (RHCOS, Fedora CoreOS, RHEL with rpm-ostree), including:
//! - Extension image fetching from OCP release payloads
//! - QEMU/virtiofsd installation via rpm-ostree
//! - SELinux policy installation
//!
//! These functions are only used on rpm-ostree platforms and are no-ops on generic Linux.
//!
//! # RPM Image Sources
//!
//! The module supports multiple RPM image sources in order of priority:
//!
//! 1. **KATA_RPMS_IMAGE** (env var): Pre-built or pre-processed image containing RPMs in `/rpms/`.
//!    This can be:
//!    - Pushed by the OSC controller after extracting from cluster's extensions image
//!    - A user-provided pre-built image (useful for air-gapped environments or custom RPMs)
//!
//! 2. **EXTENSIONS_IMAGE** (env var): Raw RHCOS extensions image from the cluster release.
//!    RPMs are extracted from `/usr/share/rpm-ostree/extensions/`.
//!
//! 3. **Baked-in RPMs**: RPMs bundled in the kata-deploy image at `/opt/kata-artifacts/rpms`.
//!    This is a fallback and typically empty in production (RPMs fetched at runtime).
//!
//! # Authentication
//!
//! For fetching images from authenticated registries, the module supports:
//!
//! - **AUTHFILE** (env var): Path to a Docker auth file (e.g., `/run/secrets/pull-secret.json`)
//! - **Service Account token**: Automatically used for internal OpenShift registry
//! - **Pull secret mount**: Common cluster pull secret locations

use anyhow::{Context, Result};
use log::info;
use std::fs;
use std::path::Path;
use std::process::Command;
use walkdir::WalkDir;

/// Fetch RPMs from a container image at runtime.
/// This allows dynamic RPM selection for mixed-version clusters (HCP).
///
/// # Priority
///
/// 1. **KATA_RPMS_IMAGE**: Image containing RPMs in `/rpms/` directory.
///    This can be either:
///    - Pushed by the OSC controller after extracting from cluster's extensions image
///    - A user-provided pre-built image for air-gapped or custom deployments
///
/// 2. **EXTENSIONS_IMAGE**: Raw RHCOS extensions image from the cluster release.
///    RPMs are extracted from `/usr/share/rpm-ostree/extensions/`.
///
/// 3. **Baked-in RPMs**: RPMs bundled in the kata-deploy image at `/opt/kata-artifacts/rpms`.
///
/// # Pre-built RPM Image
///
/// Users can create their own RPM image for custom deployments:
///
/// ```dockerfile
/// FROM scratch
/// COPY *.rpm /rpms/
/// ```
///
/// Then set `KATA_RPMS_IMAGE=quay.io/myorg/kata-rpms:v1.0` in the DaemonSet.
/// For authenticated registries, also set `AUTHFILE=/run/secrets/pull-secret.json`
/// and mount the pull secret.
pub fn fetch_extensions_from_image() -> Result<()> {
    let rpm_dest_dir = Path::new("/opt/kata-artifacts/rpms");

    // First try KATA_RPMS_IMAGE - pre-processed or pre-built image with RPMs
    // This is the preferred path for production deployments
    if let Ok(kata_rpms_image) = std::env::var("KATA_RPMS_IMAGE") {
        if !kata_rpms_image.is_empty() {
            info!("KATA_RPMS_IMAGE={}, fetching RPMs from image", kata_rpms_image);
            return fetch_rpms_from_kata_rpms_image(&kata_rpms_image, rpm_dest_dir);
        }
    }

    // Fall back to EXTENSIONS_IMAGE - raw RHCOS extensions image
    let extensions_image = match std::env::var("EXTENSIONS_IMAGE") {
        Ok(img) if !img.is_empty() => img,
        _ => {
            log::debug!("Neither KATA_RPMS_IMAGE nor EXTENSIONS_IMAGE set, using baked-in RPMs");
            return Ok(());
        }
    };

    info!("Fetching extensions from image: {}", extensions_image);

    // Skip if RPMs already exist (baked in at build time)
    // Use next().is_some() instead of count() for efficiency
    if rpm_dest_dir.exists() {
        if let Ok(mut entries) = fs::read_dir(rpm_dest_dir) {
            if entries.next().is_some() {
                log::debug!("RPMs already present at {:?}, skipping fetch", rpm_dest_dir);
                return Ok(());
            }
        }
    }

    fs::create_dir_all(rpm_dest_dir)?;

    let temp_dir = Path::new("/tmp/extension-image");
    if temp_dir.exists() {
        fs::remove_dir_all(temp_dir)?;
    }
    fs::create_dir_all(temp_dir)?;

    // Try extraction methods in order of preference.
    // We try oc first (preferred in OpenShift), then fall back to skopeo.
    // Note: try_extract_with_* returns Ok(false) if the tool isn't available,
    // so we need to check the result and try the next method if needed.
    let mut extracted = false;

    match try_extract_with_oc(&extensions_image, temp_dir, rpm_dest_dir) {
        Ok(true) => {
            extracted = true;
        }
        Ok(false) => {
            log::info!("'oc' not available, trying skopeo");
        }
        Err(e) => {
            log::warn!("'oc image extract' failed: {}, falling back to skopeo", e);
        }
    }

    if !extracted {
        match try_extract_with_skopeo(&extensions_image, temp_dir, rpm_dest_dir) {
            Ok(true) => {
                extracted = true;
            }
            Ok(false) => {
                log::warn!("skopeo not available for extensions extraction");
            }
            Err(e) => {
                log::error!("skopeo extraction failed: {}", e);
            }
        }
    }

    fs::remove_dir_all(temp_dir).ok();

    if extracted {
        info!("Successfully fetched extensions from {}", extensions_image);
        Ok(())
    } else {
        anyhow::bail!(
            "EXTENSIONS_IMAGE set to '{}' but no extraction tool available (need 'oc' or 'skopeo')",
            extensions_image
        )
    }
}

/// Fetch RPMs from the controller's pre-processed kata-rpms image.
/// This image has a simple layout: /rpms/*.rpm
fn fetch_rpms_from_kata_rpms_image(image: &str, rpm_dest_dir: &Path) -> Result<()> {
    // Clear existing baked-in RPMs to use the extracted ones
    if rpm_dest_dir.exists() {
        fs::remove_dir_all(rpm_dest_dir)?;
    }
    fs::create_dir_all(rpm_dest_dir)?;

    let temp_dir = Path::new("/tmp/kata-rpms-image");
    if temp_dir.exists() {
        fs::remove_dir_all(temp_dir)?;
    }
    fs::create_dir_all(temp_dir)?;

    // Try 'oc image extract' first (preferred in OpenShift), fall back to skopeo
    let mut extracted = false;

    // Try oc first
    match try_extract_kata_rpms_with_oc(image, temp_dir, rpm_dest_dir) {
        Ok(true) => {
            extracted = true;
        }
        Ok(false) => {
            log::info!("'oc' not available, trying skopeo");
        }
        Err(e) => {
            log::warn!("'oc image extract' failed for kata-rpms: {}, trying skopeo", e);
        }
    }

    // Try skopeo if oc didn't work
    if !extracted {
        match try_extract_kata_rpms_with_skopeo(image, temp_dir, rpm_dest_dir) {
            Ok(true) => {
                extracted = true;
            }
            Ok(false) => {
                log::warn!("skopeo not available for kata-rpms extraction");
            }
            Err(e) => {
                log::error!("skopeo extraction failed: {}", e);
            }
        }
    }

    fs::remove_dir_all(temp_dir).ok();

    if extracted {
        // Count extracted RPMs
        let rpm_count = fs::read_dir(rpm_dest_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "rpm"))
            .count();
        info!("Successfully extracted {} RPMs from kata-rpms image", rpm_count);
        Ok(())
    } else {
        anyhow::bail!(
            "KATA_RPMS_IMAGE set to '{}' but extraction failed",
            image
        )
    }
}

/// Extract RPMs from kata-rpms image using 'oc image extract'
fn try_extract_kata_rpms_with_oc(image: &str, temp_dir: &Path, rpm_dest_dir: &Path) -> Result<bool> {
    let oc_path = Path::new("/usr/bin/oc");
    if !oc_path.exists() {
        return Ok(false);
    }

    info!("Using 'oc image extract' to fetch kata-rpms");

    // The kata-rpms image has RPMs in /rpms/ directory
    let output = Command::new(oc_path)
        .args([
            "image", "extract",
            image,
            "--path", &format!("/rpms:{}", temp_dir.display()),
            "--confirm",
            "--insecure",  // Internal registry may use self-signed certs
        ])
        .output()
        .context("Failed to run oc image extract")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("oc image extract failed: {}", stderr);
    }

    // Copy RPMs from temp dir to destination
    copy_rpms_from_dir(temp_dir, rpm_dest_dir)?;
    Ok(true)
}

/// Extract RPMs from kata-rpms image using skopeo.
///
/// This function supports multiple authentication methods:
/// 1. AUTHFILE env var: Explicit path to Docker auth file (for external registries)
/// 2. Service Account token: Auto-detected for internal OpenShift registry
/// 3. Common auth file locations in the container
fn try_extract_kata_rpms_with_skopeo(image: &str, temp_dir: &Path, rpm_dest_dir: &Path) -> Result<bool> {
    let skopeo_path = Path::new("/usr/bin/skopeo");
    if !skopeo_path.exists() {
        info!("skopeo not found at /usr/bin/skopeo");
        return Ok(false);
    }

    let oci_dir = temp_dir.join("oci");
    info!("Using skopeo to fetch kata-rpms to OCI layout from: {}", image);

    // Build skopeo args
    let src = format!("docker://{}", image);
    let dst = format!("oci:{}:latest", oci_dir.display());

    // Determine if this is an internal registry (likely self-signed certs)
    let is_internal_registry = image.starts_with("image-registry.openshift-image-registry")
        || image.contains(".svc:")
        || image.contains(".svc/");

    let mut args = vec!["copy"];
    if is_internal_registry {
        args.push("--src-tls-verify=false");
    }

    // Auth priority:
    // 1. AUTHFILE env var (user-provided, e.g., for pre-built images from external registries)
    // 2. SA token for internal registry
    // 3. Common auth file locations
    let authfile_env = std::env::var("AUTHFILE").ok();
    let sa_token_path = "/var/run/secrets/kubernetes.io/serviceaccount/token";
    let common_auth_paths = [
        "/var/run/secrets/kubernetes.io/dockerconfigjson/.dockerconfigjson",
        "/run/secrets/pull-secret.json",
        "/run/containers/0/auth.json",
        "/host/var/lib/kubelet/config.json",
    ];

    let creds: String;
    let mut auth_configured = false;

    // 1. Check for explicit AUTHFILE env var
    if let Some(ref authfile) = authfile_env {
        if Path::new(authfile).exists() {
            args.push("--authfile");
            args.push(authfile);
            info!("Using AUTHFILE={} for registry auth", authfile);
            auth_configured = true;
        } else {
            log::warn!("AUTHFILE={} specified but file does not exist", authfile);
        }
    }

    // 2. For internal registry, use SA token as credentials
    if !auth_configured && is_internal_registry && Path::new(sa_token_path).exists() {
        if let Ok(token) = fs::read_to_string(sa_token_path) {
            creds = format!("serviceaccount:{}", token.trim());
            args.push("--src-creds");
            args.push(&creds);
            info!("Using SA token for internal registry auth");
            auth_configured = true;
        }
    }

    // 3. Check common auth file locations
    if !auth_configured {
        for auth_path in &common_auth_paths {
            if Path::new(auth_path).exists() {
                args.push("--authfile");
                args.push(auth_path);
                info!("Using auth file at {} for registry auth", auth_path);
                auth_configured = true;
                break;
            }
        }
    }

    if !auth_configured {
        log::debug!("No auth configured, attempting anonymous access");
    }

    args.push(&src);
    args.push(&dst);

    let output = Command::new(skopeo_path)
        .args(&args)
        .output()
        .context("Failed to run skopeo copy")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("skopeo copy failed: {}", stderr);
    }

    // Extract blobs from OCI layout
    let blobs_dir = oci_dir.join("blobs/sha256");
    if !blobs_dir.exists() {
        anyhow::bail!("No blobs directory found in OCI layout");
    }

    for entry in fs::read_dir(&blobs_dir)? {
        let entry = entry?;
        let blob_path = entry.path();

        let extract_dir = temp_dir.join("extract");
        if extract_dir.exists() {
            fs::remove_dir_all(&extract_dir)?;
        }
        fs::create_dir_all(&extract_dir)?;

        // Try gzip-compressed first, then uncompressed
        let tar_result = Command::new("tar")
            .args(["-xzf", &blob_path.display().to_string(), "-C", &extract_dir.display().to_string()])
            .output();

        let success = match tar_result {
            Ok(output) if output.status.success() => true,
            _ => {
                Command::new("tar")
                    .args(["-xf", &blob_path.display().to_string(), "-C", &extract_dir.display().to_string()])
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
            }
        };

        if success {
            // The kata-rpms image has RPMs in /rpms/
            let rpms_path = extract_dir.join("rpms");
            if rpms_path.exists() {
                copy_rpms_from_dir(&rpms_path, rpm_dest_dir)?;
            }
        }

        fs::remove_dir_all(&extract_dir).ok();
    }

    Ok(true)
}

/// Try to extract extensions using 'oc image extract' (preferred in OpenShift environments)
fn try_extract_with_oc(image: &str, temp_dir: &Path, rpm_dest_dir: &Path) -> Result<bool> {
    let oc_path = Path::new("/usr/bin/oc");
    if !oc_path.exists() {
        return Ok(false);
    }

    info!("Using 'oc image extract' to fetch extensions");

    let output = Command::new(oc_path)
        .args([
            "image", "extract",
            image,
            "--path", &format!("/usr/share/rpm-ostree/extensions:{}", temp_dir.display()),
            "--confirm",
        ])
        .output()
        .context("Failed to run oc image extract")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("oc image extract failed: {}", stderr);
    }

    // Copy RPMs to destination
    copy_rpms_from_dir(temp_dir, rpm_dest_dir)?;
    Ok(true)
}

/// Try to extract extensions using skopeo + tar (fallback method)
fn try_extract_with_skopeo(image: &str, temp_dir: &Path, rpm_dest_dir: &Path) -> Result<bool> {
    let skopeo_path = Path::new("/usr/bin/skopeo");
    if !skopeo_path.exists() {
        return Ok(false);
    }

    let oci_dir = temp_dir.join("oci");
    info!("Using skopeo to fetch extensions to OCI layout");

    // Build skopeo args, including auth file if available
    let src = format!("docker://{}", image);
    let dst = format!("oci:{}:latest", oci_dir.display());
    let mut args = vec!["copy", &src, &dst];

    // Auth priority (same as try_extract_kata_rpms_with_skopeo for consistency):
    // 1. AUTHFILE env var (user-provided)
    // 2. Common auth file locations
    let authfile_env = std::env::var("AUTHFILE").ok();
    let common_auth_paths = [
        "/var/run/secrets/kubernetes.io/dockerconfigjson/.dockerconfigjson",
        "/run/secrets/cluster-pull-secret/.dockerconfigjson",
        "/run/secrets/pull-secret.json",
        "/run/containers/0/auth.json",
        "/host/var/lib/kubelet/config.json",
    ];

    let mut auth_configured = false;

    // 1. Check for explicit AUTHFILE env var
    if let Some(ref authfile) = authfile_env {
        if Path::new(authfile).exists() {
            args.push("--authfile");
            args.push(authfile);
            info!("Using AUTHFILE={} for registry auth", authfile);
            auth_configured = true;
        } else {
            log::warn!("AUTHFILE={} specified but file does not exist", authfile);
        }
    }

    // 2. Check common auth file locations
    if !auth_configured {
        for auth_path in &common_auth_paths {
            if Path::new(auth_path).exists() {
                args.push("--authfile");
                args.push(auth_path);
                info!("Using auth file at {} for registry auth", auth_path);
                auth_configured = true;
                break;
            }
        }
    }

    if !auth_configured {
        log::debug!("No auth configured for extensions image, attempting anonymous access");
    }

    let output = Command::new(skopeo_path)
        .args(&args)
        .output()
        .context("Failed to run skopeo copy")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("skopeo copy failed: {}", stderr);
    }

    // Extract blobs from OCI layout
    // The RPMs are in /usr/share/rpm-ostree/extensions within the image layers
    let blobs_dir = oci_dir.join("blobs/sha256");
    if !blobs_dir.exists() {
        anyhow::bail!("No blobs directory found in OCI layout");
    }

    for entry in fs::read_dir(&blobs_dir)? {
        let entry = entry?;
        let blob_path = entry.path();

        // Try to extract each blob as a tar (layer)
        // Note: We extract everything and filter for RPMs, avoiding GNU-specific --wildcards
        let extract_dir = temp_dir.join("extract");
        if extract_dir.exists() {
            fs::remove_dir_all(&extract_dir)?;
        }
        fs::create_dir_all(&extract_dir)?;

        // Try gzip-compressed first, then uncompressed
        let tar_result = Command::new("tar")
            .args(["-xzf", &blob_path.display().to_string(), "-C", &extract_dir.display().to_string()])
            .output();

        let success = match tar_result {
            Ok(output) if output.status.success() => true,
            _ => {
                // Try uncompressed tar
                Command::new("tar")
                    .args(["-xf", &blob_path.display().to_string(), "-C", &extract_dir.display().to_string()])
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
            }
        };

        if success {
            // Find and copy RPMs from rpm-ostree/extensions subdirectory
            let extensions_path = extract_dir.join("usr/share/rpm-ostree/extensions");
            if extensions_path.exists() {
                copy_rpms_from_dir(&extensions_path, rpm_dest_dir)?;
            }
        }

        fs::remove_dir_all(&extract_dir).ok();
    }

    Ok(true)
}

/// Copy all .rpm files from src_dir to dst_dir
fn copy_rpms_from_dir(src_dir: &Path, dst_dir: &Path) -> Result<()> {
    for entry in WalkDir::new(src_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "rpm"))
    {
        let dst_path = dst_dir.join(entry.file_name());
        if dst_path.exists() {
            log::debug!("Overwriting existing RPM: {:?}", dst_path);
        }
        fs::copy(entry.path(), &dst_path)?;
        log::debug!("Extracted {:?}", dst_path);
    }
    Ok(())
}

/// RPM name prefixes that we need for kata installation.
/// The extensions image contains many packages we don't need (kernels, fence agents, etc.).
/// We only install the specific packages required for kata to work.
const KATA_REQUIRED_RPM_PREFIXES: &[&str] = &[
    // QEMU packages
    "qemu-kvm-core",      // QEMU KVM core package
    "qemu-kvm-common",    // QEMU common files
    "qemu-img",           // QEMU disk image utility (required by qemu-kvm-core)
    // Virtiofs
    "virtiofsd",          // virtiofs daemon for shared filesystem
    // BIOS/firmware
    "seabios-bin",        // BIOS for QEMU
    "seavgabios-bin",     // VGA BIOS for QEMU
    "ipxe-roms-qemu",     // PXE boot ROMs for QEMU
    "edk2-ovmf",          // UEFI firmware for QEMU (needed for TDX/SNP)
    // Kata
    "kata-containers",    // Kata containers package (agent, osbuilder, shim)
    // QEMU dependencies
    "libpmem",            // Persistent memory library (QEMU dependency)
    "libfdt",             // Flattened device tree library (QEMU dependency)
    "librdmacm",          // RDMA library (QEMU dependency)
    "pixman",             // Pixel manipulation library (QEMU dependency)
    "capstone",           // Disassembler library (QEMU dependency)
    "libpng",             // PNG library (QEMU dependency)
    "ndctl-libs",         // NVMe management library (libpmem dependency)
    "daxctl-libs",        // DAX control library (libpmem dependency)
];

/// Check if an RPM filename matches one of our required prefixes
fn is_kata_required_rpm(filename: &str) -> bool {
    KATA_REQUIRED_RPM_PREFIXES.iter().any(|prefix| filename.starts_with(prefix))
}

/// Install QEMU and virtiofsd via rpm-ostree on RHCOS nodes.
/// This installs QEMU to proper system paths (/usr/libexec/qemu-kvm) where
/// it can find its BIOS files without needing wrapper scripts.
pub fn install_qemu_via_rpm_ostree() -> Result<()> {
    let rpm_source_dir = Path::new("/opt/kata-artifacts/rpms");
    if !rpm_source_dir.exists() {
        log::debug!("No RPMs directory found at {:?}, skipping rpm-ostree install", rpm_source_dir);
        return Ok(());
    }

    // Check if rpm-ostree is available (we're on RHCOS)
    let host_rpm_ostree = Path::new("/host/usr/bin/rpm-ostree");
    if !host_rpm_ostree.exists() {
        log::debug!("rpm-ostree not found at {:?}, skipping QEMU installation via rpm-ostree", host_rpm_ostree);
        return Ok(());
    }

    // Check if the kata shim is already installed (from kata-containers RPM)
    // This is the key binary we need - QEMU may already be on the system but
    // we still need to install kata-containers to get the shim.
    let kata_shim_path = Path::new("/host/usr/bin/containerd-shim-kata-v2");
    if kata_shim_path.exists() {
        info!("Kata shim already installed at /usr/bin/containerd-shim-kata-v2, skipping rpm-ostree install");
        return Ok(());
    }

    // Copy only the RPMs we need to host temp directory
    // The extensions image contains many packages (kernels, fence agents, etc.) that
    // are either already in the base or not needed for kata
    let host_rpm_dir = Path::new("/host/tmp/kata-rpms");
    if host_rpm_dir.exists() {
        fs::remove_dir_all(host_rpm_dir)?;
    }
    fs::create_dir_all(host_rpm_dir)?;

    let mut copied_count = 0;
    for entry in fs::read_dir(rpm_source_dir)? {
        let entry = entry?;
        let src_path = entry.path();
        let filename = entry.file_name().to_string_lossy().to_string();

        if src_path.extension().map_or(false, |ext| ext == "rpm") && is_kata_required_rpm(&filename) {
            let dst_path = host_rpm_dir.join(entry.file_name());
            fs::copy(&src_path, &dst_path)?;
            log::debug!("Copied {:?} to {:?}", src_path, dst_path);
            copied_count += 1;
        }
    }

    info!("Selected {} kata-required RPMs from {} total in extensions", copied_count,
          fs::read_dir(rpm_source_dir)?.count());

    // Collect RPM paths for installation (from host perspective)
    let rpm_files: Vec<String> = fs::read_dir(host_rpm_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "rpm"))
        .map(|e| format!("/tmp/kata-rpms/{}", e.file_name().to_string_lossy()))
        .collect();

    if rpm_files.is_empty() {
        log::warn!("No RPM files found in {:?}", host_rpm_dir);
        return Ok(());
    }

    info!("Installing QEMU via rpm-ostree: {:?}", rpm_files);

    // Run rpm-ostree install from the host using nsenter/chroot
    // We use nsenter to run in the host's mount namespace
    let mut args = vec![
        "-t".to_string(), "1".to_string(),    // Target PID 1 (host init)
        "-m".to_string(),                     // Enter mount namespace
        "/usr/bin/rpm-ostree".to_string(),
        "install".to_string(),
        "--idempotent".to_string(),           // Don't fail if already installed
        "--apply-live".to_string(),           // Apply changes immediately without reboot
    ];
    args.extend(rpm_files.clone());

    let status = Command::new("/host/usr/bin/nsenter")
        .args(&args)
        .status()
        .context("Failed to run rpm-ostree install")?;

    if !status.success() {
        // If apply-live fails, try without it (will require reboot)
        log::warn!("rpm-ostree install --apply-live failed, trying without apply-live");
        let mut args_without_live = vec![
            "-t".to_string(), "1".to_string(),
            "-m".to_string(),
            "/usr/bin/rpm-ostree".to_string(),
            "install".to_string(),
            "--idempotent".to_string(),
        ];
        args_without_live.extend(rpm_files);

        let status = Command::new("/host/usr/bin/nsenter")
            .args(&args_without_live)
            .status()
            .context("Failed to run rpm-ostree install without apply-live")?;

        if !status.success() {
            anyhow::bail!("rpm-ostree install failed with exit code: {:?}", status.code());
        }
        log::warn!("QEMU installed but node reboot required to activate");
    } else {
        info!("QEMU installed successfully via rpm-ostree with apply-live");
    }

    // Cleanup temp RPMs
    fs::remove_dir_all(host_rpm_dir).ok();

    Ok(())
}

/// Create symlinks for RPM-installed binaries to the expected kata paths.
/// The kata-containers RPM installs binaries to /usr/bin/, but CRI-O configs
/// expect them at /opt/kata/bin/. This creates symlinks to bridge the gap.
pub fn create_rpm_symlinks(host_install_dir: &str) -> Result<()> {
    // Map of (source in /host/usr/..., target in host_install_dir/...)
    let symlinks = [
        // Shim binary: RPM installs to /usr/bin, we need it at /opt/kata/bin
        ("/host/usr/bin/containerd-shim-kata-v2", "bin/containerd-shim-kata-v2"),
        // Also create link for kata-runtime if it exists
        ("/host/usr/bin/kata-runtime", "bin/kata-runtime"),
    ];

    let bin_dir = format!("{}/bin", host_install_dir);
    fs::create_dir_all(&bin_dir)?;

    for (src, dst_relative) in &symlinks {
        let src_path = Path::new(src);
        if !src_path.exists() {
            log::debug!("Source binary not found: {}, skipping symlink", src);
            continue;
        }

        let dst_path = format!("{}/{}", host_install_dir, dst_relative);
        let dst = Path::new(&dst_path);

        // Skip if destination already exists (might be the actual binary from artifacts)
        if dst.exists() {
            log::debug!("Destination already exists: {:?}, skipping symlink", dst);
            continue;
        }

        // Create parent directory if needed
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }

        // The symlink target should be the path from host perspective (without /host prefix)
        let target = src.strip_prefix("/host").unwrap_or(src);

        // Create symlink via nsenter on host to ensure it's in the right namespace
        let ln_status = Command::new("/host/usr/bin/nsenter")
            .args([
                "-t", "1", "-m",
                "ln", "-sf", target,
                dst_path.strip_prefix("/host").unwrap_or(&dst_path),
            ])
            .status();

        match ln_status {
            Ok(s) if s.success() => {
                info!("Created symlink: {} -> {}", dst_path, target);
            }
            Ok(s) => {
                log::warn!("Failed to create symlink {} -> {}: exit code {:?}",
                          dst_path, target, s.code());
            }
            Err(e) => {
                log::warn!("Failed to run ln for symlink: {}", e);
            }
        }
    }

    Ok(())
}

/// Uninstall QEMU and virtiofsd via rpm-ostree.
pub fn uninstall_qemu_via_rpm_ostree() -> Result<()> {
    // Check if rpm-ostree is available
    let host_rpm_ostree = Path::new("/host/usr/bin/rpm-ostree");
    if !host_rpm_ostree.exists() {
        log::debug!("rpm-ostree not found, skipping QEMU uninstallation");
        return Ok(());
    }

    // Check if QEMU is installed
    let qemu_path = Path::new("/host/usr/libexec/qemu-kvm");
    if !qemu_path.exists() {
        log::debug!("QEMU not installed, skipping uninstallation");
        return Ok(());
    }

    info!("Uninstalling QEMU via rpm-ostree");

    // Uninstall QEMU packages using nsenter
    let packages = ["qemu-kvm-core", "virtiofsd", "qemu-kvm-common", "seabios-bin"];

    for package in packages {
        let args = vec![
            "-t", "1",
            "-m",
            "/usr/bin/rpm-ostree",
            "uninstall",
            "--idempotent",  // Don't fail if not installed
            package,
        ];

        let status = Command::new("/host/usr/bin/nsenter")
            .args(&args)
            .status();

        match status {
            Ok(s) if s.success() => {
                log::debug!("Uninstalled package: {}", package);
            }
            Ok(_) => {
                log::debug!("Package {} was not installed or failed to uninstall", package);
            }
            Err(e) => {
                log::warn!("Failed to run rpm-ostree uninstall for {}: {}", package, e);
            }
        }
    }

    // NOTE: We intentionally DO NOT use apply-live for uninstallation.
    //
    // Analysis of MCO (machine-config-operator) and rpm-ostree shows:
    // 1. MCO never uses apply-live - it always reboots for extension changes
    //    (see pkg/daemon/update.go:737-739)
    // 2. rpm-ostree apply-live deletes /etc files immediately and persistently,
    //    but /usr changes are deferred until reboot (via overlayfs)
    //    (see rpm-ostree/rust/src/live.rs:281-291)
    // 3. This mismatch causes CRI-O config to be deleted while binaries still exist,
    //    leading to CRI-O restart failures and cascading node failures
    //    (documented in PR #1349 openshift/sandboxed-containers-operator)
    //
    // The safe approach is to stage the changes and require a reboot,
    // which matches MCO's behavior for extension changes.
    info!("QEMU packages staged for removal - node reboot required to complete uninstallation");

    Ok(())
}

/// Install SELinux policy module for kata-monitor and QEMU access to /run/vc/.
/// This policy (osc_monitor.cil) grants containers running QEMU access to
/// container_var_run_t files, which is required for QMP socket communication.
/// Also sets the container_use_devices SELinux boolean for /dev/sev access.
pub fn install_selinux_policy(host_install_dir: &str) -> Result<()> {
    // Check if semodule is available on the host
    let host_semodule = Path::new("/host/usr/sbin/semodule");
    if !host_semodule.exists() {
        log::debug!("semodule not found at {:?}, skipping SELinux policy installation", host_semodule);
        return Ok(());
    }

    // Check if the policy file exists in our artifacts
    let policy_src = Path::new("/opt/kata-artifacts/selinux/osc_monitor.cil");
    if !policy_src.exists() {
        log::debug!("SELinux policy file not found at {:?}, skipping", policy_src);
        return Ok(());
    }

    // Copy policy to host
    let host_policy_dir = Path::new("/host/tmp/kata-selinux");
    fs::create_dir_all(host_policy_dir)?;
    let host_policy_path = host_policy_dir.join("osc_monitor.cil");
    fs::copy(policy_src, &host_policy_path)?;

    info!("Installing SELinux policy module osc_monitor");

    // Install the policy using nsenter
    let args = vec![
        "-t", "1",
        "-m",
        "/usr/sbin/semodule",
        "-i", "/tmp/kata-selinux/osc_monitor.cil",
    ];

    let status = Command::new("/host/usr/bin/nsenter")
        .args(&args)
        .status()
        .context("Failed to run semodule")?;

    if !status.success() {
        log::warn!("Failed to install SELinux policy module, continuing anyway");
    } else {
        info!("SELinux policy module osc_monitor installed successfully");
    }

    // Set container_use_devices boolean for /dev/sev access
    let setsebool_args = vec![
        "-t", "1",
        "-m",
        "/usr/sbin/setsebool",
        "-P", "container_use_devices", "1",
    ];

    let status = Command::new("/host/usr/bin/nsenter")
        .args(&setsebool_args)
        .status();

    match status {
        Ok(s) if s.success() => {
            info!("SELinux boolean container_use_devices set to 1");
        }
        _ => {
            log::debug!("Failed to set container_use_devices boolean, may not be needed");
        }
    }

    // Cleanup
    fs::remove_dir_all(host_policy_dir).ok();

    // Also copy the SELinux policy to the host install dir for reference
    let dst_selinux_dir = format!("{}/selinux", host_install_dir);
    fs::create_dir_all(&dst_selinux_dir)?;
    fs::copy(policy_src, format!("{}/osc_monitor.cil", dst_selinux_dir))?;

    Ok(())
}

/// Uninstall SELinux policy module.
pub fn uninstall_selinux_policy() -> Result<()> {
    let host_semodule = Path::new("/host/usr/sbin/semodule");
    if !host_semodule.exists() {
        log::debug!("semodule not found, skipping SELinux policy removal");
        return Ok(());
    }

    info!("Removing SELinux policy module osc_monitor");

    let args = vec![
        "-t", "1",
        "-m",
        "/usr/sbin/semodule",
        "-r", "osc_monitor",
    ];

    let status = Command::new("/host/usr/bin/nsenter")
        .args(&args)
        .status();

    match status {
        Ok(s) if s.success() => {
            info!("SELinux policy module osc_monitor removed successfully");
        }
        _ => {
            log::debug!("SELinux policy module osc_monitor was not installed or failed to remove");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_is_kata_required_rpm_qemu_packages() {
        // QEMU core packages should match
        assert!(is_kata_required_rpm("qemu-kvm-core-8.2.0-1.el9.x86_64.rpm"));
        assert!(is_kata_required_rpm("qemu-kvm-common-8.2.0-1.el9.x86_64.rpm"));
        assert!(is_kata_required_rpm("qemu-img-8.2.0-1.el9.x86_64.rpm"));
    }

    #[test]
    fn test_is_kata_required_rpm_virtiofsd() {
        assert!(is_kata_required_rpm("virtiofsd-1.10.1-1.el9.x86_64.rpm"));
    }

    #[test]
    fn test_is_kata_required_rpm_firmware() {
        // BIOS/firmware packages
        assert!(is_kata_required_rpm("seabios-bin-1.16.0-1.el9.noarch.rpm"));
        assert!(is_kata_required_rpm("seavgabios-bin-1.16.0-1.el9.noarch.rpm"));
        assert!(is_kata_required_rpm("ipxe-roms-qemu-20200823-1.el9.noarch.rpm"));
        assert!(is_kata_required_rpm("edk2-ovmf-20230524-1.el9.noarch.rpm"));
    }

    #[test]
    fn test_is_kata_required_rpm_kata_containers() {
        assert!(is_kata_required_rpm("kata-containers-3.2.0-1.el9.x86_64.rpm"));
        assert!(is_kata_required_rpm("kata-containers-cc-3.2.0-1.el9.x86_64.rpm"));
    }

    #[test]
    fn test_is_kata_required_rpm_dependencies() {
        // QEMU dependencies should match
        assert!(is_kata_required_rpm("libpmem-2.0.0-1.el9.x86_64.rpm"));
        assert!(is_kata_required_rpm("libfdt-1.6.1-1.el9.x86_64.rpm"));
        assert!(is_kata_required_rpm("librdmacm-46.0-1.el9.x86_64.rpm"));
        assert!(is_kata_required_rpm("pixman-0.40.0-1.el9.x86_64.rpm"));
        assert!(is_kata_required_rpm("capstone-4.0.2-1.el9.x86_64.rpm"));
        assert!(is_kata_required_rpm("libpng-1.6.37-1.el9.x86_64.rpm"));
        assert!(is_kata_required_rpm("ndctl-libs-76-1.el9.x86_64.rpm"));
        assert!(is_kata_required_rpm("daxctl-libs-76-1.el9.x86_64.rpm"));
    }

    #[test]
    fn test_is_kata_required_rpm_rejects_unwanted() {
        // Kernel packages should NOT match
        assert!(!is_kata_required_rpm("kernel-5.14.0-1.el9.x86_64.rpm"));
        assert!(!is_kata_required_rpm("kernel-core-5.14.0-1.el9.x86_64.rpm"));
        assert!(!is_kata_required_rpm("kernel-modules-5.14.0-1.el9.x86_64.rpm"));

        // Other unwanted packages
        assert!(!is_kata_required_rpm("fence-agents-all-4.10.0-1.el9.x86_64.rpm"));
        assert!(!is_kata_required_rpm("podman-4.6.0-1.el9.x86_64.rpm"));
        assert!(!is_kata_required_rpm("cri-o-1.28.0-1.el9.x86_64.rpm"));

        // Similar but not matching prefixes
        assert!(!is_kata_required_rpm("qemu-guest-agent-8.2.0-1.el9.x86_64.rpm"));
        // Note: libpmem2 would match because it starts with "libpmem" - this is acceptable
        // as libpmem2 is also a valid dependency
    }

    #[test]
    fn test_is_kata_required_rpm_empty_string() {
        assert!(!is_kata_required_rpm(""));
    }

    #[test]
    fn test_is_kata_required_rpm_no_extension() {
        // Should still match based on prefix, extension doesn't matter for matching
        assert!(is_kata_required_rpm("qemu-kvm-core"));
        assert!(is_kata_required_rpm("kata-containers"));
    }

    #[test]
    fn test_kata_required_rpm_prefixes_not_empty() {
        assert!(!KATA_REQUIRED_RPM_PREFIXES.is_empty());
        // Should have at least the essential packages
        assert!(KATA_REQUIRED_RPM_PREFIXES.contains(&"qemu-kvm-core"));
        assert!(KATA_REQUIRED_RPM_PREFIXES.contains(&"kata-containers"));
        assert!(KATA_REQUIRED_RPM_PREFIXES.contains(&"virtiofsd"));
    }

    #[test]
    fn test_copy_rpms_from_dir_copies_rpm_files() {
        let src_dir = TempDir::new().unwrap();
        let dst_dir = TempDir::new().unwrap();

        // Create some RPM files in source
        fs::write(src_dir.path().join("test-1.0.rpm"), "rpm content 1").unwrap();
        fs::write(src_dir.path().join("other-2.0.rpm"), "rpm content 2").unwrap();
        // Create a non-RPM file that should be ignored
        fs::write(src_dir.path().join("readme.txt"), "not an rpm").unwrap();

        copy_rpms_from_dir(src_dir.path(), dst_dir.path()).unwrap();

        // RPM files should be copied
        assert!(dst_dir.path().join("test-1.0.rpm").exists());
        assert!(dst_dir.path().join("other-2.0.rpm").exists());
        // Non-RPM files should NOT be copied
        assert!(!dst_dir.path().join("readme.txt").exists());
    }

    #[test]
    fn test_copy_rpms_from_dir_handles_nested_directories() {
        let src_dir = TempDir::new().unwrap();
        let dst_dir = TempDir::new().unwrap();

        // Create nested directory structure
        let nested = src_dir.path().join("subdir");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("nested-1.0.rpm"), "nested rpm").unwrap();
        fs::write(src_dir.path().join("root-1.0.rpm"), "root rpm").unwrap();

        copy_rpms_from_dir(src_dir.path(), dst_dir.path()).unwrap();

        // Both RPMs should be copied (flattened to dst_dir)
        assert!(dst_dir.path().join("root-1.0.rpm").exists());
        assert!(dst_dir.path().join("nested-1.0.rpm").exists());
    }

    #[test]
    fn test_copy_rpms_from_dir_overwrites_existing() {
        let src_dir = TempDir::new().unwrap();
        let dst_dir = TempDir::new().unwrap();

        // Create RPM in source with new content
        fs::write(src_dir.path().join("test.rpm"), "new content").unwrap();
        // Create existing RPM in dest with old content
        fs::write(dst_dir.path().join("test.rpm"), "old content").unwrap();

        copy_rpms_from_dir(src_dir.path(), dst_dir.path()).unwrap();

        // Should have new content
        let content = fs::read_to_string(dst_dir.path().join("test.rpm")).unwrap();
        assert_eq!(content, "new content");
    }

    #[test]
    fn test_copy_rpms_from_dir_empty_source() {
        let src_dir = TempDir::new().unwrap();
        let dst_dir = TempDir::new().unwrap();

        // Empty source directory should succeed without error
        copy_rpms_from_dir(src_dir.path(), dst_dir.path()).unwrap();

        // Destination should remain empty
        assert_eq!(fs::read_dir(dst_dir.path()).unwrap().count(), 0);
    }
}
