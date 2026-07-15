"use strict";

const childProcess = require("node:child_process");

const originalSpawn = childProcess.ChildProcess.prototype.spawn;

if (!originalSpawn.__novaWindowsHide) {
  const hiddenSpawn = function (options) {
    return originalSpawn.call(this, {
      ...options,
      // exec / execFile 会显式补 windowsHide=false，必须覆盖而不是只填默认值。
      windowsHide: true,
    });
  };
  Object.defineProperty(hiddenSpawn, "__novaWindowsHide", { value: true });
  childProcess.ChildProcess.prototype.spawn = hiddenSpawn;
}

for (const method of ["spawnSync", "execFileSync"]) {
  const original = childProcess[method];
  if (!original.__novaWindowsHide) {
    const hidden = function (command, args, options) {
      if (Array.isArray(args)) {
        return original.call(this, command, args, {
          ...(options || {}),
          windowsHide: true,
        });
      }
      return original.call(this, command, {
        ...(args || {}),
        windowsHide: true,
      });
    };
    Object.defineProperty(hidden, "__novaWindowsHide", { value: true });
    childProcess[method] = hidden;
  }
}

const originalExecSync = childProcess.execSync;
if (!originalExecSync.__novaWindowsHide) {
  const hiddenExecSync = function (command, options) {
    return originalExecSync.call(this, command, {
      ...(options || {}),
      windowsHide: true,
    });
  };
  Object.defineProperty(hiddenExecSync, "__novaWindowsHide", { value: true });
  childProcess.execSync = hiddenExecSync;
}
