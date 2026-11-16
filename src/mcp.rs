//! MCP server (Streamable HTTP): 讓 agent 可直連進程驅動動作
//! 僅本機直連 (127.0.0.1), 無認證; Streamable HTTP 以 application/json 回應
//! 工具: screenshot / status / procs / attach / scan / refine / results / selection / read
//! 可選外掛: 依賴方向 mcp -> ui (UiAction 事件流), ui 對本模組一無所知
//! 寫操作一律走 ui::apply_actions 與 GUI 同一條派發路徑, MCP 只產出事件 + 唯讀觀察

use crate::mem::{self, entry_key, ChangeMode, ValueType};
use crate::ui::{self, AppState, Tab, TypeSlot, UiAction};
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters, ServerHandler};
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{tool, tool_handler, tool_router, ErrorData};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(15);
const SCAN_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ProcsParams {
    /// 包含系統行程 (預設只顯示用戶應用)
    #[serde(default)]
    all: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct AttachParams {
    /// 行程 PID (見 procs)
    pid: i32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct TapParams {
    /// 點擊位置的 X 座標 (物理 px, 如同 adb shell input tap; 寬度 0..2399)
    x: f32,
    /// 點擊位置的 Y 座標 (物理 px; 高度 0..1079)
    y: f32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ReadParams {
    /// 位址 (十六進位, 可含 0x 前綴)
    addr: String,
    /// u8/u16/u32/u64/i8/i16/i32/i64/f32/f64/bool/bytes (預設 u64); bytes 模式用 len 指定長度
    #[serde(default)]
    kind: Option<String>,
    /// bytes 模式讀取長度 (預設 64, 上限 256)
    #[serde(default)]
    len: Option<usize>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ScanParams {
    /// 目標值; bool 用 0/1
    value: String,
    /// 類型: i32/u32/i64/u64/i16/u16/f32/f64/bool/bit/u8/ptr; 省略則沿用 UI 當前選擇
    #[serde(rename = "type", default)]
    r#type: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RefineParams {
    /// 篩選模式: any_change / unchanged / increased / decreased / increased_by / decreased_by / exact_value
    mode: String,
    /// increased_by / decreased_by / exact_value 必填
    value: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ResultsParams {
    /// 偏移筆數 (預設 0)
    offset: Option<usize>,
    /// 顯示筆數 (預設 50, 上限 500)
    limit: Option<usize>,
}

/// 讀取並解碼 String 預覽: UTF-8 直接 lossy, UTF-16 以 LE u16 後 lossy; 失敗回 None
fn string_preview(pid: i32, ty: ValueType, addr: u64, len: u16) -> Option<String> {
    if len == 0 {
        return None;
    }
    let buf = crate::mem::hex_dump(pid, addr, len as usize);
    if buf.is_empty() {
        return None;
    }
    match ty {
        ValueType::StringUtf8 => Some(String::from_utf8_lossy(&buf).chars().take(48).collect()),
        ValueType::StringUtf16 => {
            let units: Vec<u16> = buf
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            Some(String::from_utf16_lossy(&units).chars().take(48).collect())
        }
        _ => None,
    }
}

fn parse_type(s: &str) -> Option<(TypeSlot, bool)> {
    match s.to_ascii_lowercase().as_str() {
        "i32" => Some((TypeSlot::S32, true)),
        "u32" => Some((TypeSlot::S32, false)),
        "i64" => Some((TypeSlot::S64, true)),
        "u64" => Some((TypeSlot::S64, false)),
        "i16" => Some((TypeSlot::S16, true)),
        "u16" => Some((TypeSlot::S16, false)),
        "f32" => Some((TypeSlot::F32, true)),
        "f64" | "double" => Some((TypeSlot::F64, true)),
        "bool" => Some((TypeSlot::Bool, true)),
        "bit" | "boolbit" => Some((TypeSlot::Bit, true)),
        "u8" => Some((TypeSlot::U8, true)),
        "ptr" => Some((TypeSlot::Ptr, true)),
        "range" => Some((TypeSlot::Range, true)),
        "str16" | "string" | "utf16" => Some((TypeSlot::Utf16, true)),
        "str8" | "utf8" => Some((TypeSlot::Utf8, true)),
        _ => None,
    }
}

async fn wait_idle(app: &Arc<Mutex<AppState>>, timeout: Duration) -> Result<(), ErrorData> {
    let deadline = Instant::now() + timeout;
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        {
            let state = app
                .lock()
                .map_err(|_| ErrorData::internal_error("app state lock failed", None))?;
            if !state.scanning {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(ErrorData::internal_error(
                "掃描超時, 背景可能仍在進行",
                None,
            ));
        }
    }
}

#[derive(Clone)]
struct Theraohe {
    app: Arc<Mutex<AppState>>,
    tool_router: ToolRouter<Self>,
}

impl Theraohe {
    fn lock(&self) -> Result<MutexGuard<'_, AppState>, ErrorData> {
        self.app
            .lock()
            .map_err(|_| ErrorData::internal_error("app state lock failed", None))
    }

    fn apply(&self, actions: Vec<UiAction>) {
        ui::apply_actions(&self.app, actions);
    }
}

#[tool_router(router = tool_router)]
impl Theraohe {
    #[tool(description = "對整個螢幕(含 overlay)截圖並保存到系統默認截圖目錄, 回傳檔案路徑")]
    async fn screenshot(&self) -> Result<String, ErrorData> {
        self.apply(vec![UiAction::CaptureScreenshot]);
        let deadline = Instant::now() + SCREENSHOT_TIMEOUT;
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Ok(mut state) = self.app.lock() {
                if let Some(res) = state.screenshot_result.take() {
                    return res
                        .map(|path| format!("截圖已保存 {path}"))
                        .map_err(|e| {
                            ErrorData::internal_error(format!("截圖失敗: {e}"), None)
                        });
                }
            }
            if Instant::now() >= deadline {
                return Err(ErrorData::internal_error("等待截圖完成超時", None));
            }
        }
    }

    #[tool(description = "回傳當前狀態欄文字")]
    async fn status(&self) -> Result<String, ErrorData> {
        Ok(self.lock()?.status.clone())
    }

    #[tool(description = "以 overlay 螢幕座標 (物理像素, 如同 `adb shell input tap` 的單位) 點擊 overlay 的某個位置: 用於點擊按鈕/切換檢視等; 座標超過螢幕邊界視為取消")]
    async fn tap(&self, Parameters(p): Parameters<TapParams>) -> Result<String, ErrorData> {
        let zoom = {
            let state = self.lock()?;
            state.zoom
        };
        // 螢幕邏輯座標 (egui) = 物理 px / zoom_factor
        let x = p.x / zoom;
        let y = p.y / zoom;
        let id = egui::TouchId(0);
        ui::push_touch(&self.app, id, egui::TouchPhase::Start, x, y);
        ui::push_touch(&self.app, id, egui::TouchPhase::End, x, y);
        // 等一小段時間讓 UI 產出幀
        tokio::time::sleep(Duration::from_millis(16)).await;
        Ok(format!("已點擊 ({:.1}, {:.1})", p.x, p.y))
    }

    #[tool(description = "切換 overlay 主視窗的折疊狀態 (對應音量鍵開盒/縮回), 用於醒出主 UI")]
    async fn toggle_overlay(&self) -> Result<String, ErrorData> {
        let state = self.lock()?;
        state
            .collapse_toggle
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok("已觸發折疊切換".to_string())
    }

    #[tool(
        description = "讀取附加行程的絕對位址記憶體; kind: u8/u16/u32/u64/i8/i16/i32/i64/f32/f64/bool/bytes(預設 u64); bytes 配合 len (預設64, 上限256)"
    )]
    async fn read(&self, Parameters(p): Parameters<ReadParams>) -> Result<String, ErrorData> {
        let state = self.lock()?;
        let pid = state.pid;
        if pid <= 0 {
            return Err(ErrorData::invalid_params(
                "尚未附加行程, 請先呼叫 attach",
                None,
            ));
        }
        let hex = p.addr.strip_prefix("0x").or_else(|| p.addr.strip_prefix("0X")).unwrap_or(&p.addr);
        let addr = u64::from_str_radix(hex, 16).map_err(|_| {
            ErrorData::invalid_params(format!("位址解析失敗: {} (需要十六進位, 0x 可選)", p.addr), None)
        })?;
        let int = |size: usize| -> Result<u64, ErrorData> {
            crate::nb9::read_value(pid, addr, size)
                .ok_or_else(|| ErrorData::internal_error("讀取失敗 (位址未映射或驅動拒絕)", None))
        };
        let kind = p.kind.as_deref().unwrap_or("u64").to_ascii_lowercase();
        match kind.as_str() {
            "u8" => Ok(format!("0x{addr:x} u8 = {}", int(1)?)),
            "u16" => Ok(format!("0x{addr:x} u16 = {}", int(2)?)),
            "u32" => Ok(format!("0x{addr:x} u32 = {}", int(4)?)),
            "u64" => {
                let v = int(8)?;
                Ok(format!("0x{addr:x} u64 = 0x{v:x} ({v})"))
            }
            "i8" => Ok(format!("0x{addr:x} i8 = {}", int(1)? as i8)),
            "i16" => Ok(format!("0x{addr:x} i16 = {}", int(2)? as i16)),
            "i32" => Ok(format!("0x{addr:x} i32 = {}", int(4)? as i32)),
            "i64" => Ok(format!("0x{addr:x} i64 = {}", int(8)? as i64)),
            "f32" => {
                let mut buf = [0u8; 4];
                if !crate::nb9::read(pid, addr, &mut buf) {
                    return Err(ErrorData::internal_error("讀取失敗 (位址未映射或驅動拒絕)", None));
                }
                Ok(format!("0x{addr:x} f32 = {}", f32::from_le_bytes(buf)))
            }
            "f64" => {
                let mut buf = [0u8; 8];
                if !crate::nb9::read(pid, addr, &mut buf) {
                    return Err(ErrorData::internal_error("讀取失敗 (位址未映射或驅動拒絕)", None));
                }
                Ok(format!("0x{addr:x} f64 = {}", f64::from_le_bytes(buf)))
            }
            "bool" => Ok(format!("0x{addr:x} bool = {}", int(1)? != 0)),
            "bytes" => {
                let len = p.len.unwrap_or(64).min(256);
                let bytes = crate::mem::hex_dump(pid, addr, len);
                if bytes.is_empty() {
                    return Err(ErrorData::internal_error(
                        "讀取失敗 (位址未映射或驅動拒絕)",
                        None,
                    ));
                }
                let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
                Ok(format!("0x{addr:x} bytes[{}] = {}", bytes.len(), hex.join(" ")))
            }
            other => Err(ErrorData::invalid_params(format!("不支援的 kind: {other}"), None)),
        }
    }

    #[tool(description = "列出行程 (PID 與名稱), 預設只含用戶應用; all=true 含系統行程")]
    async fn procs(&self, Parameters(p): Parameters<ProcsParams>) -> Result<String, ErrorData> {
        self.apply(vec![UiAction::RefreshProcs]);
        let state = self.lock()?;
        let shown: Vec<_> = state
            .procs
            .iter()
            .filter(|proc| p.all || !proc.system)
            .collect();
        let mut out = format!("共 {} 個行程 (顯示 {})\n", state.procs.len(), shown.len());
        for p in shown.iter().take(500) {
            out.push_str(&format!("{} {}\n", p.pid, p.name));
        }
        if shown.len() > 500 {
            out.push_str(&format!("... 其餘 {} 個省略\n", shown.len() - 500));
        }
        Ok(out)
    }

    #[tool(description = "附加到指定 PID, 之後的 scan/refine 作用於該行程")]
    async fn attach(&self, Parameters(p): Parameters<AttachParams>) -> Result<String, ErrorData> {
        self.apply(vec![UiAction::Attach(p.pid)]);
        let state = self.lock()?;
        if state.maps.is_empty() {
            return Err(ErrorData::internal_error(
                format!("附加 PID {} 失敗: 讀不到記憶體映射 (需要 root?)", p.pid),
                None,
            ));
        }
        Ok(format!(
            "已附加 {} (PID {}), {} 段映射, 基底位址 0x{:x}",
            state.proc_name,
            p.pid,
            state.maps.len(),
            state.module_base
        ))
    }

    #[tool(
        description = "對附加行程執行首次/新一輪掃描, 等待完成後回傳結果數量; type 省略則沿用 UI 當前選擇"
    )]
    async fn scan(&self, Parameters(p): Parameters<ScanParams>) -> Result<String, ErrorData> {
        let actions = {
            let state = self.lock()?;
            if state.scanning {
                return Err(ErrorData::internal_error("已有掃描進行中", None));
            }
            if state.pid <= 0 {
                return Err(ErrorData::invalid_params(
                    "尚未附加行程, 請先呼叫 attach",
                    None,
                ));
            }
            if state.maps.is_empty() {
                return Err(ErrorData::internal_error(
                    "記憶體映射為空, 請重新 attach",
                    None,
                ));
            }
            let (slot, signed) = match &p.r#type {
                Some(t) => parse_type(t)
                    .ok_or_else(|| ErrorData::invalid_params(format!("不支援的類型: {t}"), None))?,
                None => (state.scan_slot, state.signed),
            };
            let is_range = slot.is_interval();
            let ty = slot.ty(signed);
            if !is_range && ty.parse(&p.value).is_none() {
                return Err(ErrorData::invalid_params(
                    format!("目標值解析失敗: {}", p.value),
                    None,
                ));
            }
            let mut actions = Vec::new();
            // ScanSlot 在跨權威源時會做轉換, 隨後 StartScan 整體替換; 輸入框需先於 StartScan 設好
            if slot != state.scan_slot {
                actions.push(UiAction::ScanSlot(slot));
            }
            if signed != state.signed {
                actions.push(UiAction::SetSigned(signed));
            }
            actions.push(UiAction::SetInput(p.value.clone()));
            // scan 工具語義固定為目標值; UI 可能停在範圍/時間條件上
            actions.push(UiAction::ChangeMode(ChangeMode::ExactValue));
            actions.push(UiAction::Tab(Tab::Scan));
            actions.push(UiAction::StartScan);
            actions
        };
        self.apply(actions);
        wait_idle(&self.app, SCAN_TIMEOUT).await?;
        let summary = {
            let state = self.lock()?;
            if state.scan_slot.is_interval() {
                state
                    .scan
                    .blocks
                    .as_ref()
                    .map(|b| format!("掃描完成: {} 個候選分塊", b.candidate_blocks))
                    .unwrap_or_else(|| "沒有候選".to_string())
            } else {
                format!("掃描完成: {} 個結果", state.scan.typed.len())
            }
        };
        Ok(summary)
    }

    #[tool(
        description = "對已有結果按變化篩選; increased_by/decreased_by/exact_value 需 value; snapshot 為重新快照(不篩選); 等待完成後回傳結果數量"
    )]
    async fn refine(
        &self,
        Parameters(p): Parameters<RefineParams>,
    ) -> Result<String, ErrorData> {
        let mode = match p.mode.as_str() {
            "any_change" => ChangeMode::AnyChange,
            "unchanged" => ChangeMode::Unchanged,
            "increased" => ChangeMode::Increased,
            "decreased" => ChangeMode::Decreased,
            "increased_by" => ChangeMode::IncreasedBy,
            "decreased_by" => ChangeMode::DecreasedBy,
            "exact_value" => ChangeMode::ExactValue,
            "snapshot" => ChangeMode::Snapshot,
            _ => {
                return Err(ErrorData::invalid_params(
                    format!("未知模式: {}", p.mode),
                    None,
                ))
            }
        };
        let needs_value = matches!(
            mode,
            ChangeMode::IncreasedBy | ChangeMode::DecreasedBy | ChangeMode::ExactValue
        );
        let actions = {
            let state = self.lock()?;
            if state.scan_slot.is_interval()
                && !matches!(mode, ChangeMode::AnyChange | ChangeMode::Unchanged | ChangeMode::Snapshot)
            {
                return Err(ErrorData::invalid_params(
                    "區塊模式僅支援 any_change/unchanged",
                    None,
                ));
            }
            if state.scanning {
                return Err(ErrorData::internal_error("已有掃描進行中", None));
            }
            // 常規結果或 256B 快照 (投影出的視圖不可篩) 任一可篩選即放行
            let refinable = !state.scan.typed.is_empty()
                || (state.scan_slot.is_interval()
                    && state.frozen_ty == ValueType::Range
                    && state.scan.blocks.is_some());
            if !refinable {
                return Err(ErrorData::invalid_params("無結果可篩選, 請先 scan", None));
            }
            let ty = state.scan_slot.ty(state.signed);
            let mut actions = Vec::new();
            if needs_value {
                let v = p.value.clone().unwrap_or_default();
                if ty.parse(&v).is_none() {
                    return Err(ErrorData::invalid_params(
                        format!("目標值解析失敗: {v}"),
                        None,
                    ));
                }
                actions.push(UiAction::SetInput(v));
            }
            actions.push(UiAction::ChangeMode(mode));
            actions.push(UiAction::Tab(Tab::Scan));
            actions.push(UiAction::StartRefine);
            actions
        };
        self.apply(actions);
        wait_idle(&self.app, SCAN_TIMEOUT).await?;
        let summary = {
            let state = self.lock()?;
            if state.scan_slot.is_interval() {
                state
                    .scan
                    .blocks
                    .as_ref()
                    .map(|b| format!("篩選完成: {} 個候選分塊", b.candidate_blocks))
                    .unwrap_or_else(|| "沒有候選".to_string())
            } else {
                format!("篩選完成: {} 個結果", state.scan.typed.len())
            }
        };
        Ok(summary)
    }

    #[tool(description = "查看掃描結果 (分頁, 預設前 50 筆, 上限 500); 回傳的位址會加入選中集合")]
    async fn results(
        &self,
        Parameters(p): Parameters<ResultsParams>,
    ) -> Result<String, ErrorData> {
        let (addrs, out) = {
            let state = self.lock()?;
            let total = state.preview_row_count();
            if total == 0 {
                return Ok("無結果".to_string());
            }
            let ty = state.scan_slot.ty(state.signed);
            let size = ty.size();
            // 區塊模式沒有寬度/型別語意, 一律顯示 8 位元組原始位元
            let raw = if size == 0 { 8 } else { size };
            let offset = p.offset.unwrap_or(0).min(total);
            let limit = p.limit.unwrap_or(50).clamp(1, 500);
            let mut out = format!("共 {total} 個結果\n");
            let mut addrs = Vec::with_capacity(limit);
            for i in offset..(offset + limit).min(total) {
                let Some((base, bit, window_bits, len)) = state.preview_row(i) else {
                    continue;
                };
                addrs.push(entry_key(base, bit));
                let bits = state
                    .live_bits
                    .get(&entry_key(base, bit))
                    .copied()
                    .unwrap_or(window_bits);
                let addr_text = if size == 0 {
                    format!("0x{base:012x}")
                } else if matches!(ty, ValueType::BoolBit) {
                    format!("0x{base:012x}({bit})")
                } else {
                    format!("0x{base:012x}")
                };
                let raw_text = if matches!(ty, ValueType::BoolBit) {
                    format!("{}", bits as u8)
                } else if size == 0 && mem::is_string(ty) {
                    format!("0x{:016x}", bits)
                } else {
                    ui::hex_bytes(bits, raw)
                };
                let parsed_text = if size == 0 && mem::is_string(ty) {
                    // 直接解碼字串 (可讀), 若讀不到退回 hash hex
                    string_preview(state.pid, ty, base, len).unwrap_or_else(|| format!("0x{bits:x}"))
                } else if size == 0 {
                    format!("0x{bits:x}")
                } else {
                    ty.format(bits)
                };
                out.push_str(&format!("{addr_text}  {raw_text}  {parsed_text}\n"));
            }
            (addrs, out)
        };
        if !addrs.is_empty() {
            self.apply(vec![UiAction::Tab(Tab::Scan)]);
            let mut state = self.lock()?;
            state.selected.extend(addrs);
            state.bump();
        }
        Ok(out)
    }

    #[tool(description = "列出目前人類在結果頁選中的行 (含是否自選中後變更)")]
    async fn selection(&self) -> Result<String, ErrorData> {
        let state = self.lock()?;
        if state.selected.is_empty() {
            return Ok("無選中項".to_string());
        }
        let ty = state.scan_slot.ty(state.signed);
        let size = ty.size();
        let raw = if size == 0 { 8 } else { size };
        let mut n = 0usize;
        let mut out = String::new();
        for i in 0..state.preview_row_count() {
            let Some((base, bit, window_bits, len)) = state.preview_row(i) else {
                continue;
            };
            let key = entry_key(base, bit);
            if !state.selected.contains(&key) {
                continue;
            }
            n += 1;
            let bits = state.live_bits.get(&key).copied().unwrap_or(window_bits);
            // 切換型別: 凍結解讀不同, 一律標記已變更
            let mark = if ty != state.frozen_ty || bits != window_bits {
                "  [已變更]"
            } else {
                ""
            };
            let addr_text = if matches!(ty, ValueType::BoolBit) {
                format!("0x{base:012x}({bit})")
            } else {
                format!("0x{base:012x}")
            };
            let raw_text = if matches!(ty, ValueType::BoolBit) {
                format!("{}", bits as u8)
            } else if size == 0 && mem::is_string(ty) {
                format!("0x{:016x}", bits)
            } else {
                ui::hex_bytes(bits, raw)
            };
            let parsed_text = if size == 0 && mem::is_string(ty) {
                string_preview(state.pid, ty, base, len).unwrap_or_else(|| format!("0x{bits:x}"))
            } else if size == 0 {
                format!("0x{bits:x}")
            } else {
                ty.format(bits)
            };
            out.push_str(&format!("{addr_text}  {raw_text}  {parsed_text}{mark}\n"));
        }
        Ok(format!("選中 {n} 項\n{out}"))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Theraohe {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("theraohe", env!("CARGO_PKG_VERSION")))
    }
}

pub fn start(app: Arc<Mutex<AppState>>, port: u16) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("[mcp] tokio runtime 建立失敗: {e}");
                return;
            }
        };
        runtime.block_on(serve(app, port));
    });
}

async fn serve(app: Arc<Mutex<AppState>>, port: u16) {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[mcp] bind 127.0.0.1:{port} 失敗: {e}");
            return;
        }
    };
    eprintln!("[mcp] listening on http://127.0.0.1:{port}");
    // 無狀態模式: 每個 POST 自包含, 以 application/json 回應, 不維護 session
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true);
    let service = StreamableHttpService::new(
        move || {
            Ok(Theraohe {
                app: Arc::clone(&app),
                tool_router: Theraohe::tool_router(),
            })
        },
        Arc::new(LocalSessionManager::default()),
        config,
    );
    // 任意路徑皆接受 POST, 與舊行為一致
    let router = axum::Router::new().fallback_service(service);
    if let Err(e) = axum::serve(listener, router).await {
        eprintln!("[mcp] serve 結束: {e}");
    }
}
