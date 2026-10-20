// 统一 HTTP 客户端：按设置中的代理地址构建并缓存 ureq Agent。
// 仅支持 http/https 代理（未启用 ureq 的 socks 特性）；代理串非法时回退直连，避免整链不可用。
// 本机回环请求（CDP 调试端口等）不经过代理，调用方直接使用 ureq 而不走这里。
use std::sync::Mutex;
use std::time::Duration;

static CACHE: Mutex<Option<(String, ureq::Agent)>> = Mutex::new(None);

/// 默认 Agent：按当前设置的代理构建（空串 = 直连）
pub fn agent() -> ureq::Agent {
    agent_for(&crate::core::settings::load().http_proxy)
}

/// 指定代理串的 Agent（空串 = 直连）；同一代理串复用已构建的 Agent
pub fn agent_for(proxy: &str) -> ureq::Agent {
    let key = proxy.trim().to_string();
    if let Some((k, a)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if *k == key {
            return a.clone();
        }
    }
    // 统一默认超时（覆盖 DNS/连接/读写）：调用方仍可用 .timeout() 覆盖单次请求
    let mut builder = ureq::AgentBuilder::new().timeout(Duration::from_secs(12));
    if !key.is_empty() {
        if let Ok(p) = ureq::Proxy::new(&key) {
            builder = builder.proxy(p);
        }
    }
    let agent = builder.build();
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, agent.clone()));
    agent
}
