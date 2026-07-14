"use strict";

const childProcess = require("node:child_process");

if (!childProcess.spawn.__novaWindowsHide) {
  const originalSpawn = childProcess.spawn;
  const hiddenSpawn = function (command, args, options) {
    if (Array.isArray(args)) {
      const spawnOptions = options || {};
      return originalSpawn.call(this, command, args, {
        ...spawnOptions,
        windowsHide: spawnOptions.windowsHide ?? true,
      });
    }
    const spawnOptions = args || {};
    return originalSpawn.call(this, command, {
      ...spawnOptions,
      windowsHide: spawnOptions.windowsHide ?? true,
    });
  };
  Object.defineProperty(hiddenSpawn, "__novaWindowsHide", { value: true });
  childProcess.spawn = hiddenSpawn;
}
