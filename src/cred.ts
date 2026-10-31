/** 粘贴容错：storage.json 整文件 / 键值行 / 裸密文 / 邮箱+Tab+密文 / TRAEBUNDLE */

const AUTH_KEY = "iCubeAuthInfo://icube.cloudide";
const BUNDLE = "TRAEBUNDLE:";
const MIN_BLOB = 80;

function normalizeB64(s: string): string {
  return s
    .replace(/\\\//g, "/")
    .replace(/-/g, "+")
    .replace(/_/g, "/")
    .replace(/[^A-Za-z0-9+/=]/g, "");
}

function looksLikeBlob(s: string): boolean {
  const n = normalizeB64(s);
  return n.length >= MIN_BLOB && /^[A-Za-z0-9+/=]+$/.test(n);
}

function extractCloudideFromJson(raw: string): string | null {
  try {
    const v = JSON.parse(raw) as Record<string, unknown>;
    const hit = v[AUTH_KEY];
    return typeof hit === "string" && looksLikeBlob(hit) ? normalizeB64(hit) : null;
  } catch {
    return null;
  }
}

function extractCloudideFromText(raw: string): string | null {
  const pos = raw.indexOf(AUTH_KEY);
  if (pos < 0) return null;
  let rest = raw.slice(pos + AUTH_KEY.length).replace(/^[\s"':=]+/, "");
  rest = rest.replace(/^["']/, "");
  const end = rest.search(/["',\s}]/);
  const blob = (end >= 0 ? rest.slice(0, end) : rest).trim();
  return looksLikeBlob(blob) ? normalizeB64(blob) : null;
}

/** 粘贴容错：允许整行「邮箱<TAB>密文」、带 `iCubeAuthInfo://` 前缀或两端引号 */
export function normalizeCred(raw: string): string {
  let s = raw.trim();
  const tab = s.lastIndexOf("\t");
  if (tab >= 0) s = s.slice(tab + 1).trim();
  if (/^\S+@\S+\s+[A-Za-z0-9+/=_-]{100,}$/.test(s)) s = s.split(/\s+/).pop() || s;
  s = s.replace(/^iCubeAuthInfo:\/\//i, "").trim();
  s = s.replace(/^["']|["']$/g, "").trim();
  return normalizeB64(s);
}

/**
 * 解析一次粘贴的全部登录凭据。
 * 若文本里出现 storage.json 的 cloudide 键，只取该值（忽略 usertag / icube-dc）。
 */
export function parseCredBlobs(raw: string): string[] {
  const text = raw.replace(/\\\//g, "/");
  const fromJson = extractCloudideFromJson(text);
  if (fromJson) return [fromJson];
  const fromLine = extractCloudideFromText(text);
  if (fromLine) return [fromLine];

  const seen = new Set<string>();
  const out: string[] = [];
  const push = (blob: string) => {
    if (!blob || seen.has(blob)) return;
    seen.add(blob);
    out.push(blob);
  };

  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (trimmed.toUpperCase().startsWith(BUNDLE)) {
      const body = trimmed.slice(BUNDLE.length).replace(/^["']|["']$/g, "").trim();
      if (body.length < 100 || !/^[A-Za-z0-9+/=]+$/.test(body)) continue;
      push(`${BUNDLE}${body}`);
      continue;
    }
    if (/iCubeAuthInfo:\/\/(usertag|icube-dc)/i.test(trimmed) && !trimmed.includes("cloudide")) {
      continue;
    }
    const blob = normalizeCred(line);
    if (blob.length < MIN_BLOB || !/^[A-Za-z0-9+/=]+$/.test(blob)) continue;
    push(blob);
  }
  return out;
}

export function pasteHint(raw: string): string | null {
  const t = raw.trim();
  if (!t) return null;
  if (parseCredBlobs(t).length > 0) return null;
  if (/iCubeAuthInfo:\/\/usertag/i.test(t) && !t.includes("cloudide")) {
    return "这是 usertag（区域标签），不是登录 Token。请复制 iCubeAuthInfo://icube.cloudide 的值";
  }
  if (/iCubeAuthInfo:\/\/icube-dc/i.test(t) && !t.includes("cloudide")) {
    return "这是 icube-dc 设备密钥，不是登录 Token。请复制 iCubeAuthInfo://icube.cloudide 的值";
  }
  if (/^iCubeAuthInfo:\/\/icube\.cloudide\s*$/i.test(t)) {
    return "这是键名。请复制该键右边的加密串值，或直接粘贴 storage.json 里包含 icube.cloudide 的那一行";
  }
  return null;
}
