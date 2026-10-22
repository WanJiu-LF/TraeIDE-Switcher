// 一键发布：本地构建（含 NSIS 签名产物）→ 生成 latest.json → 发布到源码仓库的 Releases
// 用法：npm run release            （本次更新说明用最近提交信息或默认文案）
//      npm run release -- --notes "本次修复说明"
// 注意：更新端点为 <仓库>/releases/latest/download/latest.json，因此源码仓库的每个正式
//      Release 都必须带上 latest.json + setup.exe + .sig，否则客户端自动更新会 404。
import { spawnSync } from "node:child_process";
import { closeSync, existsSync, openSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const RELEASE_REPO = "WanJiu-LF/TraeIDE-Switcher";
// 国内镜像源（Cloudflare Worker，回源 GitHub Releases）：必须与 tauri.conf.json endpoints[0] 一致
const MIRROR_ENDPOINT = "https://trae-updater.wanjiu.workers.dev/latest.json";
const KEY_FILE = join(root, "src-tauri", "keys", "updater.key");

const die = (msg) => {
  console.error(`\n[发布中止] ${msg}\n`);
  process.exit(1);
};

const conf = JSON.parse(readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8"));
const version = conf.version;
const endpoint = conf.plugins?.updater?.endpoints?.[0] ?? "";
if (!version) die("tauri.conf.json 缺少 version");

// 签名私钥只在本机使用：从本地文件注入环境变量，构建出的安装包才会带 .sig
if (!existsSync(KEY_FILE)) die(`缺少签名私钥：${KEY_FILE}（无此文件将无法生成更新签名）`);
process.env.TAURI_SIGNING_PRIVATE_KEY = readFileSync(KEY_FILE, "utf8").trim();
process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ??= "";

const nsisDir = join(root, "src-tauri", "target", "release", "bundle", "nsis");
const exeName = `TraeIDE-Switcher_${version}_x64-setup.exe`;
const exePath = join(nsisDir, exeName);
const sigPath = `${exePath}.sig`;

// 占用检测：运行中的换号器会锁住 target 下的 exe，直接构建会以共享冲突失败
const lockProbe = join(root, "src-tauri", "target", "release", "traeide-switcher.exe");
if (existsSync(lockProbe)) {
  try {
    closeSync(openSync(lockProbe, "r+"));
  } catch {
    die("请先关闭正在运行的 TraeIDE-Switcher，再执行发布（构建产物被占用）");
  }
}

console.log(`\n[1/4] 构建 v${version}（NSIS + 更新签名）…`);
const build = spawnSync("npm", ["run", "tauri", "build"], { cwd: root, stdio: "inherit", shell: true });
if (build.status !== 0) die("构建失败");

if (!existsSync(exePath) || !existsSync(sigPath)) {
  die(`未找到签名产物，检查 bundle.createUpdaterArtifacts 是否为 true：\n  ${exeName}\n  ${exeName}.sig`);
}

console.log("[2/4] 生成 latest.json（客户端据此判断是否有新版本）…");
const notesArg = process.argv.find((a, i) => process.argv[i - 1] === "--notes");
const url = `https://github.com/${RELEASE_REPO}/releases/download/v${version}/${exeName}`;
const latest = {
  version,
  notes: notesArg ?? `TraeIDE-Switcher v${version}`,
  pub_date: new Date().toISOString(),
  platforms: {
    "windows-x86_64": { signature: readFileSync(sigPath, "utf8").trim(), url },
  },
};
const latestPath = join(nsisDir, "latest.json");
writeFileSync(latestPath, JSON.stringify(latest, null, 2));

const tag = `v${version}`;
const exists = spawnSync("gh", ["release", "view", tag, "--repo", RELEASE_REPO], { cwd: root, stdio: "ignore" });
if (exists.status === 0) {
  die(`产物仓库中已存在 ${tag}。请先同步提升 src-tauri/tauri.conf.json 与 package.json 的 version 后重试。`);
}
// GitHub 不允许在空仓库创建 Release，先探测一次提交数，避免构建完才发现发不出去
const probe = spawnSync("gh", ["api", `repos/${RELEASE_REPO}/commits?per_page=1`, "--jq", "length"], {
  cwd: root,
  encoding: "utf8",
});
if (probe.status === 0 && probe.stdout.trim() === "0") {
  die(`产物仓库 ${RELEASE_REPO} 仍是空仓库，GitHub 不允许创建 Release。请先在其中提交任意文件（如 README）后重试。`);
}

console.log(`[3/4] 发布 ${tag} 到 ${RELEASE_REPO}…`);
const rel = spawnSync(
  "gh",
  ["release", "create", tag, "--repo", RELEASE_REPO, "--title", tag, "--notes", latest.notes, "--latest", exePath, sigPath, latestPath],
  { cwd: root, stdio: "inherit", shell: false },
);
if (rel.status !== 0) die("发布失败（检查 gh 登录状态与 repo 权限）");

// 镜像源校验：Worker 实时回源 latest.json，Release 生效后应立刻读到新版本；
// 未同步只告警不中断（客户端会按端点顺序回落 GitHub）
console.log("[4/4] 校验国内镜像源…");
let mirrorOk = false;
let mirrorVersion = "";
for (let i = 0; i < 4 && !mirrorOk; i++) {
  try {
    const r = await fetch(`${MIRROR_ENDPOINT}?t=${Date.now()}`, { headers: { "cache-control": "no-cache" } });
    const j = await r.json();
    mirrorVersion = String(j.version ?? "");
    mirrorOk = mirrorVersion === version;
  } catch (e) {
    mirrorVersion = `请求失败（${e.message}）`;
  }
  if (!mirrorOk) await new Promise((r) => setTimeout(r, 3000));
}
if (mirrorOk) {
  console.log(`  已同步 v${version}：${MIRROR_ENDPOINT}`);
} else {
  console.log(`  [警告] 镜像未同步到 v${version}（当前 ${mirrorVersion || "未知"}），客户端将回落 GitHub 源`);
}

console.log(`\n完成：内置更新源（按序回退）\n  1) ${endpoint}\n  2) https://github.com/${RELEASE_REPO}/releases/latest/download/latest.json`);
console.log(`下载地址 ${url}\n`);
