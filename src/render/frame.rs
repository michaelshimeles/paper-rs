//! One full app frame: egui layout → canvas pass → egui pass. Shared by the window and headless capture.

use super::gpu::CanvasRenderer;
use super::scene;
use crate::editor::Editor;
use crate::geom::v2;
use crate::ui::{self, UiState};

pub struct Painter {
    pub canvas: CanvasRenderer,
    pub cache: scene::SceneCache,
    egui: egui_wgpu::Renderer,
    /// Clear to transparent so a vibrancy view behind the window shows through wherever the
    /// UI doesn't paint (the canvas always paints its own opaque background).
    pub transparent: bool,
}

/// Result of laying out the UI for a frame.
pub struct Layout {
    pub output: egui::FullOutput,
    /// Canvas rect in logical px.
    pub canvas: egui::Rect,
}

/// Run the egui UI, then sync the editor viewport to the canvas rect.
pub fn layout(
    ctx: &egui::Context,
    input: egui::RawInput,
    ed: &mut Editor,
    st: &mut UiState,
) -> Layout {
    let mut canvas = egui::Rect::NOTHING;
    let output = ctx.run_ui(input, |ui| {
        canvas = ui::draw(ui, ed, st);
    });
    ed.viewport = v2(
        canvas.width().max(1.0) as f64,
        canvas.height().max(1.0) as f64,
    );
    if ed.pending_fit && canvas.width() > 1.0 {
        ed.pending_fit = false;
        ed.zoom_to_fit();
    }
    Layout { output, canvas }
}

impl Painter {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Painter {
        Painter {
            canvas: CanvasRenderer::new(device, format),
            cache: scene::SceneCache::default(),
            egui: egui_wgpu::Renderer::new(device, format, egui_wgpu::RendererOptions::default()),
            transparent: false,
        }
    }

    /// Draw a frame into `target` (`size` physical px).
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        size: [u32; 2],
        ctx: &egui::Context,
        layout: Layout,
        ed: &Editor,
        show_grid: bool,
    ) {
        let ppp = layout.output.pixels_per_point;
        let offset = v2(layout.canvas.min.x as f64, layout.canvas.min.y as f64);
        let dark = ctx.theme() == egui::Theme::Dark;
        let frame = scene::canvas(ed, &mut self.cache, offset, show_grid, dark);
        let mut enc = device.create_command_encoder(&Default::default());
        let bg = if self.transparent {
            [0.0; 4]
        } else {
            scene::canvas_bg(dark).to_f32(1.0)
        };
        self.canvas.render(
            device,
            queue,
            &mut enc,
            target,
            size,
            ppp,
            &frame,
            [
                bg[0] as f64,
                bg[1] as f64,
                bg[2] as f64,
                if self.transparent { 0.0 } else { 1.0 },
            ],
        );

        let mut out = layout.output;
        let prims = ctx.tessellate(out.shapes, ppp);
        self.upload_textures(device, queue, &mut out.textures_delta);
        let sd = egui_wgpu::ScreenDescriptor {
            size_in_pixels: size,
            pixels_per_point: ppp,
        };
        let extra = self
            .egui
            .update_buffers(device, queue, &mut enc, &prims, &sd);
        {
            let mut pass = enc
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.egui.render(&mut pass, &prims, &sd);
        }
        queue.submit(extra.into_iter().chain([enc.finish()]));
        self.free_textures(&mut out.textures_delta);
    }

    /// Apply `textures_delta.set` (allocations and partial updates). Must run for every
    /// egui frame, even when the surface frame is skipped; otherwise a later partial
    /// update targets a texture that was never allocated and egui-wgpu panics.
    pub fn upload_textures(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        delta: &mut egui::TexturesDelta,
    ) {
        for (id, deltas) in delta.set.drain() {
            for d in &deltas {
                self.egui.update_texture(device, queue, id, d);
            }
        }
    }

    /// Apply `textures_delta.free`. Call only after rendering (or when no render happens).
    pub fn free_textures(&mut self, delta: &mut egui::TexturesDelta) {
        for id in delta.free.drain() {
            self.egui.free_texture(&id);
        }
    }

    /// Frame skipped (occluded / zero-size / surface error): keep texture state in sync.
    pub fn skip_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        delta: &mut egui::TexturesDelta,
    ) {
        self.upload_textures(device, queue, delta);
        self.free_textures(delta);
    }
}

/// Render the full app (chrome + canvas) offscreen, for screenshots and visual tests.
pub fn capture(
    gpu: &super::gpu::Headless,
    ed: &mut Editor,
    st: &mut UiState,
    size_points: [f32; 2],
    ppp: f32,
    prepare: impl FnOnce(&mut Editor),
) -> image::RgbaImage {
    let ctx = egui::Context::default();
    ui::setup_style(&ctx);
    crate::theme::set_preference(&ctx, st.options.theme);
    let size = [
        (size_points[0] * ppp).round() as u32,
        (size_points[1] * ppp).round() as u32,
    ];
    let input = || egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size_points[0], size_points[1]),
        )),
        viewports: std::iter::once((
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                native_pixels_per_point: Some(ppp),
                ..Default::default()
            },
        ))
        .collect(),
        ..Default::default()
    };
    // egui needs a couple of passes to settle sizes and fonts.
    let mut painter = Painter::new(&gpu.device, super::gpu::OFFSCREEN_FORMAT);
    let tex = gpu.target(size);
    let view = tex.create_view(&Default::default());
    let l = layout(&ctx, input(), ed, st);
    painter.paint(
        &gpu.device,
        &gpu.queue,
        &view,
        size,
        &ctx,
        l,
        ed,
        !st.show_grid_off,
    );
    // The canvas size is known now; let the caller frame the camera.
    prepare(ed);
    let l = layout(&ctx, input(), ed, st);
    painter.paint(
        &gpu.device,
        &gpu.queue,
        &view,
        size,
        &ctx,
        l,
        ed,
        !st.show_grid_off,
    );
    let l = layout(&ctx, input(), ed, st);
    painter.paint(
        &gpu.device,
        &gpu.queue,
        &view,
        size,
        &ctx,
        l,
        ed,
        !st.show_grid_off,
    );
    gpu.read(&tex)
}
