//! 軟鍵盤, 作為獨立元件使用: 不讀寫任何輸入方的狀態
//! 只要 ctx 中有 TextEdit(含 DragValue 內嵌的編輯框) 持有焦點, 自己浮現;
//! 按鍵通過向 ctx 注入 egui 事件與輸入框協作, 每次按鍵後搶回焦點
//! 宿主只需在每幀 widget 繪製之前呼叫 pump_events, 並在繪製後呼叫 show

use egui::{vec2, Vec2, Button, Context, Event, Id, Key, Modifiers, Order, ProgressBar, Rect, RichText, ScrollArea, Window};
use std::time::{Duration, Instant};

/// 應用內剪切板: 複製內容以浮窗呈現, 點擊內容延長 30 秒; 倒計時結束即離開作用域
/// 窗口位置由 egui Memory 記住, 關閉重開位置不變; 宿主只需 copy + show, 無其他耦合
pub struct Clipboard {
    entry: Option<ClipEntry>,
}

struct ClipEntry {
    text: String,
    expires: Instant,
}

const CLIP_TTL_SECS: u64 = 30;

impl Default for Clipboard {
    fn default() -> Self {
        Self { entry: None }
    }
}

impl Clipboard {
    pub fn copy(&mut self, text: String) {
        self.entry = Some(ClipEntry {
            text,
            expires: Instant::now() + Duration::from_secs(CLIP_TTL_SECS),
        });
    }

    fn clear_expired(&mut self) {
        if self.entry.as_ref().is_some_and(|e| e.expires <= Instant::now()) {
            self.entry = None;
        }
    }

    /// 畫剪切板窗口; 回傳其 rect 供宿主做命中測試
    pub fn show(&mut self, ctx: &Context) -> Option<Rect> {
        self.clear_expired();
        // 位置由 egui Memory 記住, 窗口關閉後重開位置不變
        let ClipEntry { text, expires } = self.entry.as_mut()?;
        let remain = expires.saturating_duration_since(Instant::now());
        let secs = remain.as_secs_f32();
        let frac = (secs / CLIP_TTL_SECS as f32).clamp(0.0, 1.0);
        let screen = ctx.content_rect();
        let response = Window::new("剪切板")
            .id(Id::unique("theraohe-clipboard"))
            .resizable(false)
            .movable(true)
            .collapsible(true)
            .default_pos(egui::pos2(screen.width() * 0.05, screen.height() * 0.15))
            .order(Order::Foreground)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add(Button::new(RichText::new(&*text).monospace().size(20.0)))
                        .clicked()
                    {
                        *expires += Duration::from_secs(CLIP_TTL_SECS);
                    }
                    ui.label(RichText::new(format!("{}s", secs.ceil() as u32)).monospace().small());
                });
                ui.add(ProgressBar::new(frac).desired_height(3.0));
            });
        response.map(|r| r.response.rect)
    }
}

// 焦點從輸入框離開時鍵盤會立即消失; 但鍵盤按鍵本身會讓輸入框失焦一瞬間,
// 留一點幀寬避免在 tap 幀自殺
const HYSTERESIS_FRAMES: u32 = 2;

const KEYBOARD_ID: &str = "theraohe-keyboard";
const EMOJI_FONT_NAME: &str = "TheRaohe-SystemEmoji";
const EMOJI_COLS: usize = 8;
const ROW_COUNT: f32 = 4.0;
const NUMERALS_W: f32 = 0.65;
const NUMERALS_H: f32 = 1.25;

const LETTERS_ROW0: &[char] = &['q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p'];
const LETTERS_ROW1: &[char] = &['a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l'];
const LETTERS_ROW2: &[char] = &['z', 'x', 'c', 'v', 'b', 'n', 'm'];
const NUMERALS_ROWS: &[&[(&str, KbKey)]] = &[
    &[("1", KbKey::Symbol("1")), ("2", KbKey::Symbol("2")), ("3", KbKey::Symbol("3")), ("-", KbKey::Symbol("-"))],
    &[("4", KbKey::Symbol("4")), ("5", KbKey::Symbol("5")), ("6", KbKey::Symbol("6")), ("空格", KbKey::Space)],
    &[("7", KbKey::Symbol("7")), ("8", KbKey::Symbol("8")), ("9", KbKey::Symbol("9")), ("←", KbKey::Backspace)],
    &[("abc", KbKey::ToLetters), ("0", KbKey::Symbol("0")), (".", KbKey::Symbol(".")), ("→", KbKey::Confirm)],
];

