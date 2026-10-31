import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { parseCredBlobs, pasteHint } from "./cred.ts";

const blob = `dGMFEAAA${"A".repeat(160)}`;

describe("parseCredBlobs", () => {
  it("extracts cloudide from storage.json object", () => {
    const sj = JSON.stringify({
      "iCubeAuthInfo://usertag": `dGMFEAAA${"B".repeat(160)}`,
      "iCubeAuthInfo://icube-dc:2644743167423884": `dGMFEAAA${"C".repeat(160)}`,
      "iCubeAuthInfo://icube.cloudide": blob,
    });
    assert.deepEqual(parseCredBlobs(sj), [blob]);
  });

  it("extracts cloudide from a pretty-printed JSON line", () => {
    const line = `    "iCubeAuthInfo://icube.cloudide": "${blob}",`;
    assert.deepEqual(parseCredBlobs(line), [blob]);
  });

  it("ignores usertag / icube-dc key lines", () => {
    const raw = [
      `"iCubeAuthInfo://usertag": "${blob.replace(/A/g, "B")}",`,
      `"iCubeAuthInfo://icube-dc:1": "${blob.replace(/A/g, "C")}",`,
    ].join("\n");
    assert.deepEqual(parseCredBlobs(raw), []);
  });

  it("keeps a raw ciphertext line", () => {
    assert.deepEqual(parseCredBlobs(blob), [blob]);
  });
});

describe("pasteHint", () => {
  it("warns on usertag-only paste", () => {
    assert.match(pasteHint("iCubeAuthInfo://usertag") ?? "", /usertag/);
  });

  it("warns on icube-dc-only paste", () => {
    assert.match(pasteHint("iCubeAuthInfo://icube-dc:2644743167423884") ?? "", /icube-dc/);
  });

  it("warns on key name without value", () => {
    assert.match(pasteHint("iCubeAuthInfo://icube.cloudide") ?? "", /键名/);
  });
});
