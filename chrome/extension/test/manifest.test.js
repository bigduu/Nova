import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const manifest = JSON.parse(
  await readFile(new URL("../manifest.json", import.meta.url), "utf8"),
);

test("manifest is MV3 with a module service worker", () => {
  assert.equal(manifest.manifest_version, 3);
  assert.deepEqual(manifest.background, {
    service_worker: "service-worker.js",
    type: "module",
  });
});

test("semantic content scripts never run in subframes or blank-frame fallbacks", () => {
  assert.equal(manifest.content_scripts.length, 1);
  const content = manifest.content_scripts[0];
  assert.equal(content.all_frames, false);
  assert.equal(content.match_about_blank, false);
  assert.equal(content.run_at, "document_start");
  assert.deepEqual(content.js, ["lib/semantic-runtime.js", "content-script.js"]);
});

test("extension has no externally connectable web surface", () => {
  assert.equal(Object.hasOwn(manifest, "externally_connectable"), false);
  assert.equal(Object.hasOwn(manifest, "web_accessible_resources"), false);
});

test("extension requests only native messaging API permission", () => {
  assert.deepEqual(manifest.permissions, ["nativeMessaging"]);
  assert.equal(Object.hasOwn(manifest, "host_permissions"), false);
});

test("declarative content access is limited to HTTP documents", () => {
  assert.deepEqual(manifest.content_scripts[0].matches, [
    "http://*/*",
    "https://*/*",
  ]);
});
