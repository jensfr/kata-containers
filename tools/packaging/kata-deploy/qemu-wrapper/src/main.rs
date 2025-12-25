// QEMU wrapper for Kata Containers on OpenShift HCP
//
// This binary wrapper adds the -L flag to QEMU to specify the data directory
// for BIOS/firmware files. Shell wrappers don't work because kata containers
// uses exec() directly.
//
// SPDX-License-Identifier: Apache-2.0

use std::env;
use std::os::unix::process::CommandExt;
use std::process::Command;

const QEMU_BINARY: &str = "/opt/kata/bin/qemu-system-x86_64.bin";
const QEMU_DATA_DIR: &str = "/opt/kata/share/qemu-kvm";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    // Build new argument list with -L prepended
    let mut new_args = vec![
        "-L".to_string(),
        QEMU_DATA_DIR.to_string(),
    ];
    new_args.extend(args);

    // exec() replaces this process with QEMU
    let err = Command::new(QEMU_BINARY)
        .args(&new_args)
        .exec();

    // exec() only returns on error
    eprintln!("qemu-wrapper: failed to exec {}: {}", QEMU_BINARY, err);
    std::process::exit(1);
}