// 系統 emoji 候選路徑; 裝第一個能讀到的, 不維護可用清單
const EMOJI_FONT_CANDIDATES: &[&str] = &[
    "/system/fonts/NotoColorEmoji.ttf",
    "/product/fonts/NotoColorEmoji.ttf",
    "/system/fonts/NotoColorEmoji_17.ttf",
    "/system/fonts/NotoColorEmoji-subset.ttf",
];

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum KbMode {
    #[default]
    Letters,
    Numerals,
    Emoji,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Shift {
    #[default]
    Off,
    Once,
    Lock,
}

#[derive(Clone, Copy)]
enum KbKey {
    Letter(char),
    Symbol(&'static str),
    EmojiChar(char),
    Space,
    Backspace,
    Shift,
    Confirm,
    ToNumerals,
    ToLetters,
    ToEmoji,
}

#[derive(Default)]
pub struct Keyboard {
    input_widget: Option<Id>,
    needed: u32,
    events: Vec<Event>,
    mode: KbMode,
    shift: Shift,
    last_rect: Option<Rect>,
    pending_resize: Option<Vec2>,
    emoji_font_attempted: bool,
    emojis: Option<Vec<char>>,
}

impl Keyboard {
    // 注入待辦事件; 必須在任何 widget 之前呼叫, 否則事件會延後一幀
    pub fn pump_events(&mut self, ctx: &Context) {
        // egui 會在 TextEdit 失焦的每個 pass 結束時把選取折疊成單一游標;
        // 鍵盤按鍵會讓輸入框失焦, 待處理事件存在時若已失焦, 必須在事件被處理的那次 paint 之前搶回焦點, 選取才不會被折疊
        if !self.events.is_empty() && !ctx.text_edit_focused() {
            if let Some(id) = self.input_widget {
                ctx.memory_mut(|m| m.request_focus(id));
            }
        }
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
        self.install_emoji_font_once(ctx);
        let screen = ctx.content_rect();
        let kb_w = (screen.width() - 60.0).min(788.0).max(360.0);
        let kb_h = 368.0_f32.min(screen.height() - 80.0).max(280.0);
        let kb_pos = egui::pos2(
            ((screen.width() - kb_w) * 0.5).max(20.0),
            (screen.height() - kb_h - 30.0).max(20.0),
        );
        let mut window = Window::new("鍵盤")
            .id(Id::unique(KEYBOARD_ID))
            .resizable(true)
            .movable(true)
            .collapsible(false)
            .order(Order::Foreground)
            .default_pos(kb_pos)
            .default_size(vec2(kb_w, kb_h))
            .min_size(vec2(234.0, 280.0))
            .max_size(vec2(screen.width(), screen.height()))
            .constrain(true);
        // 切換鍵盤型態的那一幀用 fixed_size 強制改變尺寸, 使 Resize 狀態直接持久化,
        // 之後照常可拖動; 數字鍵盤 = 字母 X 65% / Y 125%, 切回時逆回去
        if let Some(size) = self.pending_resize.take() {
            window = window.resize(|r| r.fixed_size(size));
        }
        let response = window.show(ctx, |ui| {
                ui.set_min_width(ui.available_width());
                let spacing = ui.spacing().item_spacing.x;
                match self.mode {
                    KbMode::Emoji => {
                        self.show_emoji(ctx, ui, spacing);
                    }
                    KbMode::Letters | KbMode::Numerals => {
                        let row_height = ((ui.available_height()
                            - ui.spacing().item_spacing.y * (ROW_COUNT - 1.0))
                            / ROW_COUNT)
                            .max(0.0);
                        match self.mode {
                            KbMode::Letters => self.show_letters(ctx, ui, spacing, row_height),
                            _ => self.show_numerals(ctx, ui, row_height),
                        }
                    }
                }
            });
        response.map(|r| {
            let rect = r.response.rect;
            self.last_rect = Some(rect);
            // 指針在鍵盤內時每個 pass 都搶回焦點: egui 會在 TB 失焦的任何 pass 折疊選取,
            // 搶回後整個會話都不會再失焦
            if r.response.container_contains_pointer() {
                self.focus_back_to_input_widget(ctx);
            }
            rect
        })
    }

    fn show_letters(&mut self, ctx: &Context, ui: &mut egui::Ui, spacing: f32, row_height: f32) {
        let w = ui.available_width();
        let u = (w - spacing * 9.0) / 10.0;
        // q w e r t y u i o p: 單鍵寬度基準
        ui.allocate_ui(vec2(w, row_height), |ui| {
            ui.spacing_mut().item_spacing.x = spacing;
            ui.horizontal(|ui| {
                for &k in LETTERS_ROW0 {
                    self.key(ui, ctx, self.letter_label(k), KbKey::Letter(k), u, row_height);
                }
            });
        });
        // a s d f g h j k l: qwe 闊度, 兩側留空置中
        ui.allocate_ui(vec2(w, row_height), |ui| {
            let child_w = 9.0 * u + 8.0 * spacing;
            let leading = ((w - child_w) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(leading);
                ui.spacing_mut().item_spacing.x = spacing;
                for &k in LETTERS_ROW1 {
                    self.key(ui, ctx, self.letter_label(k), KbKey::Letter(k), u, row_height);
                }
            });
        });
        // ⇧ z x c v b n m ←: 字母區用 qwe 闊度, 兩側控制鍵填滿剩餘闊度
        ui.allocate_ui(vec2(w, row_height), |ui| {
            let ctrl = (w - 7.0 * u - 8.0 * spacing) / 2.0;
            ui.spacing_mut().item_spacing.x = spacing;
            ui.horizontal(|ui| {
                self.shift_key(ui, ctx, ctrl, row_height);
                for &k in LETTERS_ROW2 {
                    self.key(ui, ctx, self.letter_label(k), KbKey::Letter(k), u, row_height);
                }
                self.key(ui, ctx, "←".to_string(), KbKey::Backspace, ctrl, row_height);
            });
        });
        // ?123 , 😊 空格 . →: 空格佔 50% 行寬
        ui.allocate_ui(vec2(w, row_height), |ui| {
            let space_w = 0.5 * w;
            let w1 = ((w - space_w) - 5.0 * spacing) / 5.0;
            ui.spacing_mut().item_spacing.x = spacing;
            ui.horizontal(|ui| {
                self.key(ui, ctx, "?123".to_string(), KbKey::ToNumerals, w1, row_height);
                self.key(ui, ctx, ",".to_string(), KbKey::Symbol(","), w1, row_height);
                self.key(ui, ctx, "😊".to_string(), KbKey::ToEmoji, w1, row_height);
                self.key(ui, ctx, "空格".to_string(), KbKey::Space, space_w, row_height);
                self.key(ui, ctx, ".".to_string(), KbKey::Symbol("."), w1, row_height);
                self.key(ui, ctx, "→".to_string(), KbKey::Confirm, w1, row_height);
            });
        });
    }

    fn show_numerals(&mut self, ctx: &Context, ui: &mut egui::Ui, row_height: f32) {
        let w = ui.available_width();
        for row in NUMERALS_ROWS {
            ui.allocate_ui(vec2(w, row_height), |ui| {
                ui.columns(row.len(), |cols| {
                    for (col, &(label, key)) in cols.iter_mut().zip(row.iter()) {
                        let clicked = col
                            .centered_and_justified(|ui| ui.button(label).clicked())
                            .inner;
                        if clicked {
                            self.press_key(ctx, key);
                        }
                    }
                });
            });
        }
    }

    fn show_emoji(&mut self, ctx: &Context, ui: &mut egui::Ui, spacing: f32) {
        let w = ui.available_width();
        let row_height = ui.spacing().interact_size.y;
        let emojis = self.emojis(ctx);
        if emojis.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("沒有找到系統 emoji 字體");
            });
        } else {
            ScrollArea::vertical()
                .max_height(ui.available_height() - row_height - spacing)
                .show(ui, |ui| {
                    for chunk in emojis.chunks(EMOJI_COLS) {
                        ui.columns(EMOJI_COLS, |cols| {
                            for (col, &ch) in cols.iter_mut().zip(chunk.iter()) {
                                let cw = col.available_width();
                                let size = (cw * 0.45).clamp(14.0, 28.0);
                                let label = RichText::new(ch.to_string()).size(size);
                                if col.add_sized([cw, cw], Button::new(label)).clicked() {
                                    self.press_key(ctx, KbKey::EmojiChar(ch));
                                }
                            }
                        });
                    }
                });
        }
        ui.allocate_ui(vec2(w, row_height), |ui| {
            if ui.centered_and_justified(|ui| ui.button("ABC").clicked()).inner {
                self.press_key(ctx, KbKey::ToLetters);
            }
        });
    }

    fn key(&mut self, ui: &mut egui::Ui, ctx: &Context, label: String, key: KbKey, width: f32, height: f32) {
        if ui.add_sized([width, height], Button::new(label)).clicked() {
            self.press_key(ctx, key);
        }
    }

    fn shift_key(&mut self, ui: &mut egui::Ui, ctx: &Context, width: f32, height: f32) {
        let (label, lock) = match self.shift {
            Shift::Off | Shift::Once => ("⇧", false),
            Shift::Lock => ("⬆", true),
        };
        let label = if lock {
            RichText::new(label).family(egui::FontFamily::Monospace)
        } else {
            RichText::new(label)
        };
        let enabled = self.shift != Shift::Off;
        if ui.add_sized([width, height], Button::selectable(enabled, label)).clicked() {
            self.press_key(ctx, KbKey::Shift);
        }
    }

    fn letter_label(&self, ch: char) -> String {
        match self.shift {
            Shift::Off => ch.to_string(),
            Shift::Once | Shift::Lock => ch.to_uppercase().collect(),
        }
    }

    fn install_emoji_font_once(&mut self, ctx: &Context) {
        if self.emoji_font_attempted {
            return;
        }
        self.emoji_font_attempted = true;
        for path in EMOJI_FONT_CANDIDATES {
            let Ok(bytes) = std::fs::read(path) else { continue };
            ctx.add_font(egui::FontInsert {
                name: EMOJI_FONT_NAME.into(),
                data: egui::FontData::from_owned(bytes),
                families: vec![
                    egui::InsertFontFamily {
                        family: egui::FontFamily::Name(EMOJI_FONT_NAME.into()),
                        priority: egui::FontPriority::Lowest,
                    },
                    egui::InsertFontFamily {
                        family: egui::FontFamily::Proportional,
                        priority: egui::FontPriority::Lowest,
                    },
                    egui::InsertFontFamily {
                        family: egui::FontFamily::Monospace,
                        priority: egui::FontPriority::Lowest,
                    },
                ],
            });
            break;
        }
    }

    // 從系統 emoji 字體探測可用碼位; 沒有就空著, 不自造清單
    fn emojis(&mut self, ctx: &Context) -> Vec<char> {
        if self.emojis.is_none() {
            let charts = ctx.fonts_mut(|f| {
                f.characters(&egui::FontFamily::Name(EMOJI_FONT_NAME.into()))
                    .keys()
                    .copied()
                    .filter(|&c| {
                        let u = c as u32;
                        u >= 0x00A1 && !(0xFE00..=0xFE0F).contains(&u) && u != 0x200D
                    })
                    .collect::<Vec<_>>()
            });
            self.emojis = Some(charts);
        }
        self.emojis.clone().unwrap_or_default()
    }

    fn switch_mode(&mut self, ctx: &Context, target: KbMode) {
        if self.mode == target {
            return;
        }
        let new_size = match (self.mode, target) {
            (KbMode::Letters | KbMode::Emoji, KbMode::Numerals) => self.last_rect.map(|rect| {
                vec2(rect.width() * NUMERALS_W, rect.height() * NUMERALS_H)
            }),
            (KbMode::Numerals, KbMode::Letters | KbMode::Emoji) => self.last_rect.map(|rect| {
                vec2(rect.width() / NUMERALS_W, rect.height() / NUMERALS_H)
            }),
            _ => None,
        };
        if let Some(size) = new_size {
            let max = ctx.content_rect().size() - vec2(24.0, 24.0);
            self.pending_resize = Some(size.min(max));
        }
        self.mode = target;
    }

    fn remember_input_widget(&mut self, ctx: &Context) {
        if ctx.text_edit_focused() {
            self.input_widget = ctx.memory(|m| m.focused());
        }
    }

    fn focus_back_to_input_widget(&mut self, ctx: &Context) {
        if let Some(id) = self.input_widget {
            ctx.memory_mut(|m| m.request_focus(id));
        }
    }

    fn keyboard_input_needed(&mut self, ctx: &Context) -> bool {
        let needed = if ctx.text_edit_focused() {
            self.needed = HYSTERESIS_FRAMES;
            true
        } else {
            self.needed = self.needed.saturating_sub(1);
            self.needed > 0
        };
        if needed {
            ctx.request_repaint();
        }
        needed
    }

    fn press_key(&mut self, ctx: &Context, key: KbKey) {
        match key {
            KbKey::Letter(c) => {
                let text: String = match self.shift {
                    Shift::Off => c.to_string(),
                    Shift::Once => {
                        self.shift = Shift::Off;
                        c.to_uppercase().collect()
                    }
                    Shift::Lock => c.to_uppercase().collect(),
                };
                self.events.push(Event::Text(text));
            }
            KbKey::Symbol(s) => self.events.push(Event::Text(s.to_string())),
            KbKey::EmojiChar(c) => self.events.push(Event::Text(c.to_string())),
            KbKey::Space => self.events.push(Event::Text(" ".to_string())),
            KbKey::Backspace => self.events.push(key_event(Key::Backspace, Modifiers::NONE)),
            KbKey::Shift => {
                self.shift = match self.shift {
                    Shift::Off => Shift::Once,
                    Shift::Once => Shift::Lock,
                    Shift::Lock => Shift::Off,
                };
            }
            KbKey::Confirm => {
                ctx.memory_mut(|m| m.stop_text_input());
                self.needed = 0;
                self.input_widget = None;
                ctx.request_repaint();
                return;
            }
            KbKey::ToNumerals => self.switch_mode(ctx, KbMode::Numerals),
            KbKey::ToLetters => self.switch_mode(ctx, KbMode::Letters),
            KbKey::ToEmoji => self.switch_mode(ctx, KbMode::Emoji),
        }
        // 鍵盤按鍵會讓輸入框失焦(egui 默認點擊別處即失去焦點), 按完鍵立刻搶回來;
        // 指針在鍵盤上時 show() 每個 pass 也會補一次
        self.needed = HYSTERESIS_FRAMES;
        self.focus_back_to_input_widget(ctx);
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

#[cfg(test)]
mod tests {
    use super::*;
    use egui::RawInput;

    fn screen() -> Rect {
        Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(400.0, 320.0))
    }

    #[allow(clippy::too_many_arguments)]
    fn pass(
        ctx: &Context,
        tb_id: Id,
        text: &mut String,
        kb: &mut Keyboard,
        events: Vec<Event>,
        outclick: bool,
        press: Option<KbKey>,
    ) {
        let out = ctx.run_ui(
            RawInput {
                screen_rect: Some(screen()),
                focused: true,
                events,
                ..Default::default()
            },
            |ui| {
                kb.pump_events(ui.ctx());
                ui.add(egui::TextEdit::singleline(text).id(tb_id));
                if outclick {
                    ui.ctx().memory_mut(|m| m.surrender_focus(tb_id));
                }
                let _ = kb.show(ui.ctx());
                if let Some(k) = press {
                    kb.press_key(ui.ctx(), k);
                }
            },
        );
        out.drop_without_applying_deltas();
    }

    #[test]
    fn keyboard_text_replaces_selection() {
        let ctx = Context::default();
        ctx.set_fonts(egui::FontDefinitions::empty());
        let tb_id = Id::unique("test-tb");
        let mut text = String::from("1234");
        let mut kb = Keyboard::default();

        // pass 1: ?? TB
        pass(&ctx, tb_id, &mut text, &mut kb, vec![], false, None);
        ctx.memory_mut(|m| m.request_focus(tb_id));

        // pass 2: Ctrl+A ??
        pass(
            &ctx,
            tb_id,
            &mut text,
            &mut kb,
            vec![key_event(Key::A, Modifiers::COMMAND)],
            false,
            None,
        );

        // pass 3: ?? tap(TB ?? pass ??); ??????, ?? "5"
        pass(&ctx, tb_id, &mut text, &mut kb, vec![], true, Some(KbKey::Letter('5')));

        // pass 4: pump ??; TB ?? selection ??????
        pass(&ctx, tb_id, &mut text, &mut kb, vec![], false, None);

        println!("text after keyboard input: {text:?}");
        assert_eq!(text, "5", "selection should be replaced");
    }
}
