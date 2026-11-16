//! 設置頁的持久化: TOON 文本格式(toon-format), 檔案放在二進制同目錄的 `settings.toon`。
//! 存檔時機只有兩個, 都由 `ui.rs` 觸發:
//! 1. 離開設置頁(UiAction::Tab 從 Settings 切走)
//! 2. 關閉應用(UiAction::ConfirmClose)
//! 讀檔只在進程啟動時做一次。捷徑窗口的位置/大小由使用者拖動, 每幀以 draw_ui 的
//! Window::show 返回的 rect 為單一真相回寫在 group 上, 存檔時直接寫進 group 記錄。
//! 存值本身; 僅 `window` 允許缺席(大組沒被拖動、也沒畫出來時沒有值可存)。

use crate::ui::{AppState, ShortcutGroup};
use egui::{pos2, vec2};
use std::path::PathBuf;

const FILE_NAME: &str = "settings.toon";

fn default_backend() -> String {
    "gpu".to_string()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedSettings {
    zoom: f32,
    refresh_interval: f32,
    hex_refresh_interval: f32,
    // 舊 settings.toon 沒有此欄位時 serde 會以 default 補上 (= GPU, 符合默認)
    #[serde(default = "default_backend")]
    backend: String,
    shortcut_groups: Vec<PersistedGroup>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedGroup {
    name: String,
    rows: Vec<Vec<String>>,
    free_position: bool,
    allow_out_of_bounds: bool,
    window: Option<PersistedWindow>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq)]
struct PersistedWindow {
    pos_x: f32,
    pos_y: f32,
    size_x: f32,
    size_y: f32,
}

pub(crate) struct LoadedSettings {
    pub zoom: f32,
    pub refresh_interval: f32,
    pub hex_refresh_interval: f32,
    pub shortcut_groups: Vec<ShortcutGroup>,
    pub backend: crate::ui::RenderBackend,
    /// 檔案存在但解析/讀寫失敗時的提示, 進程啟動後會反映到狀態欄; 成功載入為 None
    pub note: Option<String>,
}

impl Default for LoadedSettings {
    fn default() -> Self {
        LoadedSettings {
            zoom: 2.0,
            refresh_interval: 0.25,
            hex_refresh_interval: 0.25,
            shortcut_groups: vec![ShortcutGroup::new("捷徑 1")],
            backend: crate::ui::RenderBackend::Gpu,
            note: None,
        }
    }
}

fn file_path() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join(FILE_NAME))
}

pub(crate) fn load() -> LoadedSettings {
    let mut settings = LoadedSettings::default();
    let Some(path) = file_path() else {
        return settings;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return settings,
        Err(e) => {
            settings.note = Some(format!("設置讀取失敗: {e}"));
            return settings;
        }
    };
    match toon_format::decode_default::<PersistedSettings>(&text) {
        Ok(p) => {
            settings.zoom = p.zoom;
            settings.refresh_interval = p.refresh_interval;
            settings.hex_refresh_interval = p.hex_refresh_interval;
            settings.backend = crate::ui::RenderBackend::from_str_lossy(&p.backend);
            // 大組順序由名字決定; 文件可以亂序, 載入時一律排序
            let mut groups: Vec<ShortcutGroup> = p
                .shortcut_groups
                .into_iter()
                .map(|g| {
                    let mut group = ShortcutGroup::new(&g.name);
                    group.rows = g.rows;
                    group.free_position = g.free_position;
                    group.allow_out_of_bounds = g.allow_out_of_bounds;
                    group.window_geom = g.window.map(|w| {
                        egui::Rect::from_min_size(
                            pos2(w.pos_x, w.pos_y),
                            vec2(w.size_x, w.size_y),
                        )
                    });
                    group
                })
                .collect();
            groups.sort_by(|a, b| a.name.cmp(&b.name));
            settings.shortcut_groups = groups;
        }
        Err(e) => {
            // 檔案壞了不阻塞進程: 退回默認值, 並保留原檔供人工檢查
            let _ = std::fs::rename(&path, path.with_extension("toon.bad"));
            settings.note = Some(format!("設置解析失敗, 已切換到默認值: {e}"));
        }
    }
    settings
}

pub(crate) fn save(state: &AppState) {
    let Some(path) = file_path() else {
        eprintln!("[persist] 二進制同目錄不可用, 設置未保存");
        return;
    };
    let persisted = PersistedSettings {
        zoom: state.zoom,
        refresh_interval: state.refresh_interval,
        hex_refresh_interval: state.hex_refresh_interval,
        backend: state
            .pending_backend
            .unwrap_or(state.render_backend)
            .as_str()
            .to_string(),
        shortcut_groups: state
            .shortcut_groups
            .iter()
            .map(|g| PersistedGroup {
                name: g.name.clone(),
                rows: g.rows.clone(),
                free_position: g.free_position,
                allow_out_of_bounds: g.allow_out_of_bounds,
                window: g.window_geom.map(|rect| PersistedWindow {
                    pos_x: rect.min.x,
                    pos_y: rect.min.y,
                    size_x: rect.width(),
                    size_y: rect.height(),
                }),
            })
            .collect(),
    };
    match toon_format::encode_default(&persisted) {
        Ok(text) => {
            let tmp = path.with_extension("toon.tmp");
            match std::fs::write(&tmp, &text).and_then(|()| std::fs::rename(&tmp, &path)) {
                Ok(()) => (),
                Err(e) => eprintln!("[persist] 設置寫入失敗: {e}"),
            }
        }
        Err(e) => eprintln!("[persist] 設置編碼失敗: {e}"),
    }
}
