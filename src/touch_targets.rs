//! 可命名的觸控目標: MCP 可新增/刪除/列出/觸發; GUI 只負責渲染與拖動
//! 目標位置以 egui 點為單位儲存, 按下時依螢幕方向與層偏移換算成觸控裝置原始座標

use std::sync::Mutex;

use egui::{Color32, Stroke, vec2};

use crate::display;
use crate::ui::AppState;

pub struct TouchTarget {
    pub name: String,
    /// 窗口左上 (點)
    pub x: f32,
    pub y: f32,
    /// 十字中心 (點); 每幀由 draw 更新, 換算/顯示一律用它
    pub cx: f32,
    pub cy: f32,
}

static RANGES: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);
static LAYER: Mutex<(f32, f32)> = Mutex::new((0.0, 0.0));
static INITIALIZED: Mutex<bool> = Mutex::new(false);

pub fn set_ranges(x_min: i32, x_max: i32, y_min: i32, y_max: i32) {
    if let Ok(mut r) = RANGES.lock() {
        *r = Some((x_min, x_max, y_min, y_max));
    }
}

pub fn set_layer_offset(x: f32, y: f32) {
    if let Ok(mut l) = LAYER.lock() {
        *l = (x, y);
    }
}

/// 新目標初始化於螢幕中央; 重複名稱回傳 Err
pub fn add(state: &mut AppState, name: &str) -> Result<(f32, f32), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("名稱不可為空".to_string());
    }
    if name.chars().count() > 40 {
        return Err("名稱過長 (上限 40 字元)".to_string());
    }
    if state.touch_targets.iter().any(|t| t.name == name) {
        return Err(format!("已存在名為 {name} 的觸控目標"));
    }
    let (w, h) = display::screen_wh().unwrap_or((1080, 2400));
    let ppp = state.ppp.max(0.01);
    // 依序往右下錯開, 避免新目標與既有的完全重疊
    let offset = ((state.touch_targets.len() as u32) * 26) % 130;
    let cx = (w as f32 / ppp) * 0.5 + offset as f32;
    let cy = (h as f32 / ppp) * 0.5 + offset as f32;
    let t = TouchTarget {
        name: name.to_string(),
        // 錨點初值以十字中心推回, 首幀由 draw 精確更新
        x: cx - CONTENT * 0.5,
        y: cy - CONTENT * 0.5,
        cx,
        cy,
    };
    state.touch_targets.push(t);
    Ok((cx, cy))
}

pub fn remove(state: &mut AppState, name: &str) -> bool {
    let before = state.touch_targets.len();
    state.touch_targets.retain(|t| t.name != name);
    state.touch_targets.len() != before
}

pub fn list(state: &AppState) -> String {
    if state.touch_targets.is_empty() {
        return "無觸控目標".to_string();
    }
    let ppp = state.ppp.max(0.01);
    let layer = LAYER.lock().map(|l| *l).unwrap_or((0.0, 0.0));
    let mut out = format!("共 {} 個觸控目標\n", state.touch_targets.len());
    for t in &state.touch_targets {
        let (cx, cy) = (t.cx, t.cy);
        out.push_str(&format!(
            "{}  pt({:.1}, {:.1})  px({:.0}, {:.0})\n",
            t.name,
            cx,
            cy,
            cx * ppp + layer.0,
            cy * ppp + layer.1,
        ));
    }
    out
}

fn ranges_max() -> (i32, i32) {
    if let Ok(r) = RANGES.lock() {
        if let Some((_, x_max, _, y_max)) = *r {
            return (x_max, y_max);
        }
    }
    display::screen_info()
        .map(|(w, h, _)| (w - 1, h - 1))
        .unwrap_or((1079, 2399))
}

fn ensure_init() -> bool {
    let mut done = match INITIALIZED.lock() {
        Ok(v) => v,
        Err(_) => return false,
    };
    if *done {
        return true;
    }
    let (mx, my) = ranges_max();
    if crate::nb9::touch_init(mx as u32, my as u32) {
        *done = true;
        return true;
    }
    false
}

/// 目標點 (points) → 觸控裝置原始座標; 方向換算與 sf_touch 的 map_touch 互為逆
pub fn to_device(x: f32, y: f32, ppp: f32) -> Option<(u32, u32)> {
    let (w, h, rotation) = display::screen_info()?;
    let nw = if rotation % 2 == 1 { h as f32 } else { w as f32 };
    let nh = if rotation % 2 == 1 { w as f32 } else { h as f32 };
    let (lx, ly) = LAYER
        .lock()
        .ok()
        .map(|l| (x * ppp + l.0, y * ppp + l.1))?;
    let (ux, uy) = match rotation {
        0 => (lx / nw, ly / nh),
        1 => (1.0 - ly / nw, lx / nh),
        2 => (1.0 - lx / nw, 1.0 - ly / nh),
        _ => (ly / nw, 1.0 - lx / nh),
    };
    let (x0, x1, y0, y1) = RANGES
        .lock()
        .ok()
        .and_then(|r| *r)
        .unwrap_or((0, w - 1, 0, h - 1));
    let clamp01 = |v: f32| v.clamp(0.0, 1.0);
    Some((
        (x0 as f32 + clamp01(ux) * (x1 - x0) as f32).round() as u32,
        (y0 as f32 + clamp01(uy) * (y1 - y0) as f32).round() as u32,
    ))
}

