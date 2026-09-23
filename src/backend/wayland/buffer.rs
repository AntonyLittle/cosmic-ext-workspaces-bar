// SPDX-License-Identifier: GPL-3.0-only
// Based on cosmic-workspaces-epoch, Copyright 2023 System76 <info@system76.com>
// Simplified to shm-only buffers rendered as iced images.

use cosmic::cctk;

use cctk::screencopy::{Formats, Rect};
use cctk::wayland_client::protocol::{wl_buffer, wl_shm, wl_shm_pool};
use cctk::wayland_client::{Connection, Dispatch, QueueHandle};

use std::os::fd::AsFd;

use super::AppData;
use crate::utils;

pub struct Buffer {
    pub buffer: wl_buffer::WlBuffer,
    pub buffer_damage: Vec<Rect>,
    pub size: (u32, u32),
    pub mmap: memmap2::Mmap,
}

impl AppData {
    pub fn create_buffer(&self, formats: &Formats) -> Buffer {
        // Bytes in memory are R,G,B,A; matches `image::Handle::from_rgba`
        let format = wl_shm::Format::Abgr8888;
        assert!(formats.shm_formats.contains(&format));
        let (width, height) = formats.buffer_size;

        let fd = utils::create_memfile().unwrap();
        rustix::fs::ftruncate(&fd, width as u64 * height as u64 * 4).unwrap();

        let pool = self.shm_state.wl_shm().create_pool(
            fd.as_fd(),
            width as i32 * height as i32 * 4,
            &self.qh,
            (),
        );

        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            width as i32 * 4,
            format,
            &self.qh,
            (),
        );

        pool.destroy();

        let mmap = unsafe { memmap2::Mmap::map(&fd).unwrap() };

        let full_damage = vec![Rect {
            x: 0,
            y: 0,
            width: width as i32,
            height: height as i32,
        }];

        Buffer {
            buffer,
            buffer_damage: full_damage,
            size: (width, height),
            mmap,
        }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for AppData {
    fn event(
        _app_data: &mut Self,
        _buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_buffer::Event::Release => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for AppData {
    fn event(
        _app_data: &mut Self,
        _shm: &wl_shm_pool::WlShmPool,
        _event: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
