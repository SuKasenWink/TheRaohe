//! 軟鍵盤, 作為獨立元件使用: 不讀寫任何輸入方的狀態
//! 只要 ctx 中有 TextEdit(含 DragValue 內嵌的編輯框) 持有焦點, 自己浮現;
//! 按鍵通過向 ctx 注入 egui 事件與輸入框協作, 每次按鍵後搶回焦點
//! 宿主只需在每幀 widget 繪製之前呼叫 pump_events, 並在繪製後呼叫 show

use egui::{vec2, Align, Button, Context, Event, Id, Key, Layout, Modifiers, Order, Rect, Window};

const KB_ROWS: [&[&str]; 5] = [
    &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"],
    &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"],
    &["a", "s", "d", "f", "g", "h", "j", "k", "l"],
    &["z", "x", "c", "v", "b", "n", "m", ".", "_", "-"],
    &["清除", "退格", "完成"],
];

// 焦點剛離開輸入框時鍵盤不會立刻消失, 緩衝幾幀, 避免點擊外圍時閃爍
const HYSTERESIS_FRAMES: u32 = 20;

#[derive(Default)]
pub struct Keyboard {
    input_widget: Option<Id>,
    needed: u32,
    events: Vec<Event>,
}

impl Keyboard {
    // 注入待辦事件; 必須在任何 widget 之前呼叫, 否則事件會延後一幀
    pub fn pump_events(&mut self, ctx: &Context) {
        if self.events.is_empty() {
            return;
        }
        let events = std::mem::take(&mut self.events);
        ctx.input_mut(|input| input.events.extend(events));
    }

    /// 畫出軟鍵盤, 回傳其 rect 供宿主做命中測試(例如觸控是否落在鍵盤上)
    pub fn show(&mut self, ctx: &Context) -> Option<Rect> {
        self.remember_input_widget(ctx);
        if !self.keyboard_input_needed(ctx) {
            return None;
        }
        let screen = ctx.content_rect();
        let kb_w = (screen.width() - 60.0).min(788.0).max(360.0);
        let kb_h = 368.0_f32.min(screen.height() - 80.0).max(280.0);
        let kb_pos = egui::pos2(
            ((screen.width() - kb_w) * 0.5).max(20.0),
            (screen.height() - kb_h - 30.0).max(20.0),
        );
        let response = Window::new("鍵盤")
            .id(Id::unique("theraohe-keyboard"))
            .resizable(true)
            .movable(true)
            .collapsible(false)
            .order(Order::Foreground)
            .default_pos(kb_pos)
            .default_size(vec2(kb_w, kb_h))
            .min_size(vec2(360.0, 280.0))
            .max_size(vec2(screen.width(), screen.height()))
            .constrain(true)
            .show(ctx, |ui| {
                ui.set_min_width(ui.available_width());
                let width = ui.available_width();
                let gap = 6.0;
                for row in KB_ROWS {
                    ui.allocate_ui_with_layout(
                        vec2(width, 60.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            let count = row.len() as f32;
                            let bw = (ui.available_width() - gap * (count - 1.0)) / count;
                            for key in row {
                                if ui.add_sized([bw, 60.0], Button::new(*key)).clicked() {
                                    self.press_key(ctx, key);
                                }
                            }
                        },
                    );
                }
            });
        response.map(|r| r.response.rect)
    }

    fn remember_input_widget(&mut self, ctx: &Context) {
        if ctx.text_edit_focused() {
            self.input_widget = ctx.memory(|m| m.focused());
        }
    }

    fn keyboard_input_needed(&mut self, ctx: &Context) -> bool {
        let focused = ctx.text_edit_focused();
        if focused {
            self.needed = HYSTERESIS_FRAMES;
            true
        } else {
            self.needed = self.needed.saturating_sub(1);
            let needed = self.needed > 0;
            if needed {
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
            }
            needed
        }
    }

    fn press_key(&mut self, ctx: &Context, key: &str) {
        match key {
            "清除" => {
                self.events.push(key_event(Key::A, Modifiers::COMMAND));
                self.events.push(key_event(Key::Backspace, Modifiers::NONE));
            }
            "退格" => self.events.push(key_event(Key::Backspace, Modifiers::NONE)),
            "完成" => {
                ctx.memory_mut(|m| m.stop_text_input());
                self.needed = 0;
                self.input_widget = None;
                ctx.request_repaint();
                return;
            }
            text => self.events.push(Event::Text(text.to_string())),
        }
        // 點鍵盤會使輸入框失去焦點(egui 默認點擊別處即失去焦點), 按完鍵立刻搶回來
        if let Some(id) = self.input_widget {
            ctx.memory_mut(|m| m.request_focus(id));
        }
        // 讓下一幀一定有機會注入事件/重繪(隱藏鍵盤)
        ctx.request_repaint();
    }
}

fn key_event(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}
