// npm install 时让本仓库启用 .githooks（commit-msg 会去掉 Cursor 自动加的共同作者署名）。
// 不在 git 仓库里（例如解压的源码包）时什么也不做。
import { spawnSync } from "node:child_process";

spawnSync("git", ["config", "core.hooksPath", ".githooks"], { stdio: "ignore" });
