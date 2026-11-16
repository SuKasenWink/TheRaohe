//! GPU (Vulkan / GLES) æ¸²æŸ“å¾Œç«¯: é€éŽ wgpu + egui-wgpu æŠŠ egui primitives ç›´æŽ¥ç•«åˆ° overlay layerã€‚
//! layer å»ºç«‹æ™‚æŒ‡å®š RGBA_8888 (è¦‹ theraohe.rs çš„ ANativeWindow_setBuffersGeometry), ä¸”å…¶
//! å°ºå¯¸æ˜¯ side x side çš„æ–¹å½¢; é€™è£¡çš„ swapchain å¿…é ˆé…ç½®åŒå°ºå¯¸, å¦å‰‡ BufferQueue å¹¾ä½•ä¸ç¬¦,
//! compositor æœƒéœé»˜ä¸Ÿæ£„ bufferã€‚UI å…§å®¹ç”± egui é™åˆ¶åœ¨å¯è¦‹å€ (view_w x view_h) å…§,
//! å…¶é¤˜å€åŸŸä¿æŒé€æ˜Žâ€”â€”èˆ‡ CPU è»Ÿé«”å¾Œç«¯æŠŠ canvas æ‹·åˆ° side x side buffer å·¦ä¸Šè§’çš„èªžæ„ä¸€è‡´ã€‚
//! æˆªåœ–èµ° egui-wgpu çš„ CaptureState: é›¢å±æ¸²æŸ“åˆ° view-sized texture, ç›´æŽ¥ readback,
//! ä¸ç¢° swapchain, ä¹Ÿä¸ç”¨é¡å¤–ä¸€ä»½æ¸²æŸ“ä»£ç¢¼ã€‚

