// SPDX-License-Identifier: GPL-3.0-only
// Based on cosmic-workspaces-epoch, Copyright 2023 System76 <info@system76.com>

use rustix::io::Errno;
use rustix::shm;
use std::os::fd::OwnedFd;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "linux")]
fn create_memfd() -> rustix::io::Result<OwnedFd> {
    let fd = rustix::io::retry_on_intr(|| {
        rustix::fs::memfd_create(
            "cosmic-ext-workspaces-bar-shm",
            rustix::fs::MemfdFlags::CLOEXEC | rustix::fs::MemfdFlags::ALLOW_SEALING,
        )
    })?;
    let _ = rustix::fs::fcntl_add_seals(
        &fd,
        rustix::fs::SealFlags::SHRINK | rustix::fs::SealFlags::SEAL,
    );
    Ok(fd)
}

pub fn create_memfile() -> rustix::io::Result<OwnedFd> {
    #[cfg(target_os = "linux")]
    if let Ok(fd) = create_memfd() {
        return Ok(fd);
    }

    loop {
        let flags = shm::OFlags::CREATE | shm::OFlags::EXCL | shm::OFlags::RDWR;

        let time = SystemTime::now();
        let name = format!(
            "/cosmic-ext-workspaces-bar-shm-{}",
            time.duration_since(UNIX_EPOCH).unwrap().subsec_nanos()
        );

        match shm::open(&name, flags, 0o600.into()) {
            Ok(fd) => match shm::unlink(&name) {
                Ok(_) => return Ok(fd),
                Err(errno) => {
                    return Err(errno);
                }
            },
            Err(Errno::EXIST) => {
                continue;
            }
            Err(errno) => return Err(errno),
        }
    }
}
