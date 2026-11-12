use ab_glyph::{point, Font, FontVec, Glyph, PxScale, ScaleFont};

pub struct OverlayRenderer {
    font: FontVec,
    title: String,
    title_px: f32,
    label_px: f32,
}

pub fn rect_bounds(width: f32, height: f32, large: bool) -> [f32; 4] {
    let (cx, cy, rw, rh) = rect_geometry(width, height, large);
    [
        cx - rw * 0.5 - 6.0,
        cy - rh * 0.5 - 6.0,
        cx + rw * 0.5 + 6.0,
        cy + rh * 0.5 + 6.0,
    ]
}

fn rect_geometry(width: f32, height: f32, large: bool) -> (f32, f32, f32, f32) {
    let (rw, rh) = if large {
        (width * 0.68, height * 0.48)
    } else {
        (width * 0.45, height * 0.24)
    };
    (width * 0.5, height * 0.57, rw, rh)
}

impl OverlayRenderer {
    pub fn load(font_path: &str, font_index: Option<u32>, title: &str) -> Result<Self, String> {
        let candidates: Vec<u32> = match font_index {
            Some(i) => vec![i],
            None => vec![0, 2, 1, 3, 4],
        };
        let mut last_err = String::from("no candidates");
        let mut chosen = None;
        for index in candidates {
            let data = match std::fs::read(font_path) {
                Ok(v) => v,
                Err(e) => return Err(format!("read {font_path}: {e}")),
            };
            match FontVec::try_from_vec_and_index(data, index) {
                Ok(font) => {
                    if has_glyphs(&font, title) && has_glyphs(&font, "Rect") {
                        chosen = Some(font);
                        break;
                    }
                    last_err = format!("font index {index} missing required glyphs");
                }
                Err(e) => last_err = format!("font index {index}: {e:?}"),
            }
        }
        let font = chosen.ok_or(last_err)?;
        Ok(Self {
            font,
            title: title.to_string(),
            title_px: 28.0,
            label_px: 22.0,
        })
    }

    pub fn draw(&self, buf: &mut [u32], width: usize, height: usize, stride: usize, large: bool) {
        for px in buf.iter_mut().take(height.saturating_mul(stride)) {
            *px = 0;
        }
        if width == 0 || height == 0 {
            return;
        }

        let w = width as f32;
        let h = height as f32;
        let panel = [10u8, 12u8, 18u8, 214u8];
        let border = [64u8, 84u8, 124u8, 235u8];
        fill_round_rect(
            buf,
            width,
            height,
            stride,
            2.0,
            2.0,
            w - 4.0,
            h - 4.0,
            28.0,
            border,
        );
        fill_round_rect(
            buf,
            width,
            height,
            stride,
            4.0,
            4.0,
            w - 8.0,
            h - 8.0,
            26.0,
            panel,
        );

        self.draw_rect(buf, width, height, stride, large);

        let title_baseline = 56.0;
        draw_text_centered(
            &self.font,
            buf,
            width,
            height,
            stride,
            &self.title,
            self.title_px,
            title_baseline,
            [238u8, 244u8, 255u8, 255u8],
        );

        let label_baseline = h - 18.0;
        draw_text_centered(
            &self.font,
            buf,
            width,
            height,
            stride,
            "Rect",
            self.label_px,
            label_baseline,
            [184u8, 202u8, 235u8, 235u8],
        );
    }

    fn draw_rect(&self, buf: &mut [u32], width: usize, height: usize, stride: usize, large: bool) {
        let (cx, cy, rw, rh) = rect_geometry(width as f32, height as f32, large);
        let x = cx - rw * 0.5;
        let y = cy - rh * 0.5;
        let radius = rh.min(rw) * 0.18;
        fill_round_rect(
            buf,
            width,
            height,
            stride,
            x,
            y,
            rw,
            rh,
            radius,
            [222u8, 240u8, 255u8, 220u8],
        );
        fill_round_rect_gradient(
            buf,
            width,
            height,
            stride,
            x + 2.0,
            y + 2.0,
            rw - 4.0,
            rh - 4.0,
            radius - 2.0,
            [62u8, 196u8, 255u8, 248u8],
            [174u8, 82u8, 246u8, 248u8],
        );
        draw_line(
            buf,
            width,
            height,
            stride,
            x + radius + 8.0,
            y + 7.0,
            x + rw - radius - 8.0,
            y + 7.0,
            [255u8, 255u8, 255u8, 95u8],
        );
        draw_line(
            buf,
            width,
            height,
            stride,
            x + radius + 8.0,
            y + rh - 7.0,
            x + rw - radius - 8.0,
            y + rh - 7.0,
            [24u8, 16u8, 72u8, 90u8],
        );
    }
}

fn has_glyphs(font: &FontVec, text: &str) -> bool {
    text.chars().all(|c| c == ' ' || font.glyph_id(c).0 != 0)
}

fn text_width(font: &FontVec, text: &str, px: f32) -> f32 {
    let scaled = font.as_scaled(px);
    let mut x = 0.0f32;
    let mut prev = None;
    for ch in text.chars() {
        let id = scaled.glyph_id(ch);
        if let Some(p) = prev {
            x += scaled.kern(p, id);
        }
        x += scaled.h_advance(id);
        prev = Some(id);
    }
    x
}