use egui_wgpu::{wgpu, Renderer, RendererOptions, ScreenDescriptor};
use egui_wgpu::capture::CaptureState;
use raw_window_handle::{AndroidDisplayHandle, AndroidNdkWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::ffi::c_void;
use std::sync::mpsc;

pub enum PresentOutcome {
    /// swapchain æœ¬å¹€æ²’æœ‰ buffer å¯ç”¨ (Timeout/Occluded/Lost); å‘¼å«æ–¹ä¸æ‡‰è¨ˆå…¥å‘ˆç¾å¹€æ•¸
    Skipped,
    /// æœ¬å¹€å·²å‘ˆç¾çµ¦ compositor
    Presented,
    /// é›¢å±å®Œæˆæˆªåœ–; åƒç´  = å¯è¦‹å€, premultiplied RGBA
    Captured(Vec<[u8; 4]>),
}

pub struct GpuRenderer {
    window: usize,
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    alpha_mode: wgpu::CompositeAlphaMode,
    surface_usage: wgpu::TextureUsages,
    /// swapchain / layer å°ºå¯¸ (side x side æ–¹å½¢)
    surface_w: u32,
    surface_h: u32,
    /// å¯è¦‹å€å°ºå¯¸ (egui çš„ä¸–ç•Œ)
    view_w: u32,
    view_h: u32,
    needs_reconfigure: bool,
    renderer: Renderer,
    capture: Option<CaptureState>,
}

impl GpuRenderer {
    /// `surface_w/h` å¿…é ˆç­‰æ–¼ ANativeWindow_setBuffersGeometry çš„å°ºå¯¸,
    /// `view_w/h` æ˜¯å¯¦éš›å¯è¦‹å€ (egui åº§æ¨™ = ç‰©ç†åƒç´ )
    pub fn new(
        window: *mut c_void,
        surface_w: u32,
        surface_h: u32,
        view_w: u32,
        view_h: u32,
    ) -> Result<Self, String> {
        let surface_w = surface_w.max(1);
        let surface_h = surface_h.max(1);
        let view_w = view_w.max(1);
        let view_h = view_h.max(1);
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::VULKAN | wgpu::Backends::GL;
        let instance = wgpu::Instance::new(desc);
        let surface = create_surface(&instance, window)?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .map_err(|e| format!("request_adapter: {e}"))?;
        let info = adapter.get_info();
        eprintln!(
            "[gpu] adapter: {} backend={:?} type={:?}",
            info.name, info.backend, info.device_type
        );
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("theraohe"),
            ..Default::default()
        }))
        .map_err(|e| format!("request_device: {e}"))?;
        let caps = surface.get_capabilities(&adapter);
        if caps.formats.is_empty() {
            return Err("surface ä¸æ”¯æ´ä»»ä½•æ ¼å¼".to_string());
        }
        eprintln!(
            "[gpu] surface formats={:?} alpha_modes={:?} present_modes={:?} usages={:?}",
            caps.formats, caps.alpha_modes, caps.present_modes, caps.usages
        );
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| {
                matches!(
                    f,
                    wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
                )
            })
            .unwrap_or(caps.formats[0]);
        // Mali 的 swapchain image 缺少 HWC 能 import 的 usage 時, layer 的 buffer 永遠進不了
        // compositor; Mali 只保證它 SDK 報告的整組(不外加自定義的), 所以直接取 caps 報告的
        // 集合, 不做收窄
        let surface_usage = caps.usages;
        // overlay ä¾è³´é ä¹˜ alpha åšé¤ç©º; æ‹¿ä¸åˆ° PreMultiplied å°±é€€è€Œæ±‚å…¶æ¬¡, é€æ˜Žåº¦ç”± compositor å…œåº•
        let alpha_mode = caps
            .alpha_modes
            .iter()
            .copied()
            .find(|m| *m == wgpu::CompositeAlphaMode::PreMultiplied)
            .or_else(|| caps.alpha_modes.first().copied())
            .unwrap_or(wgpu::CompositeAlphaMode::Auto);
        eprintln!("[gpu] format={format:?} alpha={alpha_mode:?} surface={surface_w}x{surface_h} view={view_w}x{view_h}");
        configure(&surface, &device, format, alpha_mode, surface_usage, surface_w, surface_h);
        let renderer = Renderer::new(&device, format, RendererOptions::default());
        Ok(Self {
            window: window as usize,
            instance,
            surface,
            device,
            queue,
            format,
            alpha_mode,
            surface_usage,
            surface_w,
            surface_h,
            view_w,
            view_h,
            needs_reconfigure: false,
            renderer,
            capture: None,
        })
    }

    /// å¯è¦‹å€è®Šæ›´æ™‚å‘¼å« (æ—‹è½‰); swapchain å°ºå¯¸è·Ÿè‘— side x side èµ°, é€šå¸¸ä¸è®Š
    pub fn resize(
        &mut self,
        surface_w: u32,
        surface_h: u32,
        view_w: u32,
        view_h: u32,
    ) {
        let surface_w = surface_w.max(1);
        let surface_h = surface_h.max(1);
        let view_w = view_w.max(1);
        let view_h = view_h.max(1);
        self.view_w = view_w;
        self.view_h = view_h;
        if (self.surface_w, self.surface_h) != (surface_w, surface_h) {
            self.surface_w = surface_w;
            self.surface_h = surface_h;
            self.needs_reconfigure = false;
            configure(
                &self.surface,
                &self.device,
                self.format,
                self.alpha_mode,
                self.surface_usage,
                surface_w,
                surface_h,
            );
        }
        if let Some(capture) = &mut self.capture {
            capture.update(&self.device, extent(view_w, view_h));
        }
    }

    /// å‘ˆç¾ä¸€å¹€; capture=true æ™‚é›¢å±æ¸²æŸ“ä¸¦ç›´æŽ¥ readback å¯è¦‹å€åƒç´  (premultiplied RGBA),
    /// è©²å¹€ä¸å‘ˆç¾çµ¦ compositor (ç•«é¢å…§å®¹èˆ‡ä¸Šä¸€å¹€ç­‰åƒ¹, å°‘ä¸€æ¬¡ present å°ä½¿ç”¨è€…ç„¡æ„Ÿ)
    ///
    /// å›žå‚³ `false` = swapchain æœ¬å¹€æ²’æœ‰ buffer å¯ç”¨ (Timeout/Occluded/Lost å·²é‡å»ºæˆ–è·³éŽ),
    /// å‘¼å«æ–¹ä¸å¯æŠŠé€™ç•¶ Graphics presentation; `true` = æœ¬å¹€ç¢ºå¯¦å‘ˆç¾çµ¦ compositor
    pub fn present(
        &mut self,
        paint_jobs: &[egui::epaint::ClippedPrimitive],
        textures_delta: egui::TexturesDelta,
        pixels_per_point: f32,
        capture: bool,
    ) -> Result<PresentOutcome, String> {
        if self.needs_reconfigure {
            self.needs_reconfigure = false;
            configure(
                &self.surface,
                &self.device,
                self.format,
                self.alpha_mode,
                self.surface_usage,
                self.surface_w,
                self.surface_h,
            );
        }
        let screen = ScreenDescriptor {
            size_in_pixels: [self.view_w, self.view_h],
            pixels_per_point,
        };
        let mut textures_delta = textures_delta;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("theraohe"),
            });

        for (id, deltas) in textures_delta.set.drain() {
            for delta in deltas {
                self.renderer
                    .update_texture(&self.device, &self.queue, id, &delta);
            }
        }
        let pending_free = std::mem::take(&mut textures_delta.free);
        let user_cmd_bufs = self.renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            paint_jobs,
            &screen,
        );

        if capture {
            return self.render_offscreen(encoder, user_cmd_bufs, paint_jobs, &screen, pending_free);
        }


        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                // Suboptimal ä»å¯ç”¨; ä¸‹ä¸€å¹€é‡å»ºé…ç½®
                self.needs_reconfigure = true;
                frame
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                eprintln!("[gpu] get_current_texture: Timeout");
                self.free_textures(pending_free);
                return Ok(PresentOutcome::Skipped);
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                eprintln!("[gpu] get_current_texture: Occluded");
                self.free_textures(pending_free);
                return Ok(PresentOutcome::Skipped);
            }
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                eprintln!("[gpu] get_current_texture: Lost/Outdated, é‡å»º surface");
                self.recreate_surface()?;
                self.free_textures(pending_free);
                return Ok(PresentOutcome::Skipped);
            }
            _ => {
                self.free_textures(pending_free);
                return Err("get_current_texture: æœªçŸ¥éŒ¯èª¤".to_string());
            }
        };

        let frame_view = frame.texture.create_view(&Default::default());
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("theraohe"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &frame_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            // SAFETY-ish: RenderPass æœƒåœ¨ encoder ä¹‹å‰æäº¤, forget_lifetime åªæ˜¯æ”¾å¯¬ç”Ÿå‘½é€±æœŸæ¨™è¨»;
            // pass åœ¨æœ¬å€å¡ŠçµæŸæ™‚ drop, encoder éš¨å¾Œåœ¨åŒä¸€å¹€ submit
            self.renderer.render(&mut pass.forget_lifetime(), paint_jobs, &screen);
        }

        self.queue
            .submit(user_cmd_bufs.into_iter().chain([encoder.finish()]));
        self.free_textures(pending_free);
        // 不呈現就不會有 buffer 交給 compositor, layer 在 SF 眼裡永遠是空的;
        // SurfaceTexture 被 drop 而不 present = texture_discard (wgpu-core 的 SurfaceTexture::drop)
        self.queue.present(frame);
        Ok(PresentOutcome::Presented)
    }

    // æˆªåœ–å¹€: egui ç•«åˆ° view-sized é›¢å± texture, ä¸ç¢° swapchain, readback å‡ºåƒç´ 
    fn render_offscreen(
        &mut self,
        mut encoder: wgpu::CommandEncoder,
        user_cmd_bufs: Vec<wgpu::CommandBuffer>,
        paint_jobs: &[egui::epaint::ClippedPrimitive],
        screen: &ScreenDescriptor,
        pending_free: impl IntoIterator<Item = egui::TextureId>,
    ) -> Result<PresentOutcome, String> {
        let size = extent(self.view_w, self.view_h);
        self.capture
            .get_or_insert_with(|| CaptureState::new(&self.device, size, self.format));
        self.capture.as_mut().unwrap().update(&self.device, size);
        let target_view = self
            .capture
            .as_ref()
            .unwrap()
            .texture
            .create_view(&Default::default());
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("theraohe-capture"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            self.renderer.render(&mut pass.forget_lifetime(), paint_jobs, screen);
        }
        let buffer = {
            let cs = self.capture.as_mut().unwrap();
            cs.copy_textures(&self.device, None, &mut encoder)
        };
        self.queue
            .submit(user_cmd_bufs.into_iter().chain([encoder.finish()]));
        self.free_textures(pending_free);

        let (tx, rx) = mpsc::channel();
        self.capture.as_ref().unwrap().read_screen_rgba(
            buffer,
            vec![egui::ScreenshotCallback::new(move |image: std::sync::Arc<egui::ColorImage>| {
                let _ = tx.send(image);
            })],
        );
        // è®“ map çš„ callback åœ¨æ­¤å¹€å…§è·‘å®Œ; æˆªåœ–æ˜¯ä½Žé »æ“ä½œ, é˜»å¡žç­‰ GPU æ˜¯å¯æŽ¥å—çš„
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let image = rx.recv().map_err(|_| "æˆªåœ–è®€å›žå¤±æ•—".to_string())?;
        let pixels = std::sync::Arc::into_inner(image)
            .map(|ci| ci.pixels)
            .unwrap_or_default();
        Ok(PresentOutcome::Captured(pixels.into_iter().map(|c| c.to_array()).collect()))
    }

    fn free_textures(&mut self, ids: impl IntoIterator<Item = egui::TextureId>) {
        for id in ids {
            self.renderer.free_texture(&id);
        }
    }

    // Lost éœ€è¦é‡å»º surface æ‰èƒ½æ¢å¾©; adapter æ‹¿å›žåŽŸæ ¼å¼, æ‹¿ä¸å›žå°±å ±éŒ¯, äº¤çµ¦å‘¼å«æ–¹å›žè½ CPU
    fn recreate_surface(&mut self) -> Result<(), String> {
        // SAFETY: window ç”± surface control æŒæœ‰, é€²ç¨‹çµæŸå‰æœ‰æ•ˆ
        let surface = create_surface(&self.instance, self.window as *mut c_void)?;
        let adapter = pollster::block_on(self.instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .map_err(|e| format!("recreate request_adapter: {e}"))?;
        let caps = surface.get_capabilities(&adapter);
        if !caps.formats.contains(&self.format) {
            return Err("recreate: surface ä¸å†åŒ…å«ç›®å‰æ ¼å¼".to_string());
        }
        self.surface = surface;
        self.needs_reconfigure = false;
        configure(
            &self.surface,
            &self.device,
            self.format,
            self.alpha_mode,
            self.surface_usage,
            self.surface_w,
            self.surface_h,
        );
        Ok(())
    }
}

