// SPDX-License-Identifier: GPL-3.0-only
//
// GPU-side capture destination buffers. When the compositor advertises dmabuf
// support for a screencopy session (`Formats::dmabuf_device`/`dmabuf_formats`),
// we allocate the destination buffer as a dmabuf via `gbm` instead of `shm`.
// This is stage 1: the compositor's GPU->CPU copy for screencopy is
// unavoidable either way, but capturing into a dmabuf is a prerequisite for
// stage 2 (GPU-side downscale via an EGLImage import, avoiding a full-frame
// CPU readback). Falls back to shm if dmabuf isn't advertised or anything
// here fails.

use cosmic::cctk;

use cctk::screencopy::Rect;
use cctk::sctk::dmabuf::{DmabufHandler, DmabufState};
use cctk::wayland_client::protocol::wl_buffer;
use cctk::wayland_client::{Connection, QueueHandle};
use cctk::wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1;

use gbm::{BufferObject, BufferObjectFlags, Device as GbmDevice, Format as GbmFormat};

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::rc::Rc;

use super::AppData;

pub struct DmabufBuffer {
    pub buffer: wl_buffer::WlBuffer,
    pub buffer_damage: Vec<Rect>,
    pub size: (u32, u32),
    bo: BufferObject<()>,
}

impl DmabufBuffer {
    /// CPU-mmap readback of the whole buffer as tightly-packed rows (fallback
    /// path when the GPU downscale context isn't available)
    pub fn with_pixels<R>(&self, f: impl FnOnce(&[u8], u32) -> R) -> std::io::Result<R> {
        let (w, h) = self.size;
        self.bo.map(0, 0, w, h, |mapped| f(mapped.buffer(), mapped.stride()))
    }

    pub fn bo(&self) -> &BufferObject<()> {
        &self.bo
    }
}

impl Drop for DmabufBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

/// Render node devices opened so far, keyed by `dev_t`, so outputs on the
/// same GPU share one `gbm::Device`
#[derive(Default)]
pub struct DmabufDevices {
    by_dev: HashMap<libc::dev_t, Rc<GbmDevice<File>>>,
}

impl DmabufDevices {
    fn get(&mut self, target: libc::dev_t) -> Option<Rc<GbmDevice<File>>> {
        if let Some(dev) = self.by_dev.get(&target) {
            return Some(dev.clone());
        }
        let entries = std::fs::read_dir("/dev/dri").ok()?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with("renderD") {
                continue;
            }
            let path = entry.path();
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.rdev() != target {
                continue;
            }
            let Ok(file) = std::fs::OpenOptions::new().read(true).write(true).open(&path) else {
                log::warn!("found render node {path:?} but couldn't open it");
                continue;
            };
            return match GbmDevice::new(file) {
                Ok(gbm) => {
                    let gbm = Rc::new(gbm);
                    self.by_dev.insert(target, gbm.clone());
                    Some(gbm)
                }
                Err(err) => {
                    log::warn!("gbm::Device::new failed for {path:?}: {err}");
                    None
                }
            };
        }
        None
    }
}

impl AppData {
    // Lazily creates (or reuses) the GPU downscale context for `device`.
    // Returns `None` if it isn't available; caller falls back to CPU mmap.
    // A failed attempt is remembered so we don't retry (and re-log) every frame.
    pub(super) fn gpu_downscaler(&mut self, device: libc::dev_t) -> Option<&mut super::GpuDownscaler> {
        if self.gpu_downscaler_failed {
            return None;
        }
        if self.gpu_downscaler.as_ref().is_none_or(|(d, _)| *d != device) {
            let gbm = self.dmabuf_devices.get(device)?;
            let Some(downscaler) = super::GpuDownscaler::new(&gbm) else {
                self.gpu_downscaler_failed = true;
                return None;
            };
            log::info!("GPU downscale context created");
            self.gpu_downscaler = Some((device, downscaler));
        }
        self.gpu_downscaler.as_mut().map(|(_, d)| d)
    }

    /// Try to allocate a dmabuf-backed capture destination buffer matching
    /// one of the compositor's advertised (device, format, modifiers).
    /// Returns `None` if dmabuf isn't usable here; caller falls back to shm.
    pub fn create_dmabuf_buffer(
        &mut self,
        device: libc::dev_t,
        dmabuf_formats: &[(u32, Vec<u64>)],
        size: (u32, u32),
    ) -> Option<DmabufBuffer> {
        // Matches the shm path's proven-correct channel order (see buffer.rs)
        const WANT_FORMAT: u32 = GbmFormat::Abgr8888 as u32;
        let (_, modifiers) = dmabuf_formats.iter().find(|(fmt, _)| *fmt == WANT_FORMAT)?;
        if modifiers.is_empty() {
            return None;
        }
        let gbm = match self.dmabuf_devices.get(device) {
            Some(gbm) => gbm,
            None => {
                log::info!("no render node found for dmabuf device {device:#x}; using shm");
                return None;
            }
        };
        let (width, height) = size;
        let bo = match gbm.create_buffer_object_with_modifiers2::<()>(
            width,
            height,
            GbmFormat::Abgr8888,
            modifiers.iter().map(|m| (*m).into()),
            BufferObjectFlags::RENDERING,
        ) {
            Ok(bo) => bo,
            Err(err) => {
                log::warn!("gbm buffer allocation failed: {err}");
                return None;
            }
        };

        let plane_count = bo.plane_count() as i32;
        let modifier: u64 = bo.modifier().into();
        let params = self.dmabuf_state.create_params(&self.qh).ok()?;
        for plane in 0..plane_count {
            let fd = match bo.fd_for_plane(plane) {
                Ok(fd) => fd,
                Err(err) => {
                    log::warn!("failed to export dmabuf plane {plane}: {err}");
                    return None;
                }
            };
            params.add(
                fd.as_fd(),
                plane as u32,
                bo.offset(plane),
                bo.stride_for_plane(plane),
                modifier,
            );
        }
        let (buffer, params_obj) = params.create_immed(
            width as i32,
            height as i32,
            WANT_FORMAT,
            zwp_linux_buffer_params_v1::Flags::empty(),
            &self.qh,
        );
        params_obj.destroy();

        log::info!(
            "dmabuf capture buffer allocated: {width}x{height} format=Abgr8888 modifier={modifier:#x}"
        );
        let full_damage = vec![Rect {
            x: 0,
            y: 0,
            width: width as i32,
            height: height as i32,
        }];
        Some(DmabufBuffer {
            buffer,
            buffer_damage: full_damage,
            size,
            bo,
        })
    }
}

impl DmabufHandler for AppData {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_feedback(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _proxy: &cctk::wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
        _feedback: cctk::sctk::dmabuf::DmabufFeedback,
    ) {
        // Format/device negotiation for capture buffers comes from the
        // screencopy protocol's own `Formats` instead (see screencopy.rs)
    }

    fn created(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _params: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        _buffer: wl_buffer::WlBuffer,
    ) {
        // Only used with the async `create()` path; we use `create_immed()`
    }

    fn failed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _params: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
    ) {
        log::warn!("compositor rejected a dmabuf capture buffer after creation");
    }

    fn released(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _buffer: &wl_buffer::WlBuffer) {}
}

cctk::sctk::delegate_dmabuf!(AppData);
