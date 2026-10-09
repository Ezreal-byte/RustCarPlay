// SPDX-License-Identifier: GPL-3.0-only
//! Upload opaque decoded RGBA directly, without rebuilding an egui ColorImage
//! (and its second full-size pixel allocation) for every video frame.
use carplay_media::RgbaFrame;
use eframe::{egui, egui_wgpu::RenderState};

pub enum VideoTexture {
    Native {
        state: RenderState,
        texture: wgpu::Texture,
        id: egui::TextureId,
    },
    Software(egui::TextureHandle),
}

impl VideoTexture {
    pub fn upload(
        slot: &mut Option<Self>,
        state: Option<&RenderState>,
        ctx: &egui::Context,
        frame: &RgbaFrame,
    ) -> Result<(), &'static str> {
        let length = (frame.width as usize)
            .checked_mul(frame.height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or("decoded frame dimensions overflow")?;
        if frame.width == 0 || frame.height == 0 || length != frame.rgba.len() {
            return Err("invalid decoded RGBA frame");
        }
        if let Some(state) = state {
            let limit = state.device.limits().max_texture_dimension_2d;
            if frame.width > limit || frame.height > limit {
                return Err("decoded frame exceeds GPU texture limits");
            }
            if !matches!(slot, Some(Self::Native { texture, .. })
                if texture.width() == frame.width && texture.height() == frame.height)
            {
                let texture = state.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("CarPlay video"),
                    size: wgpu::Extent3d {
                        width: frame.width,
                        height: frame.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_DST
                        | if cfg!(test) {
                            wgpu::TextureUsages::COPY_SRC
                        } else {
                            wgpu::TextureUsages::empty()
                        },
                    view_formats: &[],
                });
                let view = texture.create_view(&Default::default());
                let id = state.renderer.write().register_native_texture(
                    &state.device,
                    &view,
                    wgpu::FilterMode::Linear,
                );
                *slot = Some(Self::Native {
                    state: state.clone(),
                    texture,
                    id,
                });
            }
            if let Some(Self::Native { texture, .. }) = slot {
                state.queue.write_texture(
                    texture.as_image_copy(),
                    &frame.rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(frame.width * 4),
                        rows_per_image: Some(frame.height),
                    },
                    texture.size(),
                );
            }
        } else {
            // Headless UI tests and renderers without native texture access.
            let image = egui::ColorImage::from_rgba_premultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba,
            );
            if let Some(Self::Software(texture)) = slot {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                *slot = Some(Self::Software(ctx.load_texture(
                    "carplay-frame",
                    image,
                    egui::TextureOptions::LINEAR,
                )));
            }
        }
        Ok(())
    }

    pub fn id(&self) -> egui::TextureId {
        match self {
            Self::Native { id, .. } => *id,
            Self::Software(texture) => texture.id(),
        }
    }

    pub fn size_vec2(&self) -> egui::Vec2 {
        match self {
            Self::Native { texture, .. } => {
                egui::vec2(texture.width() as f32, texture.height() as f32)
            }
            Self::Software(texture) => texture.size_vec2(),
        }
    }
}

impl Drop for VideoTexture {
    fn drop(&mut self) {
        if let Self::Native { state, id, .. } = self {
            state.renderer.write().free_texture(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::Future,
        sync::Arc,
        task::{Context, Poll, Wake, Waker},
        time::Duration,
    };

    fn block_on<F: Future>(future: F) -> F::Output {
        struct Notify(std::thread::Thread);
        impl Wake for Notify {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Waker::from(Arc::new(Notify(std::thread::current())));
        let mut context = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::park(),
            }
        }
    }

    #[test]
    #[ignore = "requires a native GPU; run locally with --ignored"]
    fn native_texture_preserves_pixels_reuses_storage_and_releases_registration() {
        let instance = wgpu::Instance::new(&Default::default());
        let state = block_on(RenderState::create(
            &Default::default(),
            &instance,
            None,
            Default::default(),
        ))
        .unwrap();
        let ctx = egui::Context::default();
        let mut slot = None;
        let mut frame = RgbaFrame {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
            pts_ns: None,
        };
        VideoTexture::upload(&mut slot, Some(&state), &ctx, &frame).unwrap();
        let original = slot.as_ref().unwrap().id();
        frame.rgba = vec![0, 0, 255, 255, 255, 255, 255, 255];
        VideoTexture::upload(&mut slot, Some(&state), &ctx, &frame).unwrap();
        assert_eq!(slot.as_ref().unwrap().id(), original);
        let Some(VideoTexture::Native { texture, .. }) = &slot else {
            panic!("native GPU upload not used")
        };
        let readback = state.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video readback"),
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = state.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            texture.size(),
        );
        state.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        state
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(10)),
            })
            .unwrap();
        rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(
            &readback.slice(..).get_mapped_range()[..8],
            frame.rgba.as_slice()
        );
        readback.unmap();
        frame.width = 1;
        frame.rgba.truncate(4);
        VideoTexture::upload(&mut slot, Some(&state), &ctx, &frame).unwrap();
        assert!(state.renderer.read().texture(&original).is_none());
        assert_eq!(slot.as_ref().unwrap().size_vec2(), egui::vec2(1., 1.));
        let resized = slot.as_ref().unwrap().id();
        drop(slot);
        assert!(state.renderer.read().texture(&resized).is_none());
    }
}
