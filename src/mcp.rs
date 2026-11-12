//! 極簡 MCP 伺服器(Streamable HTTP, 僅 JSON 回應): 讓 agent 可直連進程驅動動作
//! 支援 initialize / notifications/initialized / tools/list / tools/call
//! 通知(無 id)一律回 204; HTTP 只處理 Content-Length + Connection: close

use crate::ui::AppState;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn start(app: Arc<Mutex<AppState>>, port: u16) {
    std::thread::spawn(move || {
        let listener = match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[mcp] bind 127.0.0.1:{port} 失敗: {e}");
                return;
            }
        };
        eprintln!("[mcp] listening on http://127.0.0.1:{port}");
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let app = Arc::clone(&app);
            std::thread::spawn(move || handle_conn(stream, app));
        }
    });
}

fn handle_conn(stream: TcpStream, app: Arc<Mutex<AppState>>) {
    let Some(req) = read_request(&stream) else {
        return write_response(&stream, "400 Bad Request", "application/json", b"{}");
    };
    if req.method != "POST" {
        return write_response(
            &stream,
            "200 OK",
            "text/plain",
            b"theraohe mcp: POST JSON-RPC to this endpoint\n",
        );
    }
    match dispatch(&req.body, &app) {
        Some(resp) => {
            let body = resp.to_string();
            write_response(&stream, "200 OK", "application/json", body.as_bytes());
        }
        None => write_response(&stream, "204 No Content", "application/json", &[]),
    }
}

struct HttpRequest {
    method: String,
    body: Vec<u8>,
}

fn read_request(mut stream: &TcpStream) -> Option<HttpRequest> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_double_crlf(&buf) {
            break pos;
        }
        if buf.len() > 1 << 20 {
            return None;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let content_length = content_length(&headers).unwrap_or(0);
    let body_start = header_end + 4;
    while buf.len() < body_start + content_length {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let mut lines = headers.lines();
    let request_line = lines.next()?;
    let method = request_line.split_whitespace().next()?.to_string();
    Some(HttpRequest {
        method,
        body: buf[body_start..].to_vec(),
    })
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn content_length(headers: &str) -> Option<usize> {
    for line in headers.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            return value.trim().parse().ok();
        }
    }
    None
}

fn write_response(mut stream: &TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

// 回傳 Some = JSON-RPC 回應(請求含 id), None = 通知(無需回應)
fn dispatch(body: &[u8], app: &Arc<Mutex<AppState>>) -> Option<Value> {
    let req: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return Some(rpc_error(Value::Null, -32700, "parse error")),
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let params = req.get("params").cloned().unwrap_or(Value::Null);

    match method.as_str() {
        "initialize" => {
            let version = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2025-06-18")
                .to_string();
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "theraohe", "version": env!("CARGO_PKG_VERSION") },
                },
            }))
        }
        "notifications/initialized" | "initialized" => None,
        "tools/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": [
                    {
                        "name": "screenshot",
                        "description": "對整個螢幕(含 overlay)截圖並保存到系統默認截圖目錄, 回傳檔案路徑",
                        "inputSchema": { "type": "object", "properties": {}, "required": [] },
                    },
                    {
                        "name": "status",
                        "description": "回傳當前狀態欄文字",
                        "inputSchema": { "type": "object", "properties": {}, "required": [] },
                    },
                ],
            },
        })),
        "tools/call" => tool_call(id, &params, app),
        "ping" => Some(json!({ "jsonrpc": "2.0", "id": id, "result": {} })),
        other => {
            if id.is_null() {
                None
            } else {
                Some(rpc_error(id, -32601, &format!("method not found: {other}")))
            }
        }
    }
}

fn tool_call(id: Value, params: &Value, app: &Arc<Mutex<AppState>>) -> Option<Value> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    match name {
        "screenshot" => Some(tool_result(id, tool_screenshot(app))),
        "status" => Some(tool_result(id, tool_status(app))),
        other => Some(rpc_error(id, -32602, &format!("unknown tool: {other}"))),
    }
}

fn tool_result(id: Value, result: Result<String, String>) -> Value {
    match result {
        Ok(text) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "content": [{ "type": "text", "text": text }] },
        }),
        Err(text) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "content": [{ "type": "text", "text": text }], "isError": true },
        }),
    }
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_status(app: &Arc<Mutex<AppState>>) -> Result<String, String> {
    app.lock()
        .map(|state| state.status.clone())
        .map_err(|_| "app state lock failed".to_string())
}

fn tool_screenshot(app: &Arc<Mutex<AppState>>) -> Result<String, String> {
    {
        let mut state = app.lock().map_err(|_| "app state lock failed".to_string())?;
        state.screenshot_result = None;
        state.screenshot_pending = true;
        state.bump();
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        std::thread::sleep(Duration::from_millis(50));
        if let Ok(mut state) = app.lock() {
            if state.screenshot_result.is_some() {
                let res = state.screenshot_result.take().unwrap();
                return match res {
                    Ok(path) => Ok(format!("截圖已保存 {path}")),
                    Err(e) => Err(format!("截圖失敗: {e}")),
                };
            }
        }
        if Instant::now() >= deadline {
            return Err("等待截圖完成超時".to_string());
        }
    }
}
