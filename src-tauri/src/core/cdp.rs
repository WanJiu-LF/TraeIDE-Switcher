// CDP（Chrome DevTools Protocol）客户端：连接 Trae 主进程 --inspect 端口执行 JS
use serde::Deserialize;

#[derive(Debug)]
pub enum CdpError {
    Connect(String),
    Protocol(String),
}

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "cdp connect: {e}"),
            Self::Protocol(e) => write!(f, "cdp protocol: {e}"),
        }
    }
}

impl std::error::Error for CdpError {}

#[derive(Debug, Deserialize)]
struct TargetInfo {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    ws_url: Option<String>,
}

/// 枚举 --inspect 端口上的调试目标（主进程为 type=node）
pub fn list_targets(port: u16) -> Result<Vec<(String, String)>, CdpError> {
    let url = format!("http://127.0.0.1:{port}/json/list");
    let resp = ureq::get(&url)
        // 本机口正常是瞬时失败/成功，设超时只为兜底端口被半开连接占住的情形
        .timeout(std::time::Duration::from_secs(5))
        .call()
        .map_err(|e| CdpError::Connect(e.to_string()))?;
    let targets: Vec<TargetInfo> = resp
        .into_json()
        .map_err(|e| CdpError::Protocol(e.to_string()))?;
    Ok(targets
        .into_iter()
        .filter_map(|t| t.ws_url.map(|u| (t.kind, u)))
        .collect())
}

/// 在主进程（type=node 目标）求值 JS 表达式，返回 JSON 字符串形式的求值结果
pub fn evaluate(port: u16, expression: &str) -> Result<String, CdpError> {
    evaluate_inner(port, expression, false)
}

/// 同 evaluate，但等待 Promise resolve（async IIFE 求值）
pub fn evaluate_await(port: u16, expression: &str) -> Result<String, CdpError> {
    evaluate_inner(port, expression, true)
}

fn evaluate_inner(port: u16, expression: &str, await_promise: bool) -> Result<String, CdpError> {
    let targets = list_targets(port)?;
    let (_, ws_url) = targets
        .iter()
        .find(|(kind, _)| kind == "node")
        .or_else(|| targets.first())
        .ok_or_else(|| CdpError::Connect("no debug target".into()))?;

    let (mut socket, _) = tungstenite::connect(ws_url)
        .map_err(|e| CdpError::Connect(e.to_string()))?;

    let req = serde_json::json!({
        "id": 1,
        "method": "Runtime.evaluate",
        "params": {
            "expression": expression,
            "returnByValue": true,
            "awaitPromise": await_promise,
        }
    });
    socket
        .send(tungstenite::Message::Text(req.to_string()))
        .map_err(|e| CdpError::Protocol(e.to_string()))?;

    loop {
        let msg = socket
            .read()
            .map_err(|e| CdpError::Protocol(e.to_string()))?;
        let tungstenite::Message::Text(text) = msg else { continue };
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| CdpError::Protocol(e.to_string()))?;
        if v.get("id").and_then(|i| i.as_i64()) == Some(1) {
            if let Some(err) = v.get("error") {
                return Err(CdpError::Protocol(err.to_string()));
            }
            let result = v
                .pointer("/result/result/value")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            return Ok(result.to_string());
        }
    }
}

/// 供 evaluate 使用的小工具：把多行脚本压成单行 IIFE
pub fn wrap_script(body: &str) -> String {
    let joined = body
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(";");
    format!("(function(){{ {joined} }})()")
}
