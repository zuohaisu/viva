#!/usr/bin/env node
"use strict";

// The npm wrapper: resolve the prebuilt platform binary shipped as an
// optionalDependency of this package and exec it with argv/stdin intact.
// No postinstall downloads, no network — npm already picked and verified
// the right platform package.

const { spawn } = require("child_process");

const archMap = { arm64: "arm64", x64: "x64" };

if (process.platform !== "darwin" || !archMap[process.arch]) {
  console.error(
    `viva: no prebuilt binary for ${process.platform}/${process.arch}. ` +
      "viva ships macOS (Apple Silicon + Intel); grab a tarball from " +
      "https://github.com/zuohaisu/viva/releases instead."
  );
  process.exit(1);
}

const platformPkg = `@zuohaisu/viva-darwin-${archMap[process.arch]}`;

let nativeBin;
try {
  nativeBin = require.resolve(`${platformPkg}/bin/viva`);
} catch (err) {
  console.error(
    `viva: platform package ${platformPkg} is not installed. ` +
      "Reinstall with: npm i -g @zuohaisu/viva"
  );
  process.exit(1);
}

const child = spawn(nativeBin, process.argv.slice(2), { stdio: "inherit" });

child.on("error", (err) => {
  console.error("viva: failed to exec the platform binary:", err.message);
  process.exit(1);
});

child.on("close", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exitCode = code === null ? 1 : code;
});

// Forward termination signals so Ctrl-C and `kill` reach the office host
// (its quit path stops owned terminals and persists the handoff).
for (const sig of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(sig, () => {
    if (!child.killed) child.kill(sig);
  });
}
