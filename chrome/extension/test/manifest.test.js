import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const manifest = JSON.parse(
  await readFile(new URL("../manifest.json", import.meta.url), "utf8"),
);
const expectedIcons = {
  "16": "icons/nova-16.png",
  "32": "icons/nova-32.png",
  "48": "icons/nova-48.png",
  "128": "icons/nova-128.png",
};
const expectedActionIcons = {
  "16": "icons/nova-16.png",
  "24": "icons/nova-24.png",
  "32": "icons/nova-32.png",
};
const pngSignature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

test("manifest is MV3 with a module service worker", () => {
  assert.equal(manifest.manifest_version, 3);
  assert.deepEqual(manifest.background, {
    service_worker: "service-worker.js",
    type: "module",
  });
});

test("manifest declares Nova extension and toolbar icons", () => {
  assert.deepEqual(manifest.icons, expectedIcons);
  assert.deepEqual(manifest.action, {
    default_icon: expectedActionIcons,
    default_title: "Nova",
    default_popup: "popup.html",
  });
});

test("declared Nova icons are PNG files with exact dimensions", async () => {
  const iconFiles = new Map(
    [
      ...Object.entries(expectedIcons),
      ...Object.entries(expectedActionIcons),
    ].map(([size, path]) => [path, Number(size)]),
  );

  await Promise.all(
    [...iconFiles].map(async ([path, expectedSize]) => {
      const png = await readFile(new URL(`../${path}`, import.meta.url));
      assert.ok(png.length >= 24, `${path} is too short to be a PNG`);
      assert.deepEqual(png.subarray(0, 8), pngSignature, `${path} has an invalid PNG signature`);
      assert.equal(png.toString("ascii", 12, 16), "IHDR", `${path} has no IHDR header`);
      assert.equal(png.readUInt32BE(16), expectedSize, `${path} has the wrong width`);
      assert.equal(png.readUInt32BE(20), expectedSize, `${path} has the wrong height`);
      assert.equal(png.readUInt8(24), 8, `${path} must use 8-bit channels`);
      assert.equal(png.readUInt8(25), 6, `${path} must be RGBA`);
    }),
  );
});

test("fresh installs have no automatic content scripts", () => {
  assert.equal(Object.hasOwn(manifest, "content_scripts"), false);
});

test("extension has no externally connectable web surface", () => {
  assert.equal(Object.hasOwn(manifest, "externally_connectable"), false);
  assert.equal(Object.hasOwn(manifest, "web_accessible_resources"), false);
});

test("required access uses activeTab and scripting without mandatory host access", () => {
  assert.deepEqual(manifest.permissions, ["nativeMessaging", "activeTab", "scripting"]);
  assert.equal(Object.hasOwn(manifest, "host_permissions"), false);
});

test("optional site access is limited to HTTP and leaves file/incognito off", () => {
  assert.deepEqual(manifest.optional_host_permissions, [
    "http://*/*",
    "https://*/*",
  ]);
  assert.equal(Object.hasOwn(manifest, "incognito"), false);
});
