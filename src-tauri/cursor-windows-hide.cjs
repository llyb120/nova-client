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
