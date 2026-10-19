// 由 build/icon.svg（代码绘制）生成应用图标：png 多尺寸 + ico
import sharp from "sharp";
import pngToIco from "png-to-ico";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const svg = fs.readFileSync(path.join(root, "build", "icon.svg"));
const out = path.join(root, "src-tauri", "icons");
fs.mkdirSync(out, { recursive: true });

await sharp(svg).resize(128, 128).png().toFile(path.join(out, "128x128.png"));
await sharp(svg).resize(32, 32).png().toFile(path.join(out, "32x32.png"));

const sizes = [32, 64, 128, 256];
const bufs = await Promise.all(sizes.map((s) => sharp(svg).resize(s, s).png().toBuffer()));
fs.writeFileSync(path.join(out, "icon.ico"), await pngToIco(bufs));
console.log("icons generated:", fs.readdirSync(out).join(", "));
