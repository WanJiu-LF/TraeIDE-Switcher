// 更新源镜像 Worker：对外提供 latest.json 与安装包，回源 GitHub Releases。
// 客户端只需把更新端点设为 https://<worker>/latest.json；
// latest.json 内的下载地址会被改写为 https://<worker>/dl/...，避免再去直连 GitHub。

const REPO = "WanJiu-LF/TraeIDE-Switcher";
const UPSTREAM_LATEST = `https://github.com/${REPO}/releases/latest/download/latest.json`;
const UPSTREAM_PREFIX = `https://github.com/${REPO}/releases/download/`;

export default {
  async fetch(req, env, ctx) {
    const url = new URL(req.url);
    if (req.method !== "GET" && req.method !== "HEAD") {
      return new Response("method not allowed", { status: 405 });
    }

    // 元信息：拉取最新 Release 的 latest.json，把平台下载地址改写为本 Worker 的 /dl 路径
    if (url.pathname === "/latest.json") {
      const up = await fetch(UPSTREAM_LATEST, { redirect: "follow" });
      if (!up.ok) {
        return new Response(`upstream ${up.status}`, { status: 502 });
      }
      const meta = await up.json();
      for (const p of Object.values(meta.platforms ?? {})) {
        const src = String(p.url ?? "");
        if (src.startsWith(UPSTREAM_PREFIX)) {
          p.url = `${url.origin}/dl/${src.slice(UPSTREAM_PREFIX.length)}`;
        }
      }
      return new Response(JSON.stringify(meta), {
        headers: {
          "content-type": "application/json; charset=utf-8",
          "cache-control": "public, max-age=120",
        },
      });
    }

    // 安装包：/dl/<tag>/<文件名>，只允许本站改写出的版本化路径，
    // 按原始 URL 做边缘缓存（版本化 URL 内容不变，可长期缓存）
    const rel = url.pathname.startsWith("/dl/") ? url.pathname.slice(4) : "";
    if (!/^v\d+\.\d+\.\d+\/[A-Za-z0-9._\-]+$/.test(rel)) {
      return new Response("not found", { status: 404 });
    }
    const target = UPSTREAM_PREFIX + rel;
    const cache = caches.default;
    const key = new Request(target, { method: "GET" });
    let res = await cache.match(key);
    if (!res) {
      const up = await fetch(target, { redirect: "follow" });
      if (!up.ok) {
        return new Response(`upstream ${up.status}`, { status: 502 });
      }
      res = new Response(up.body, up);
      res.headers.set("cache-control", "public, max-age=31536000, immutable");
      ctx.waitUntil(cache.put(key, res.clone()));
    }
    return res;
  },
};