/// 在目標當下座標位置按下再抬起; 失敗時回傳原因 (初始化失敗/驅動拒絕/目標不存在)
pub fn press(state: &AppState, name: &str, hold: std::time::Duration) -> Result<(), String> {
    let t = state
        .touch_targets
        .iter()
        .find(|t| t.name == name)
        .ok_or_else(|| format!("找不到名為 {name} 的觸控目標"))?;
    let ppp = state.ppp.max(0.01);
    let (rx, ry) = to_device(t.cx, t.cy, ppp).ok_or_else(|| "螢幕資訊不可用".to_string())?;
    if !ensure_init() {
        return Err("nb9 虛擬觸控初始化失敗 (需要 root 與 RT 驅動)".to_string());
    }
    if !crate::nb9::touch_event(crate::nb9::TOUCH_DOWN, rx, ry, crate::nb9::TOUCH_SLOT_MASK) {
        return Err("觸控按下失敗 (nb9 事件被驅動拒絕)".to_string());
    }
    std::thread::sleep(hold);
    if !crate::nb9::touch_event(crate::nb9::TOUCH_UP, rx, ry, crate::nb9::TOUCH_SLOT_MASK) {
        return Err("觸控釋放失敗 (事件被驅動拒絕)".to_string());
    }
    Ok(())
}

const ACCENT: Color32 = Color32::from_rgb(255, 45, 85);
// 內容方邊 (點): zoom 2.0 下 92 實體像素, 方形佈局
pub const CONTENT: f32 = 46.0;

/// 目標位置 (窗口左上) 與十字中心 (絕對點)
/// 每幀由 draw 同步: 錨點 = Area 左上; 十字中心 = 內容區中心 (畫家畫在內容區上)

pub fn draw(ctx: &egui::Context, state: &mut AppState, hit_rects: &mut Vec<egui::Rect>) {
    // 捷徑窗同款 (半透明黑底、無陰影、前景層); 邊框用預設色 (frame 沿用 window_stroke)
    let mut shortcut_style = (*ctx.style_of(egui::Theme::Dark)).clone();
    shortcut_style.visuals.window_fill = Color32::from_black_alpha(128);
    let frame = egui::Frame::window(&shortcut_style).shadow(egui::Shadow::NONE);
    for idx in 0..state.touch_targets.len() {
        let name = state.touch_targets[idx].name.clone();
        let (px, py) = (state.touch_targets[idx].x, state.touch_targets[idx].y);
        if let Some(resp) = egui::Window::new(&name)
            .id(egui::Id::unique(("theraohe-touch-target", &name)))
            .collapsible(false)
            .resizable(false)
            .movable(true)
            .default_pos(egui::pos2(px, py))
            .default_size(vec2(CONTENT, CONTENT))
            .title_bar(false)
            .frame(frame)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(CONTENT, CONTENT), egui::Sense::hover());
                let c = rect.center();
                let p = ui.painter();
                // 名稱小字放窗口上半部
                p.text(
                    egui::pos2(c.x, rect.top() + 6.0),
                    egui::Align2::CENTER_CENTER,
                    &name,
                    egui::FontId::proportional(7.0),
                    Color32::from_rgb(235, 235, 235),
                );
                // 十字位於下半部中央, 主題色
                let cy = rect.top() + CONTENT * 0.68;
                let half = 10.0_f32;
                p.line_segment(
                    [egui::pos2(rect.left() + 5.0, cy), egui::pos2(rect.right() - 5.0, cy)],
                    Stroke::new(1.5_f32, ACCENT),
                );
                p.line_segment(
                    [egui::pos2(c.x, cy - half), egui::pos2(c.x, cy + half)],
                    Stroke::new(1.5_f32, ACCENT),
                );
                egui::pos2(c.x, cy)
            })
        {
            hit_rects.push(resp.response.rect.expand(8.0));
            state.touch_targets[idx].x = resp.response.rect.min.x;
            state.touch_targets[idx].y = resp.response.rect.min.y;
            if let Some(c) = resp.inner {
                state.touch_targets[idx].cx = c.x;
                state.touch_targets[idx].cy = c.y;
            }
        }
    }
}