fn extent(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}

fn configure(
    surface: &wgpu::Surface<'static>,
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    alpha_mode: wgpu::CompositeAlphaMode,
    surface_usage: wgpu::TextureUsages,
    width: u32,
    height: u32,
) {
    let config = wgpu::SurfaceConfiguration {
        usage: surface_usage,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        width,
        height,
        present_mode: wgpu::PresentMode::AutoVsync,
        desired_maximum_frame_latency: 1,
        alpha_mode,
        view_formats: vec![],
    };
    eprintln!("[gpu] configure surface {width}x{height} format={format:?} alpha={alpha_mode:?}");
    surface.configure(device, &config);
}

fn create_surface(
    instance: &wgpu::Instance,
    window: *mut c_void,
) -> Result<wgpu::Surface<'static>, String> {
    let window_ptr = std::ptr::NonNull::new(window.cast::<c_void>())
        .ok_or_else(|| "ANativeWindow ç‚º null".to_string())?;
    let target = wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(RawDisplayHandle::Android(AndroidDisplayHandle::new())),
        raw_window_handle: RawWindowHandle::AndroidNdk(AndroidNdkWindowHandle::new(window_ptr)),
    };
    // SAFETY: ANativeWindow è‡ª createSurface èµ·ç”± SurfaceControl æŒæœ‰, èˆ‡é€²ç¨‹åŒå£½;
    // Surface ä¸ç¶å®š Rust å€Ÿç”¨, layer åœ¨é€²ç¨‹çµæŸå‰ä¸æœƒè¢«é‡‹æ”¾
    unsafe { instance.create_surface_unsafe(target) }.map_err(|e| format!("create_surface: {e}"))
}