fn draw_text_centered(
    font: &FontVec,
    buf: &mut [u32],
    width: usize,
    height: usize,
    stride: usize,
    text: &str,
    px: f32,
    baseline: f32,
    color: [u8; 4],
) {
    let left = (width as f32 - text_width(font, text, px)) * 0.5;
    draw_text_at(
        font,
        buf,
        width,
        height,
        stride,
        text,
        px,
        left + 1.0,
        baseline + 2.0,
        [0u8, 0u8, 0u8, 160u8],
    );
    draw_text_at(
        font, buf, width, height, stride, text, px, left, baseline, color,
    );
}

fn draw_text_at(
    font: &FontVec,
    buf: &mut [u32],
    width: usize,
    height: usize,
    stride: usize,
    text: &str,
    px: f32,
    left: f32,
    baseline: f32,
    color: [u8; 4],
) {
    let scaled = font.as_scaled(px);
    let scale = PxScale::from(px);
    let mut x = left;
    let mut prev = None;
    for ch in text.chars() {
        let id = scaled.glyph_id(ch);
        if let Some(p) = prev {
            x += scaled.kern(p, id);
        }
        let glyph = Glyph {
            id,
            scale,
            position: point(x, baseline),
        };
        if let Some(outline) = scaled.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            let ox = bounds.min.x as i32;
            let oy = bounds.min.y as i32;
            outline.draw(|gx, gy, coverage| {
                blend(
                    buf,
                    width,
                    height,
                    stride,
                    ox + gx as i32,
                    oy + gy as i32,
                    color,
                    coverage,
                );
            });
        }
        x += scaled.h_advance(id);
        prev = Some(id);
    }
}

fn fill_round_rect(
    buf: &mut [u32],
    width: usize,
    height: usize,
    stride: usize,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    color: [u8; 4],
) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let x1 = (x + w).ceil() as i32;
    let y1 = (y + h).ceil() as i32;
    let r = radius.min(w * 0.5).min(h * 0.5);
    for py in y0..y1 {
        for px in x0..x1 {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;
            if fx < x || fx > x + w || fy < y || fy > y + h {
                continue;
            }
            let cx = fx.clamp(x + r, x + w - r);
            let cy = fy.clamp(y + r, y + h - r);
            let dx = fx - cx;
            let dy = fy - cy;
            if dx * dx + dy * dy <= r * r {
                blend(buf, width, height, stride, px, py, color, 1.0);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_round_rect_gradient(
    buf: &mut [u32],
    width: usize,
    height: usize,
    stride: usize,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    left: [u8; 4],
    right: [u8; 4],
) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let x1 = (x + w).ceil() as i32;
    let y1 = (y + h).ceil() as i32;
    let r = radius.min(w * 0.5).min(h * 0.5);
    for py in y0..y1 {
        for px in x0..x1 {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;
            if fx < x || fx > x + w || fy < y || fy > y + h {
                continue;
            }
            let cx = fx.clamp(x + r, x + w - r);
            let cy = fy.clamp(y + r, y + h - r);
            let dx = fx - cx;
            let dy = fy - cy;
            if dx * dx + dy * dy <= r * r {
                let t = ((fx - x) / w).clamp(0.0, 1.0);
                let color = [
                    mix(left[0], right[0], t),
                    mix(left[1], right[1], t),
                    mix(left[2], right[2], t),
                    mix(left[3], right[3], t),
                ];
                blend(buf, width, height, stride, px, py, color, 1.0);
            }
        }
    }
}

fn mix(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

#[allow(clippy::too_many_arguments)]
fn draw_line(
    buf: &mut [u32],
    width: usize,
    height: usize,
    stride: usize,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    color: [u8; 4],
) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let steps = dx.abs().max(dy.abs()).ceil() as i32;
    if steps <= 0 {
        blend(buf, width, height, stride, x0 as i32, y0 as i32, color, 1.0);
        return;
    }
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        blend(
            buf,
            width,
            height,
            stride,
            (x0 + dx * t) as i32,
            (y0 + dy * t) as i32,
            color,
            1.0,
        );
    }
}

fn blend(
    buf: &mut [u32],
    width: usize,
    height: usize,
    stride: usize,
    x: i32,
    y: i32,
    color: [u8; 4],
    coverage: f32,
) {
    if x < 0 || y < 0 || x as usize >= width || y as usize >= height {
        return;
    }
    let idx = y as usize * stride + x as usize;
    if idx >= buf.len() {
        return;
    }
    let cov = coverage.clamp(0.0, 1.0);
    let sa = (color[3] as f32 * cov).round() as u32;
    if sa == 0 {
        return;
    }
    let sr = (color[0] as u32 * sa + 127) / 255;
    let sg = (color[1] as u32 * sa + 127) / 255;
    let sb = (color[2] as u32 * sa + 127) / 255;
    let inv = 255 - sa;
    let dst = buf[idx];
    let dr = dst & 0xff;
    let dg = (dst >> 8) & 0xff;
    let db = (dst >> 16) & 0xff;
    let da = (dst >> 24) & 0xff;
    let r = sr + (dr * inv + 127) / 255;
    let g = sg + (dg * inv + 127) / 255;
    let b = sb + (db * inv + 127) / 255;
    let a = sa + (da * inv + 127) / 255;
    buf[idx] = r.min(255) | (g.min(255) << 8) | (b.min(255) << 16) | (a.min(255) << 24);
}
