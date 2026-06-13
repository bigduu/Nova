#!/usr/bin/env node
// Thin launcher: exec the vendored Nova binary, passing stdio straight through
// so the MCP stdio transport (the host talks JSON-RPC over our stdin/stdout)
// is byte-for-byte transparent — this Node process just waits for nova to exit.
//
// Self-healing: if postinstall didn't vendor the binary (e.g. installed with
// --ignore-scripts), download it now before the first launch.

"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawnSync, execFileSync } = require("child_process");

const BIN = path.join(__dirname, "nova");

function die(msg) {
  process.stderr.write(`[nova] ${msg}\n`);
  process.exit(1);
}

if (os.platform() !== "darwin") {
  die("Nova runs on macOS only (it uses ScreenCaptureKit / Accessibility).");
}

if (!fs.existsSync(BIN)) {
  // Run the postinstall downloader synchronously, then continue.
  try {
    execFileSync(process.execPath, [path.join(__dirname, "..", "install.js")], { stdio: "inherit" });
  } catch {}
  if (!fs.existsSync(BIN)) {
    die("the Nova binary is missing and could not be downloaded — check your network or install manually from https://github.com/bigduu/Nova/releases");
  }
}

const res = spawnSync(BIN, process.argv.slice(2), { stdio: "inherit" });
if (res.error) die(`failed to launch nova: ${res.error.message}`);
process.exit(res.status === null ? 1 : res.status);
