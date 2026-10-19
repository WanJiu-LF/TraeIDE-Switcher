// 一键发布：本地构建（含 NSIS 签名产物）→ 生成 latest.json → 发布到自动更新产物仓库
// 用法：npm run release            （本次更新说明用最近提交信息或默认文案）
//      npm run release -- --notes "本次修复说明"
import { spawnSync } from "node:child_process";
import { closeSync, existsSync, openSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const RELEASE_REPO = "WanJiu-LF/TraeIDE-Switcher-releases";
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
    die("请先关闭正在运行的 Trae 换号器，再执行发布（构建产物被占用）");
  }
}

console.log(`\n[1/3] 构建 v${version}（NSIS + 更新签名）…`);
const build = spawnSync("npm", ["run", "tauri", "build"], { cwd: root, stdio: "inherit", shell: true });
if (build.status !== 0) die("构建失败");

if (!existsSync(exePath) || !existsSync(sigPath)) {
  die(`未找到签名产物，检查 bundle.createUpdaterArtifacts 是否为 true：\n  ${exeName}\n  ${exeName}.sig`);
}

console.log("[2/3] 生成 latest.json（客户端据此判断是否有新版本）…");
const notesArg = process.argv.find((a, i) => process.argv[i - 1] === "--notes");
const url = `https://github.com/${RELEASE_REPO}/releases/download/v${version}/${exeName}`;
const latest = {
  version,
  notes: notesArg ?? `Trae 换号器 v${version}`,
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

console.log(`[3/3] 发布 ${tag} 到 ${RELEASE_REPO}…`);
const rel = spawnSync(
  "gh",
  ["release", "create", tag, "--repo", RELEASE_REPO, "--title", tag, "--notes", latest.notes, "--latest", exePath, sigPath, latestPath],
  { cwd: root, stdio: "inherit", shell: false },
);
if (rel.status !== 0) die("发布失败（检查 gh 登录状态与 repo 权限）");

console.log(`\n完成：客户端自动更新端点 ${endpoint}`);
console.log(`下载地址 ${url}\n`);
