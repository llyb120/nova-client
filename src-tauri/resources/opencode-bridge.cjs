"use strict";
var __create = Object.create;
var __defProp = Object.defineProperty;
var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __getProtoOf = Object.getPrototypeOf;
var __hasOwnProp = Object.prototype.hasOwnProperty;
var __commonJS = (cb, mod) => function __require() {
  try {
    return mod || (0, cb[__getOwnPropNames(cb)[0]])((mod = { exports: {} }).exports, mod), mod.exports;
  } catch (e) {
    throw mod = 0, e;
  }
};
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, { get: all[name], enumerable: true });
};
var __copyProps = (to, from, except, desc) => {
  if (from && typeof from === "object" || typeof from === "function") {
    for (let key of __getOwnPropNames(from))
      if (!__hasOwnProp.call(to, key) && key !== except)
        __defProp(to, key, { get: () => from[key], enumerable: !(desc = __getOwnPropDesc(from, key)) || desc.enumerable });
  }
  return to;
};
var __toESM = (mod, isNodeMode, target) => (target = mod != null ? __create(__getProtoOf(mod)) : {}, __copyProps(
  // If the importer is in node compatibility mode or this is not an ESM
  // file that has been converted to a CommonJS file using a Babel-
  // compatible transform (i.e. "__esModule" has not been set), then set
  // "default" to the CommonJS "module.exports" for node compatibility.
  isNodeMode || !mod || !mod.__esModule ? __defProp(target, "default", { value: mod, enumerable: true }) : target,
  mod
));
var __toCommonJS = (mod) => __copyProps(__defProp({}, "__esModule", { value: true }), mod);

// node_modules/isexe/windows.js
var require_windows = __commonJS({
  "node_modules/isexe/windows.js"(exports2, module2) {
    module2.exports = isexe;
    isexe.sync = sync;
    var fs = require("fs");
    function checkPathExt(path, options) {
      var pathext = options.pathExt !== void 0 ? options.pathExt : process.env.PATHEXT;
      if (!pathext) {
        return true;
      }
      pathext = pathext.split(";");
      if (pathext.indexOf("") !== -1) {
        return true;
      }
      for (var i = 0; i < pathext.length; i++) {
        var p = pathext[i].toLowerCase();
        if (p && path.substr(-p.length).toLowerCase() === p) {
          return true;
        }
      }
      return false;
    }
    function checkStat(stat, path, options) {
      if (!stat.isSymbolicLink() && !stat.isFile()) {
        return false;
      }
      return checkPathExt(path, options);
    }
    function isexe(path, options, cb) {
      fs.stat(path, function(er, stat) {
        cb(er, er ? false : checkStat(stat, path, options));
      });
    }
    function sync(path, options) {
      return checkStat(fs.statSync(path), path, options);
    }
  }
});

// node_modules/isexe/mode.js
var require_mode = __commonJS({
  "node_modules/isexe/mode.js"(exports2, module2) {
    module2.exports = isexe;
    isexe.sync = sync;
    var fs = require("fs");
    function isexe(path, options, cb) {
      fs.stat(path, function(er, stat) {
        cb(er, er ? false : checkStat(stat, options));
      });
    }
    function sync(path, options) {
      return checkStat(fs.statSync(path), options);
    }
    function checkStat(stat, options) {
      return stat.isFile() && checkMode(stat, options);
    }
    function checkMode(stat, options) {
      var mod = stat.mode;
      var uid = stat.uid;
      var gid = stat.gid;
      var myUid = options.uid !== void 0 ? options.uid : process.getuid && process.getuid();
      var myGid = options.gid !== void 0 ? options.gid : process.getgid && process.getgid();
      var u = parseInt("100", 8);
      var g = parseInt("010", 8);
      var o = parseInt("001", 8);
      var ug = u | g;
      var ret = mod & o || mod & g && gid === myGid || mod & u && uid === myUid || mod & ug && myUid === 0;
      return ret;
    }
  }
});

// node_modules/isexe/index.js
var require_isexe = __commonJS({
  "node_modules/isexe/index.js"(exports2, module2) {
    var fs = require("fs");
    var core;
    if (process.platform === "win32" || global.TESTING_WINDOWS) {
      core = require_windows();
    } else {
      core = require_mode();
    }
    module2.exports = isexe;
    isexe.sync = sync;
    function isexe(path, options, cb) {
      if (typeof options === "function") {
        cb = options;
        options = {};
      }
      if (!cb) {
        if (typeof Promise !== "function") {
          throw new TypeError("callback not provided");
        }
        return new Promise(function(resolve, reject) {
          isexe(path, options || {}, function(er, is) {
            if (er) {
              reject(er);
            } else {
              resolve(is);
            }
          });
        });
      }
      core(path, options || {}, function(er, is) {
        if (er) {
          if (er.code === "EACCES" || options && options.ignoreErrors) {
            er = null;
            is = false;
          }
        }
        cb(er, is);
      });
    }
    function sync(path, options) {
      try {
        return core.sync(path, options || {});
      } catch (er) {
        if (options && options.ignoreErrors || er.code === "EACCES") {
          return false;
        } else {
          throw er;
        }
      }
    }
  }
});

// node_modules/which/which.js
var require_which = __commonJS({
  "node_modules/which/which.js"(exports2, module2) {
    var isWindows = process.platform === "win32" || process.env.OSTYPE === "cygwin" || process.env.OSTYPE === "msys";
    var path = require("path");
    var COLON = isWindows ? ";" : ":";
    var isexe = require_isexe();
    var getNotFoundError = (cmd) => Object.assign(new Error(`not found: ${cmd}`), { code: "ENOENT" });
    var getPathInfo = (cmd, opt) => {
      const colon = opt.colon || COLON;
      const pathEnv = cmd.match(/\//) || isWindows && cmd.match(/\\/) ? [""] : [
        // windows always checks the cwd first
        ...isWindows ? [process.cwd()] : [],
        ...(opt.path || process.env.PATH || /* istanbul ignore next: very unusual */
        "").split(colon)
      ];
      const pathExtExe = isWindows ? opt.pathExt || process.env.PATHEXT || ".EXE;.CMD;.BAT;.COM" : "";
      const pathExt = isWindows ? pathExtExe.split(colon) : [""];
      if (isWindows) {
        if (cmd.indexOf(".") !== -1 && pathExt[0] !== "")
          pathExt.unshift("");
      }
      return {
        pathEnv,
        pathExt,
        pathExtExe
      };
    };
    var which = (cmd, opt, cb) => {
      if (typeof opt === "function") {
        cb = opt;
        opt = {};
      }
      if (!opt)
        opt = {};
      const { pathEnv, pathExt, pathExtExe } = getPathInfo(cmd, opt);
      const found = [];
      const step = (i) => new Promise((resolve, reject) => {
        if (i === pathEnv.length)
          return opt.all && found.length ? resolve(found) : reject(getNotFoundError(cmd));
        const ppRaw = pathEnv[i];
        const pathPart = /^".*"$/.test(ppRaw) ? ppRaw.slice(1, -1) : ppRaw;
        const pCmd = path.join(pathPart, cmd);
        const p = !pathPart && /^\.[\\\/]/.test(cmd) ? cmd.slice(0, 2) + pCmd : pCmd;
        resolve(subStep(p, i, 0));
      });
      const subStep = (p, i, ii) => new Promise((resolve, reject) => {
        if (ii === pathExt.length)
          return resolve(step(i + 1));
        const ext = pathExt[ii];
        isexe(p + ext, { pathExt: pathExtExe }, (er, is) => {
          if (!er && is) {
            if (opt.all)
              found.push(p + ext);
            else
              return resolve(p + ext);
          }
          return resolve(subStep(p, i, ii + 1));
        });
      });
      return cb ? step(0).then((res) => cb(null, res), cb) : step(0);
    };
    var whichSync = (cmd, opt) => {
      opt = opt || {};
      const { pathEnv, pathExt, pathExtExe } = getPathInfo(cmd, opt);
      const found = [];
      for (let i = 0; i < pathEnv.length; i++) {
        const ppRaw = pathEnv[i];
        const pathPart = /^".*"$/.test(ppRaw) ? ppRaw.slice(1, -1) : ppRaw;
        const pCmd = path.join(pathPart, cmd);
        const p = !pathPart && /^\.[\\\/]/.test(cmd) ? cmd.slice(0, 2) + pCmd : pCmd;
        for (let j = 0; j < pathExt.length; j++) {
          const cur = p + pathExt[j];
          try {
            const is = isexe.sync(cur, { pathExt: pathExtExe });
            if (is) {
              if (opt.all)
                found.push(cur);
              else
                return cur;
            }
          } catch (ex) {
          }
        }
      }
      if (opt.all && found.length)
        return found;
      if (opt.nothrow)
        return null;
      throw getNotFoundError(cmd);
    };
    module2.exports = which;
    which.sync = whichSync;
  }
});

// node_modules/path-key/index.js
var require_path_key = __commonJS({
  "node_modules/path-key/index.js"(exports2, module2) {
    "use strict";
    var pathKey = (options = {}) => {
      const environment = options.env || process.env;
      const platform = options.platform || process.platform;
      if (platform !== "win32") {
        return "PATH";
      }
      return Object.keys(environment).reverse().find((key) => key.toUpperCase() === "PATH") || "Path";
    };
    module2.exports = pathKey;
    module2.exports.default = pathKey;
  }
});

// node_modules/cross-spawn/lib/util/resolveCommand.js
var require_resolveCommand = __commonJS({
  "node_modules/cross-spawn/lib/util/resolveCommand.js"(exports2, module2) {
    "use strict";
    var path = require("path");
    var which = require_which();
    var getPathKey = require_path_key();
    function resolveCommandAttempt(parsed, withoutPathExt) {
      const env = parsed.options.env || process.env;
      const cwd = process.cwd();
      const hasCustomCwd = parsed.options.cwd != null;
      const shouldSwitchCwd = hasCustomCwd && process.chdir !== void 0 && !process.chdir.disabled;
      if (shouldSwitchCwd) {
        try {
          process.chdir(parsed.options.cwd);
        } catch (err) {
        }
      }
      let resolved;
      try {
        resolved = which.sync(parsed.command, {
          path: env[getPathKey({ env })],
          pathExt: withoutPathExt ? path.delimiter : void 0
        });
      } catch (e) {
      } finally {
        if (shouldSwitchCwd) {
          process.chdir(cwd);
        }
      }
      if (resolved) {
        resolved = path.resolve(hasCustomCwd ? parsed.options.cwd : "", resolved);
      }
      return resolved;
    }
    function resolveCommand(parsed) {
      return resolveCommandAttempt(parsed) || resolveCommandAttempt(parsed, true);
    }
    module2.exports = resolveCommand;
  }
});

// node_modules/cross-spawn/lib/util/escape.js
var require_escape = __commonJS({
  "node_modules/cross-spawn/lib/util/escape.js"(exports2, module2) {
    "use strict";
    var metaCharsRegExp = /([()\][%!^"`<>&|;, *?])/g;
    function escapeCommand(arg) {
      arg = arg.replace(metaCharsRegExp, "^$1");
      return arg;
    }
    function escapeArgument(arg, doubleEscapeMetaChars) {
      arg = `${arg}`;
      arg = arg.replace(/(?=(\\+?)?)\1"/g, '$1$1\\"');
      arg = arg.replace(/(?=(\\+?)?)\1$/, "$1$1");
      arg = `"${arg}"`;
      arg = arg.replace(metaCharsRegExp, "^$1");
      if (doubleEscapeMetaChars) {
        arg = arg.replace(metaCharsRegExp, "^$1");
      }
      return arg;
    }
    module2.exports.command = escapeCommand;
    module2.exports.argument = escapeArgument;
  }
});

// node_modules/shebang-regex/index.js
var require_shebang_regex = __commonJS({
  "node_modules/shebang-regex/index.js"(exports2, module2) {
    "use strict";
    module2.exports = /^#!(.*)/;
  }
});

// node_modules/shebang-command/index.js
var require_shebang_command = __commonJS({
  "node_modules/shebang-command/index.js"(exports2, module2) {
    "use strict";
    var shebangRegex = require_shebang_regex();
    module2.exports = (string = "") => {
      const match = string.match(shebangRegex);
      if (!match) {
        return null;
      }
      const [path, argument] = match[0].replace(/#! ?/, "").split(" ");
      const binary = path.split("/").pop();
      if (binary === "env") {
        return argument;
      }
      return argument ? `${binary} ${argument}` : binary;
    };
  }
});

// node_modules/cross-spawn/lib/util/readShebang.js
var require_readShebang = __commonJS({
  "node_modules/cross-spawn/lib/util/readShebang.js"(exports2, module2) {
    "use strict";
    var fs = require("fs");
    var shebangCommand = require_shebang_command();
    function readShebang(command) {
      const size = 150;
      const buffer = Buffer.alloc(size);
      let fd;
      try {
        fd = fs.openSync(command, "r");
        fs.readSync(fd, buffer, 0, size, 0);
        fs.closeSync(fd);
      } catch (e) {
      }
      return shebangCommand(buffer.toString());
    }
    module2.exports = readShebang;
  }
});

// node_modules/cross-spawn/lib/parse.js
var require_parse = __commonJS({
  "node_modules/cross-spawn/lib/parse.js"(exports2, module2) {
    "use strict";
    var path = require("path");
    var resolveCommand = require_resolveCommand();
    var escape = require_escape();
    var readShebang = require_readShebang();
    var isWin = process.platform === "win32";
    var isExecutableRegExp = /\.(?:com|exe)$/i;
    var isCmdShimRegExp = /node_modules[\\/].bin[\\/][^\\/]+\.cmd$/i;
    function detectShebang(parsed) {
      parsed.file = resolveCommand(parsed);
      const shebang = parsed.file && readShebang(parsed.file);
      if (shebang) {
        parsed.args.unshift(parsed.file);
        parsed.command = shebang;
        return resolveCommand(parsed);
      }
      return parsed.file;
    }
    function parseNonShell(parsed) {
      if (!isWin) {
        return parsed;
      }
      const commandFile = detectShebang(parsed);
      const needsShell = !isExecutableRegExp.test(commandFile);
      if (parsed.options.forceShell || needsShell) {
        const needsDoubleEscapeMetaChars = isCmdShimRegExp.test(commandFile);
        parsed.command = path.normalize(parsed.command);
        parsed.command = escape.command(parsed.command);
        parsed.args = parsed.args.map((arg) => escape.argument(arg, needsDoubleEscapeMetaChars));
        const shellCommand = [parsed.command].concat(parsed.args).join(" ");
        parsed.args = ["/d", "/s", "/c", `"${shellCommand}"`];
        parsed.command = process.env.comspec || "cmd.exe";
        parsed.options.windowsVerbatimArguments = true;
      }
      return parsed;
    }
    function parse(command, args, options) {
      if (args && !Array.isArray(args)) {
        options = args;
        args = null;
      }
      args = args ? args.slice(0) : [];
      options = Object.assign({}, options);
      const parsed = {
        command,
        args,
        options,
        file: void 0,
        original: {
          command,
          args
        }
      };
      return options.shell ? parsed : parseNonShell(parsed);
    }
    module2.exports = parse;
  }
});

// node_modules/cross-spawn/lib/enoent.js
var require_enoent = __commonJS({
  "node_modules/cross-spawn/lib/enoent.js"(exports2, module2) {
    "use strict";
    var isWin = process.platform === "win32";
    function notFoundError(original, syscall) {
      return Object.assign(new Error(`${syscall} ${original.command} ENOENT`), {
        code: "ENOENT",
        errno: "ENOENT",
        syscall: `${syscall} ${original.command}`,
        path: original.command,
        spawnargs: original.args
      });
    }
    function hookChildProcess(cp, parsed) {
      if (!isWin) {
        return;
      }
      const originalEmit = cp.emit;
      cp.emit = function(name, arg1) {
        if (name === "exit") {
          const err = verifyENOENT(arg1, parsed);
          if (err) {
            return originalEmit.call(cp, "error", err);
          }
        }
        return originalEmit.apply(cp, arguments);
      };
    }
    function verifyENOENT(status, parsed) {
      if (isWin && status === 1 && !parsed.file) {
        return notFoundError(parsed.original, "spawn");
      }
      return null;
    }
    function verifyENOENTSync(status, parsed) {
      if (isWin && status === 1 && !parsed.file) {
        return notFoundError(parsed.original, "spawnSync");
      }
      return null;
    }
    module2.exports = {
      hookChildProcess,
      verifyENOENT,
      verifyENOENTSync,
      notFoundError
    };
  }
});

// node_modules/cross-spawn/index.js
var require_cross_spawn = __commonJS({
  "node_modules/cross-spawn/index.js"(exports2, module2) {
    "use strict";
    var cp = require("child_process");
    var parse = require_parse();
    var enoent = require_enoent();
    function spawn(command, args, options) {
      const parsed = parse(command, args, options);
      const spawned = cp.spawn(parsed.command, parsed.args, parsed.options);
      enoent.hookChildProcess(spawned, parsed);
      return spawned;
    }
    function spawnSync2(command, args, options) {
      const parsed = parse(command, args, options);
      const result = cp.spawnSync(parsed.command, parsed.args, parsed.options);
      result.error = result.error || enoent.verifyENOENTSync(result.status, parsed);
      return result;
    }
    module2.exports = spawn;
    module2.exports.spawn = spawn;
    module2.exports.sync = spawnSync2;
    module2.exports._parse = parse;
    module2.exports._enoent = enoent;
  }
});

// scripts/opencode-bridge.mjs
var opencode_bridge_exports = {};
__export(opencode_bridge_exports, {
  automaticPermissionReply: () => automaticPermissionReply
});
module.exports = __toCommonJS(opencode_bridge_exports);
var import_node_readline = require("node:readline");

// node_modules/@opencode-ai/sdk/dist/v2/gen/core/serverSentEvents.gen.js
var createSseClient = ({ onRequest, onSseError, onSseEvent, responseTransformer, responseValidator, sseDefaultRetryDelay, sseMaxRetryAttempts, sseMaxRetryDelay, sseSleepFn, url, ...options }) => {
  let lastEventId;
  const sleep = sseSleepFn ?? ((ms) => new Promise((resolve) => setTimeout(resolve, ms)));
  const createStream = async function* () {
    let retryDelay = sseDefaultRetryDelay ?? 3e3;
    let attempt = 0;
    const signal = options.signal ?? new AbortController().signal;
    while (true) {
      if (signal.aborted)
        break;
      attempt++;
      const headers = options.headers instanceof Headers ? options.headers : new Headers(options.headers);
      if (lastEventId !== void 0) {
        headers.set("Last-Event-ID", lastEventId);
      }
      try {
        const requestInit = {
          redirect: "follow",
          ...options,
          body: options.serializedBody,
          headers,
          signal
        };
        let request = new Request(url, requestInit);
        if (onRequest) {
          request = await onRequest(url, requestInit);
        }
        const _fetch = options.fetch ?? globalThis.fetch;
        const response = await _fetch(request);
        if (!response.ok)
          throw new Error(`SSE failed: ${response.status} ${response.statusText}`);
        if (!response.body)
          throw new Error("No body in SSE response");
        const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
        let buffer = "";
        const abortHandler = () => {
          try {
            reader.cancel();
          } catch {
          }
        };
        signal.addEventListener("abort", abortHandler);
        try {
          while (true) {
            const { done, value } = await reader.read();
            if (done)
              break;
            buffer += value;
            buffer = buffer.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
            const chunks = buffer.split("\n\n");
            buffer = chunks.pop() ?? "";
            for (const chunk of chunks) {
              const lines = chunk.split("\n");
              const dataLines = [];
              let eventName;
              for (const line of lines) {
                if (line.startsWith("data:")) {
                  dataLines.push(line.replace(/^data:\s*/, ""));
                } else if (line.startsWith("event:")) {
                  eventName = line.replace(/^event:\s*/, "");
                } else if (line.startsWith("id:")) {
                  lastEventId = line.replace(/^id:\s*/, "");
                } else if (line.startsWith("retry:")) {
                  const parsed = Number.parseInt(line.replace(/^retry:\s*/, ""), 10);
                  if (!Number.isNaN(parsed)) {
                    retryDelay = parsed;
                  }
                }
              }
              let data;
              let parsedJson = false;
              if (dataLines.length) {
                const rawData = dataLines.join("\n");
                try {
                  data = JSON.parse(rawData);
                  parsedJson = true;
                } catch {
                  data = rawData;
                }
              }
              if (parsedJson) {
                if (responseValidator) {
                  await responseValidator(data);
                }
                if (responseTransformer) {
                  data = await responseTransformer(data);
                }
              }
              onSseEvent?.({
                data,
                event: eventName,
                id: lastEventId,
                retry: retryDelay
              });
              if (dataLines.length) {
                yield data;
              }
            }
          }
        } finally {
          signal.removeEventListener("abort", abortHandler);
          reader.releaseLock();
        }
        break;
      } catch (error) {
        onSseError?.(error);
        if (sseMaxRetryAttempts !== void 0 && attempt >= sseMaxRetryAttempts) {
          break;
        }
        const backoff = Math.min(retryDelay * 2 ** (attempt - 1), sseMaxRetryDelay ?? 3e4);
        await sleep(backoff);
      }
    }
  };
  const stream = createStream();
  return { stream };
};

// node_modules/@opencode-ai/sdk/dist/v2/gen/core/pathSerializer.gen.js
var separatorArrayExplode = (style) => {
  switch (style) {
    case "label":
      return ".";
    case "matrix":
      return ";";
    case "simple":
      return ",";
    default:
      return "&";
  }
};
var separatorArrayNoExplode = (style) => {
  switch (style) {
    case "form":
      return ",";
    case "pipeDelimited":
      return "|";
    case "spaceDelimited":
      return "%20";
    default:
      return ",";
  }
};
var separatorObjectExplode = (style) => {
  switch (style) {
    case "label":
      return ".";
    case "matrix":
      return ";";
    case "simple":
      return ",";
    default:
      return "&";
  }
};
var serializeArrayParam = ({ allowReserved, explode, name, style, value }) => {
  if (!explode) {
    const joinedValues2 = (allowReserved ? value : value.map((v) => encodeURIComponent(v))).join(separatorArrayNoExplode(style));
    switch (style) {
      case "label":
        return `.${joinedValues2}`;
      case "matrix":
        return `;${name}=${joinedValues2}`;
      case "simple":
        return joinedValues2;
      default:
        return `${name}=${joinedValues2}`;
    }
  }
  const separator = separatorArrayExplode(style);
  const joinedValues = value.map((v) => {
    if (style === "label" || style === "simple") {
      return allowReserved ? v : encodeURIComponent(v);
    }
    return serializePrimitiveParam({
      allowReserved,
      name,
      value: v
    });
  }).join(separator);
  return style === "label" || style === "matrix" ? separator + joinedValues : joinedValues;
};
var serializePrimitiveParam = ({ allowReserved, name, value }) => {
  if (value === void 0 || value === null) {
    return "";
  }
  if (typeof value === "object") {
    throw new Error("Deeply-nested arrays/objects aren\u2019t supported. Provide your own `querySerializer()` to handle these.");
  }
  return `${name}=${allowReserved ? value : encodeURIComponent(value)}`;
};
var serializeObjectParam = ({ allowReserved, explode, name, style, value, valueOnly }) => {
  if (value instanceof Date) {
    return valueOnly ? value.toISOString() : `${name}=${value.toISOString()}`;
  }
  if (style !== "deepObject" && !explode) {
    let values = [];
    Object.entries(value).forEach(([key, v]) => {
      values = [...values, key, allowReserved ? v : encodeURIComponent(v)];
    });
    const joinedValues2 = values.join(",");
    switch (style) {
      case "form":
        return `${name}=${joinedValues2}`;
      case "label":
        return `.${joinedValues2}`;
      case "matrix":
        return `;${name}=${joinedValues2}`;
      default:
        return joinedValues2;
    }
  }
  const separator = separatorObjectExplode(style);
  const joinedValues = Object.entries(value).map(([key, v]) => serializePrimitiveParam({
    allowReserved,
    name: style === "deepObject" ? `${name}[${key}]` : key,
    value: v
  })).join(separator);
  return style === "label" || style === "matrix" ? separator + joinedValues : joinedValues;
};

// node_modules/@opencode-ai/sdk/dist/v2/gen/core/utils.gen.js
var PATH_PARAM_RE = /\{[^{}]+\}/g;
var defaultPathSerializer = ({ path, url: _url }) => {
  let url = _url;
  const matches = _url.match(PATH_PARAM_RE);
  if (matches) {
    for (const match of matches) {
      let explode = false;
      let name = match.substring(1, match.length - 1);
      let style = "simple";
      if (name.endsWith("*")) {
        explode = true;
        name = name.substring(0, name.length - 1);
      }
      if (name.startsWith(".")) {
        name = name.substring(1);
        style = "label";
      } else if (name.startsWith(";")) {
        name = name.substring(1);
        style = "matrix";
      }
      const value = path[name];
      if (value === void 0 || value === null) {
        continue;
      }
      if (Array.isArray(value)) {
        url = url.replace(match, serializeArrayParam({ explode, name, style, value }));
        continue;
      }
      if (typeof value === "object") {
        url = url.replace(match, serializeObjectParam({
          explode,
          name,
          style,
          value,
          valueOnly: true
        }));
        continue;
      }
      if (style === "matrix") {
        url = url.replace(match, `;${serializePrimitiveParam({
          name,
          value
        })}`);
        continue;
      }
      const replaceValue = encodeURIComponent(style === "label" ? `.${value}` : value);
      url = url.replace(match, replaceValue);
    }
  }
  return url;
};
var getUrl = ({ baseUrl, path, query, querySerializer, url: _url }) => {
  const pathUrl = _url.startsWith("/") ? _url : `/${_url}`;
  let url = (baseUrl ?? "") + pathUrl;
  if (path) {
    url = defaultPathSerializer({ path, url });
  }
  let search = query ? querySerializer(query) : "";
  if (search.startsWith("?")) {
    search = search.substring(1);
  }
  if (search) {
    url += `?${search}`;
  }
  return url;
};
function getValidRequestBody(options) {
  const hasBody = options.body !== void 0;
  const isSerializedBody = hasBody && options.bodySerializer;
  if (isSerializedBody) {
    if ("serializedBody" in options) {
      const hasSerializedBody = options.serializedBody !== void 0 && options.serializedBody !== "";
      return hasSerializedBody ? options.serializedBody : null;
    }
    return options.body !== "" ? options.body : null;
  }
  if (hasBody) {
    return options.body;
  }
  return void 0;
}

// node_modules/@opencode-ai/sdk/dist/v2/gen/core/auth.gen.js
var getAuthToken = async (auth, callback) => {
  const token = typeof callback === "function" ? await callback(auth) : callback;
  if (!token) {
    return;
  }
  if (auth.scheme === "bearer") {
    return `Bearer ${token}`;
  }
  if (auth.scheme === "basic") {
    return `Basic ${btoa(token)}`;
  }
  return token;
};

// node_modules/@opencode-ai/sdk/dist/v2/gen/core/bodySerializer.gen.js
var jsonBodySerializer = {
  bodySerializer: (body) => JSON.stringify(body, (_key, value) => typeof value === "bigint" ? value.toString() : value)
};

// node_modules/@opencode-ai/sdk/dist/v2/gen/client/utils.gen.js
var createQuerySerializer = ({ parameters = {}, ...args } = {}) => {
  const querySerializer = (queryParams) => {
    const search = [];
    if (queryParams && typeof queryParams === "object") {
      for (const name in queryParams) {
        const value = queryParams[name];
        if (value === void 0 || value === null) {
          continue;
        }
        const options = parameters[name] || args;
        if (Array.isArray(value)) {
          const serializedArray = serializeArrayParam({
            allowReserved: options.allowReserved,
            explode: true,
            name,
            style: "form",
            value,
            ...options.array
          });
          if (serializedArray)
            search.push(serializedArray);
        } else if (typeof value === "object") {
          const serializedObject = serializeObjectParam({
            allowReserved: options.allowReserved,
            explode: true,
            name,
            style: "deepObject",
            value,
            ...options.object
          });
          if (serializedObject)
            search.push(serializedObject);
        } else {
          const serializedPrimitive = serializePrimitiveParam({
            allowReserved: options.allowReserved,
            name,
            value
          });
          if (serializedPrimitive)
            search.push(serializedPrimitive);
        }
      }
    }
    return search.join("&");
  };
  return querySerializer;
};
var getParseAs = (contentType) => {
  if (!contentType) {
    return "stream";
  }
  const cleanContent = contentType.split(";")[0]?.trim();
  if (!cleanContent) {
    return;
  }
  if (cleanContent.startsWith("application/json") || cleanContent.endsWith("+json")) {
    return "json";
  }
  if (cleanContent === "multipart/form-data") {
    return "formData";
  }
  if (["application/", "audio/", "image/", "video/"].some((type) => cleanContent.startsWith(type))) {
    return "blob";
  }
  if (cleanContent.startsWith("text/")) {
    return "text";
  }
  return;
};
var checkForExistence = (options, name) => {
  if (!name) {
    return false;
  }
  if (options.headers.has(name) || options.query?.[name] || options.headers.get("Cookie")?.includes(`${name}=`)) {
    return true;
  }
  return false;
};
var setAuthParams = async ({ security, ...options }) => {
  for (const auth of security) {
    if (checkForExistence(options, auth.name)) {
      continue;
    }
    const token = await getAuthToken(auth, options.auth);
    if (!token) {
      continue;
    }
    const name = auth.name ?? "Authorization";
    switch (auth.in) {
      case "query":
        if (!options.query) {
          options.query = {};
        }
        options.query[name] = token;
        break;
      case "cookie":
        options.headers.append("Cookie", `${name}=${token}`);
        break;
      case "header":
      default:
        options.headers.set(name, token);
        break;
    }
  }
};
var buildUrl = (options) => getUrl({
  baseUrl: options.baseUrl,
  path: options.path,
  query: options.query,
  querySerializer: typeof options.querySerializer === "function" ? options.querySerializer : createQuerySerializer(options.querySerializer),
  url: options.url
});
var mergeConfigs = (a, b) => {
  const config = { ...a, ...b };
  if (config.baseUrl?.endsWith("/")) {
    config.baseUrl = config.baseUrl.substring(0, config.baseUrl.length - 1);
  }
  config.headers = mergeHeaders(a.headers, b.headers);
  return config;
};
var headersEntries = (headers) => {
  const entries = [];
  headers.forEach((value, key) => {
    entries.push([key, value]);
  });
  return entries;
};
var mergeHeaders = (...headers) => {
  const mergedHeaders = new Headers();
  for (const header of headers) {
    if (!header) {
      continue;
    }
    const iterator = header instanceof Headers ? headersEntries(header) : Object.entries(header);
    for (const [key, value] of iterator) {
      if (value === null) {
        mergedHeaders.delete(key);
      } else if (Array.isArray(value)) {
        for (const v of value) {
          mergedHeaders.append(key, v);
        }
      } else if (value !== void 0) {
        mergedHeaders.set(key, typeof value === "object" ? JSON.stringify(value) : value);
      }
    }
  }
  return mergedHeaders;
};
var Interceptors = class {
  fns = [];
  clear() {
    this.fns = [];
  }
  eject(id) {
    const index = this.getInterceptorIndex(id);
    if (this.fns[index]) {
      this.fns[index] = null;
    }
  }
  exists(id) {
    const index = this.getInterceptorIndex(id);
    return Boolean(this.fns[index]);
  }
  getInterceptorIndex(id) {
    if (typeof id === "number") {
      return this.fns[id] ? id : -1;
    }
    return this.fns.indexOf(id);
  }
  update(id, fn) {
    const index = this.getInterceptorIndex(id);
    if (this.fns[index]) {
      this.fns[index] = fn;
      return id;
    }
    return false;
  }
  use(fn) {
    this.fns.push(fn);
    return this.fns.length - 1;
  }
};
var createInterceptors = () => ({
  error: new Interceptors(),
  request: new Interceptors(),
  response: new Interceptors()
});
var defaultQuerySerializer = createQuerySerializer({
  allowReserved: false,
  array: {
    explode: true,
    style: "form"
  },
  object: {
    explode: true,
    style: "deepObject"
  }
});
var defaultHeaders = {
  "Content-Type": "application/json"
};
var createConfig = (override = {}) => ({
  ...jsonBodySerializer,
  headers: defaultHeaders,
  parseAs: "auto",
  querySerializer: defaultQuerySerializer,
  ...override
});

// node_modules/@opencode-ai/sdk/dist/v2/gen/client/client.gen.js
var createClient = (config = {}) => {
  let _config = mergeConfigs(createConfig(), config);
  const getConfig = () => ({ ..._config });
  const setConfig = (config2) => {
    _config = mergeConfigs(_config, config2);
    return getConfig();
  };
  const interceptors = createInterceptors();
  const beforeRequest = async (options) => {
    const opts = {
      ..._config,
      ...options,
      fetch: options.fetch ?? _config.fetch ?? globalThis.fetch,
      headers: mergeHeaders(_config.headers, options.headers),
      serializedBody: void 0
    };
    if (opts.security) {
      await setAuthParams({
        ...opts,
        security: opts.security
      });
    }
    if (opts.requestValidator) {
      await opts.requestValidator(opts);
    }
    if (opts.body !== void 0 && opts.bodySerializer) {
      opts.serializedBody = opts.bodySerializer(opts.body);
    }
    if (opts.body === void 0 || opts.serializedBody === "") {
      opts.headers.delete("Content-Type");
    }
    const url = buildUrl(opts);
    return { opts, url };
  };
  const request = async (options) => {
    const { opts, url } = await beforeRequest(options);
    const requestInit = {
      redirect: "follow",
      ...opts,
      body: getValidRequestBody(opts)
    };
    let request2 = new Request(url, requestInit);
    for (const fn of interceptors.request.fns) {
      if (fn) {
        request2 = await fn(request2, opts);
      }
    }
    const _fetch = opts.fetch;
    let response;
    try {
      response = await _fetch(request2);
    } catch (error2) {
      let finalError2 = error2;
      for (const fn of interceptors.error.fns) {
        if (fn) {
          finalError2 = await fn(error2, void 0, request2, opts);
        }
      }
      finalError2 = finalError2 || {};
      if (opts.throwOnError) {
        throw finalError2;
      }
      return opts.responseStyle === "data" ? void 0 : {
        error: finalError2,
        request: request2,
        response: void 0
      };
    }
    for (const fn of interceptors.response.fns) {
      if (fn) {
        response = await fn(response, request2, opts);
      }
    }
    const result = {
      request: request2,
      response
    };
    if (response.ok) {
      const parseAs = (opts.parseAs === "auto" ? getParseAs(response.headers.get("Content-Type")) : opts.parseAs) ?? "json";
      if (response.status === 204 || response.headers.get("Content-Length") === "0") {
        let emptyData;
        switch (parseAs) {
          case "arrayBuffer":
          case "blob":
          case "text":
            emptyData = await response[parseAs]();
            break;
          case "formData":
            emptyData = new FormData();
            break;
          case "stream":
            emptyData = response.body;
            break;
          case "json":
          default:
            emptyData = {};
            break;
        }
        return opts.responseStyle === "data" ? emptyData : {
          data: emptyData,
          ...result
        };
      }
      let data;
      switch (parseAs) {
        case "arrayBuffer":
        case "blob":
        case "formData":
        case "text":
          data = await response[parseAs]();
          break;
        case "json": {
          const text = await response.text();
          data = text ? JSON.parse(text) : {};
          break;
        }
        case "stream":
          return opts.responseStyle === "data" ? response.body : {
            data: response.body,
            ...result
          };
      }
      if (parseAs === "json") {
        if (opts.responseValidator) {
          await opts.responseValidator(data);
        }
        if (opts.responseTransformer) {
          data = await opts.responseTransformer(data);
        }
      }
      return opts.responseStyle === "data" ? data : {
        data,
        ...result
      };
    }
    const textError = await response.text();
    let jsonError;
    try {
      jsonError = JSON.parse(textError);
    } catch {
    }
    const error = jsonError ?? textError;
    let finalError = error;
    for (const fn of interceptors.error.fns) {
      if (fn) {
        finalError = await fn(error, response, request2, opts);
      }
    }
    finalError = finalError || {};
    if (opts.throwOnError) {
      throw finalError;
    }
    return opts.responseStyle === "data" ? void 0 : {
      error: finalError,
      ...result
    };
  };
  const makeMethodFn = (method) => (options) => request({ ...options, method });
  const makeSseFn = (method) => async (options) => {
    const { opts, url } = await beforeRequest(options);
    return createSseClient({
      ...opts,
      body: opts.body,
      headers: opts.headers,
      method,
      onRequest: async (url2, init) => {
        let request2 = new Request(url2, init);
        for (const fn of interceptors.request.fns) {
          if (fn) {
            request2 = await fn(request2, opts);
          }
        }
        return request2;
      },
      serializedBody: getValidRequestBody(opts),
      url
    });
  };
  return {
    buildUrl,
    connect: makeMethodFn("CONNECT"),
    delete: makeMethodFn("DELETE"),
    get: makeMethodFn("GET"),
    getConfig,
    head: makeMethodFn("HEAD"),
    interceptors,
    options: makeMethodFn("OPTIONS"),
    patch: makeMethodFn("PATCH"),
    post: makeMethodFn("POST"),
    put: makeMethodFn("PUT"),
    request,
    setConfig,
    sse: {
      connect: makeSseFn("CONNECT"),
      delete: makeSseFn("DELETE"),
      get: makeSseFn("GET"),
      head: makeSseFn("HEAD"),
      options: makeSseFn("OPTIONS"),
      patch: makeSseFn("PATCH"),
      post: makeSseFn("POST"),
      put: makeSseFn("PUT"),
      trace: makeSseFn("TRACE")
    },
    trace: makeMethodFn("TRACE")
  };
};

// node_modules/@opencode-ai/sdk/dist/v2/gen/core/params.gen.js
var extraPrefixesMap = {
  $body_: "body",
  $headers_: "headers",
  $path_: "path",
  $query_: "query"
};
var extraPrefixes = Object.entries(extraPrefixesMap);
var buildKeyMap = (fields, map) => {
  if (!map) {
    map = /* @__PURE__ */ new Map();
  }
  for (const config of fields) {
    if ("in" in config) {
      if (config.key) {
        map.set(config.key, {
          in: config.in,
          map: config.map
        });
      }
    } else if ("key" in config) {
      map.set(config.key, {
        map: config.map
      });
    } else if (config.args) {
      buildKeyMap(config.args, map);
    }
  }
  return map;
};
var stripEmptySlots = (params) => {
  for (const [slot, value] of Object.entries(params)) {
    if (value && typeof value === "object" && !Object.keys(value).length) {
      delete params[slot];
    }
  }
};
var buildClientParams = (args, fields) => {
  const params = {
    body: {},
    headers: {},
    path: {},
    query: {}
  };
  const map = buildKeyMap(fields);
  let config;
  for (const [index, arg] of args.entries()) {
    if (fields[index]) {
      config = fields[index];
    }
    if (!config) {
      continue;
    }
    if ("in" in config) {
      if (config.key) {
        const field = map.get(config.key);
        const name = field.map || config.key;
        if (field.in) {
          ;
          params[field.in][name] = arg;
        }
      } else {
        params.body = arg;
      }
    } else {
      for (const [key, value] of Object.entries(arg ?? {})) {
        const field = map.get(key);
        if (field) {
          if (field.in) {
            const name = field.map || key;
            params[field.in][name] = value;
          } else {
            params[field.map] = value;
          }
        } else {
          const extra = extraPrefixes.find(([prefix]) => key.startsWith(prefix));
          if (extra) {
            const [prefix, slot] = extra;
            params[slot][key.slice(prefix.length)] = value;
          } else if ("allowExtra" in config && config.allowExtra) {
            for (const [slot, allowed] of Object.entries(config.allowExtra)) {
              if (allowed) {
                ;
                params[slot][key] = value;
                break;
              }
            }
          }
        }
      }
    }
  }
  stripEmptySlots(params);
  return params;
};

// node_modules/@opencode-ai/sdk/dist/v2/gen/client.gen.js
var client = createClient(createConfig({ baseUrl: "http://localhost:4096" }));

// node_modules/@opencode-ai/sdk/dist/v2/gen/sdk.gen.js
var HeyApiClient = class {
  client;
  constructor(args) {
    this.client = args?.client ?? client;
  }
};
var HeyApiRegistry = class {
  defaultKey = "default";
  instances = /* @__PURE__ */ new Map();
  get(key) {
    const instance = this.instances.get(key ?? this.defaultKey);
    if (!instance) {
      throw new Error(`No SDK client found. Create one with "new OpencodeClient()" to fix this error.`);
    }
    return instance;
  }
  set(value, key) {
    this.instances.set(key ?? this.defaultKey, value);
  }
};
var Auth = class extends HeyApiClient {
  /**
   * Remove auth credentials
   *
   * Remove authentication credentials
   */
  remove(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "path", key: "providerID" }] }]);
    return (options?.client ?? this.client).delete({
      url: "/auth/{providerID}",
      ...options,
      ...params
    });
  }
  /**
   * Set auth credentials
   *
   * Set authentication credentials
   */
  set(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "providerID" },
          { key: "auth", map: "body" }
        ]
      }
    ]);
    return (options?.client ?? this.client).put({
      url: "/auth/{providerID}",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var App = class extends HeyApiClient {
  /**
   * Write log
   *
   * Write a log entry to the server logs with specified level and metadata.
   */
  log(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "service" },
          { in: "body", key: "level" },
          { in: "body", key: "message" },
          { in: "body", key: "extra" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/log",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * List agents
   *
   * Get a list of all available AI agents in the OpenCode system.
   */
  agents(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/agent",
      ...options,
      ...params
    });
  }
  /**
   * List skills
   *
   * Get a list of all available skills in the OpenCode system.
   */
  skills(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/skill",
      ...options,
      ...params
    });
  }
};
var ControlPlane = class extends HeyApiClient {
  /**
   * Move session
   *
   * Move a session to another project directory, optionally transferring local changes.
   */
  moveSession(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "body", key: "sessionID" },
          { in: "body", key: "destination" },
          { in: "body", key: "moveChanges" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/experimental/control-plane/move-session",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var Capabilities = class extends HeyApiClient {
  /**
   * Get experimental capabilities
   *
   * Get experimental features enabled on the OpenCode server.
   */
  get(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/experimental/capabilities",
      ...options,
      ...params
    });
  }
};
var Console = class extends HeyApiClient {
  /**
   * Get active Console provider metadata
   *
   * Get the active Console org name and the set of provider IDs managed by that Console org.
   */
  get(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/experimental/console",
      ...options,
      ...params
    });
  }
  /**
   * List switchable Console orgs
   *
   * Get the available Console orgs across logged-in accounts, including the current active org.
   */
  listOrgs(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/experimental/console/orgs",
      ...options,
      ...params
    });
  }
  /**
   * Switch active Console org
   *
   * Persist a new active Console account/org selection for the current local OpenCode state.
   */
  switchOrg(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "accountID" },
          { in: "body", key: "orgID" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/experimental/console/switch",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var Session = class extends HeyApiClient {
  /**
   * List sessions
   *
   * Get a list of all OpenCode sessions across projects, sorted by most recently updated. Archived sessions are excluded by default.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "query", key: "roots" },
          { in: "query", key: "start" },
          { in: "query", key: "cursor" },
          { in: "query", key: "search" },
          { in: "query", key: "limit" },
          { in: "query", key: "archived" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/experimental/session",
      ...options,
      ...params
    });
  }
  /**
   * Background subagents
   *
   * Detach any synchronous subagents currently blocking the session and continue them in the background.
   */
  background(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/experimental/session/{sessionID}/background",
      ...options,
      ...params
    });
  }
};
var Resource = class extends HeyApiClient {
  /**
   * Get MCP resources
   *
   * Get all available MCP resources from connected servers. Optionally filter by name.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/experimental/resource",
      ...options,
      ...params
    });
  }
};
var ProjectCopy = class extends HeyApiClient {
  /**
   * Generate project copy name
   *
   * Generate a short name for a project copy from task context.
   */
  generateName(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "projectID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "context" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/experimental/project/{projectID}/copy/generate-name",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var Adapter = class extends HeyApiClient {
  /**
   * List workspace adapters
   *
   * List all available workspace adapters for the current project.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/experimental/workspace/adapter",
      ...options,
      ...params
    });
  }
};
var Workspace = class extends HeyApiClient {
  /**
   * List workspaces
   *
   * List all workspaces.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/experimental/workspace",
      ...options,
      ...params
    });
  }
  /**
   * Create workspace
   *
   * Create a workspace for the current project.
   */
  create(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "id" },
          { in: "body", key: "type" },
          { in: "body", key: "branch" },
          { in: "body", key: "extra" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/experimental/workspace",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Sync workspace list
   *
   * Register missing workspaces returned by workspace adapters.
   */
  syncList(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/experimental/workspace/sync-list",
      ...options,
      ...params
    });
  }
  /**
   * Workspace status
   *
   * Get connection status for workspaces in the current project.
   */
  status(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/experimental/workspace/status",
      ...options,
      ...params
    });
  }
  /**
   * Remove workspace
   *
   * Remove an existing workspace.
   */
  remove(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "id" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).delete({
      url: "/experimental/workspace/{id}",
      ...options,
      ...params
    });
  }
  /**
   * Warp session into workspace
   *
   * Move a session's sync history into the target workspace, or detach it to the local project.
   */
  warp(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "id" },
          { in: "body", key: "sessionID" },
          { in: "body", key: "copyChanges" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/experimental/workspace/warp",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  _adapter;
  get adapter() {
    return this._adapter ??= new Adapter({ client: this.client });
  }
};
var Experimental = class extends HeyApiClient {
  _controlPlane;
  get controlPlane() {
    return this._controlPlane ??= new ControlPlane({ client: this.client });
  }
  _capabilities;
  get capabilities() {
    return this._capabilities ??= new Capabilities({ client: this.client });
  }
  _console;
  get console() {
    return this._console ??= new Console({ client: this.client });
  }
  _session;
  get session() {
    return this._session ??= new Session({ client: this.client });
  }
  _resource;
  get resource() {
    return this._resource ??= new Resource({ client: this.client });
  }
  _projectCopy;
  get projectCopy() {
    return this._projectCopy ??= new ProjectCopy({ client: this.client });
  }
  _workspace;
  get workspace() {
    return this._workspace ??= new Workspace({ client: this.client });
  }
};
var Config = class extends HeyApiClient {
  /**
   * Get global configuration
   *
   * Retrieve the current global OpenCode configuration settings and preferences.
   */
  get(options) {
    return (options?.client ?? this.client).get({
      url: "/global/config",
      ...options
    });
  }
  /**
   * Update global configuration
   *
   * Update global OpenCode configuration settings and preferences.
   */
  update(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ key: "config", map: "body" }] }]);
    return (options?.client ?? this.client).patch({
      url: "/global/config",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var Global = class extends HeyApiClient {
  /**
   * Get health
   *
   * Get health information about the OpenCode server.
   */
  health(options) {
    return (options?.client ?? this.client).get({
      url: "/global/health",
      ...options
    });
  }
  /**
   * Get global events
   *
   * Subscribe to global events from the OpenCode system using server-sent events.
   */
  event(options) {
    return (options?.client ?? this.client).sse.get({
      url: "/global/event",
      ...options
    });
  }
  /**
   * Dispose instance
   *
   * Clean up and dispose all OpenCode instances, releasing all resources.
   */
  dispose(options) {
    return (options?.client ?? this.client).post({
      url: "/global/dispose",
      ...options
    });
  }
  /**
   * Upgrade opencode
   *
   * Upgrade opencode to the specified version or latest if not specified.
   */
  upgrade(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "body", key: "target" }] }]);
    return (options?.client ?? this.client).post({
      url: "/global/upgrade",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  _config;
  get config() {
    return this._config ??= new Config({ client: this.client });
  }
};
var Event = class extends HeyApiClient {
  /**
   * Subscribe to events
   *
   * Get events
   */
  subscribe(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).sse.get({
      url: "/event",
      ...options,
      ...params
    });
  }
};
var Config2 = class extends HeyApiClient {
  /**
   * Get configuration
   *
   * Retrieve the current OpenCode configuration settings and preferences.
   */
  get(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/config",
      ...options,
      ...params
    });
  }
  /**
   * Update configuration
   *
   * Update OpenCode configuration settings and preferences.
   */
  update(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { key: "config", map: "body" }
        ]
      }
    ]);
    return (options?.client ?? this.client).patch({
      url: "/config",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * List config providers
   *
   * Get a list of all configured AI providers and their default models.
   */
  providers(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/config/providers",
      ...options,
      ...params
    });
  }
};
var Tool = class extends HeyApiClient {
  /**
   * List tools
   *
   * Get a list of available tools with their JSON schema parameters for a specific provider and model combination.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "query", key: "provider" },
          { in: "query", key: "model" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/experimental/tool",
      ...options,
      ...params
    });
  }
  /**
   * List tool IDs
   *
   * Get a list of all available tool IDs, including both built-in tools and dynamically registered tools.
   */
  ids(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/experimental/tool/ids",
      ...options,
      ...params
    });
  }
};
var Worktree = class extends HeyApiClient {
  /**
   * Remove worktree
   *
   * Remove a git worktree and delete its branch.
   */
  remove(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { key: "worktreeRemoveInput", map: "body" }
        ]
      }
    ]);
    return (options?.client ?? this.client).delete({
      url: "/experimental/worktree",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * List worktrees
   *
   * List all sandbox worktrees for the current project.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/experimental/worktree",
      ...options,
      ...params
    });
  }
  /**
   * Create worktree
   *
   * Create a new git worktree for the current project and run any configured startup scripts.
   */
  create(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { key: "worktreeCreateInput", map: "body" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/experimental/worktree",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Reset worktree
   *
   * Reset a worktree branch to the primary default branch.
   */
  reset(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { key: "worktreeResetInput", map: "body" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/experimental/worktree/reset",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var Find = class extends HeyApiClient {
  /**
   * Find text
   *
   * Search for text patterns across files in the project using ripgrep.
   */
  text(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "query", key: "pattern" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/find",
      ...options,
      ...params
    });
  }
  /**
   * Find files
   *
   * Search for files or directories by name or pattern in the project directory.
   */
  files(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "query", key: "query" },
          { in: "query", key: "dirs" },
          { in: "query", key: "type" },
          { in: "query", key: "limit" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/find/file",
      ...options,
      ...params
    });
  }
  /**
   * Find symbols
   *
   * Search for workspace symbols like functions, classes, and variables using LSP.
   */
  symbols(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "query", key: "query" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/find/symbol",
      ...options,
      ...params
    });
  }
};
var File = class extends HeyApiClient {
  /**
   * List files
   *
   * List files and directories in a specified path.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "query", key: "path" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/file",
      ...options,
      ...params
    });
  }
  /**
   * Read file
   *
   * Read the content of a specified file.
   */
  read(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "query", key: "path" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/file/content",
      ...options,
      ...params
    });
  }
  /**
   * Get file status
   *
   * Get the git status of all files in the project.
   */
  status(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/file/status",
      ...options,
      ...params
    });
  }
};
var Instance = class extends HeyApiClient {
  /**
   * Dispose instance
   *
   * Clean up and dispose the current OpenCode instance, releasing all resources.
   */
  dispose(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/instance/dispose",
      ...options,
      ...params
    });
  }
};
var Path = class extends HeyApiClient {
  /**
   * Get paths
   *
   * Retrieve the current working directory and related path information for the OpenCode instance.
   */
  get(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/path",
      ...options,
      ...params
    });
  }
};
var Diff = class extends HeyApiClient {
  /**
   * Get raw VCS diff
   *
   * Retrieve a raw patch for current uncommitted changes.
   */
  raw(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/vcs/diff/raw",
      ...options,
      ...params
    });
  }
};
var Vcs = class extends HeyApiClient {
  /**
   * Get VCS info
   *
   * Retrieve version control system (VCS) information for the current project, such as git branch.
   */
  get(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/vcs",
      ...options,
      ...params
    });
  }
  /**
   * Get VCS status
   *
   * Retrieve changed files in the current working tree without patches.
   */
  status(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/vcs/status",
      ...options,
      ...params
    });
  }
  /**
   * Get VCS diff
   *
   * Retrieve the current git diff for the working tree or against the default branch.
   */
  diff(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "query", key: "mode" },
          { in: "query", key: "context" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/vcs/diff",
      ...options,
      ...params
    });
  }
  /**
   * Apply VCS patch
   *
   * Apply a raw patch to the current working tree.
   */
  apply(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "patch" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/vcs/apply",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  _diff;
  get diff2() {
    return this._diff ??= new Diff({ client: this.client });
  }
};
var Command = class extends HeyApiClient {
  /**
   * List commands
   *
   * Get a list of all available commands in the OpenCode system.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/command",
      ...options,
      ...params
    });
  }
};
var Lsp = class extends HeyApiClient {
  /**
   * Get LSP status
   *
   * Get LSP server status
   */
  status(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/lsp",
      ...options,
      ...params
    });
  }
};
var Formatter = class extends HeyApiClient {
  /**
   * Get formatter status
   *
   * Get formatter status
   */
  status(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/formatter",
      ...options,
      ...params
    });
  }
};
var Auth2 = class extends HeyApiClient {
  /**
   * Remove MCP OAuth
   *
   * Remove OAuth credentials for an MCP server.
   */
  remove(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "name" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).delete({
      url: "/mcp/{name}/auth",
      ...options,
      ...params
    });
  }
  /**
   * Start MCP OAuth
   *
   * Start OAuth authentication flow for a Model Context Protocol (MCP) server.
   */
  start(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "name" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/mcp/{name}/auth",
      ...options,
      ...params
    });
  }
  /**
   * Complete MCP OAuth
   *
   * Complete OAuth authentication for a Model Context Protocol (MCP) server using the authorization code.
   */
  callback(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "name" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "code" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/mcp/{name}/auth/callback",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Authenticate MCP OAuth
   *
   * Start OAuth flow and wait for callback (opens browser).
   */
  authenticate(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "name" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/mcp/{name}/auth/authenticate",
      ...options,
      ...params
    });
  }
};
var Mcp = class extends HeyApiClient {
  /**
   * Get MCP status
   *
   * Get the status of all Model Context Protocol (MCP) servers.
   */
  status(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/mcp",
      ...options,
      ...params
    });
  }
  /**
   * Add MCP server
   *
   * Dynamically add a new Model Context Protocol (MCP) server to the system.
   */
  add(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "name" },
          { in: "body", key: "config" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/mcp",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Connect an MCP server.
   */
  connect(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "name" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/mcp/{name}/connect",
      ...options,
      ...params
    });
  }
  /**
   * Disconnect an MCP server.
   */
  disconnect(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "name" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/mcp/{name}/disconnect",
      ...options,
      ...params
    });
  }
  _auth;
  get auth() {
    return this._auth ??= new Auth2({ client: this.client });
  }
};
var Project = class extends HeyApiClient {
  /**
   * List all projects
   *
   * Get a list of projects that have been opened with OpenCode.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/project",
      ...options,
      ...params
    });
  }
  /**
   * Get current project
   *
   * Retrieve the currently active project that OpenCode is working with.
   */
  current(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/project/current",
      ...options,
      ...params
    });
  }
  /**
   * Initialize git repository
   *
   * Create a git repository for the current project and return the refreshed project info.
   */
  initGit(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/project/git/init",
      ...options,
      ...params
    });
  }
  /**
   * Update project
   *
   * Update project properties such as name, icon, and commands.
   */
  update(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "projectID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "name" },
          { in: "body", key: "icon" },
          { in: "body", key: "commands" }
        ]
      }
    ]);
    return (options?.client ?? this.client).patch({
      url: "/project/{projectID}",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * List project directories
   *
   * List known local absolute directories for a project.
   */
  directories(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "projectID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/project/{projectID}/directories",
      ...options,
      ...params
    });
  }
};
var Pty = class extends HeyApiClient {
  /**
   * List available shells
   *
   * Get a list of available shells on the system.
   */
  shells(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/pty/shells",
      ...options,
      ...params
    });
  }
  /**
   * List PTY sessions
   *
   * Get a list of all active pseudo-terminal (PTY) sessions managed by OpenCode.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/pty",
      ...options,
      ...params
    });
  }
  /**
   * Create PTY session
   *
   * Create a new pseudo-terminal (PTY) session for running shell commands and processes.
   */
  create(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "command" },
          { in: "body", key: "args" },
          { in: "body", key: "cwd" },
          { in: "body", key: "title" },
          { in: "body", key: "env" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/pty",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Remove PTY session
   *
   * Remove and terminate a specific pseudo-terminal (PTY) session.
   */
  remove(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "ptyID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).delete({
      url: "/pty/{ptyID}",
      ...options,
      ...params
    });
  }
  /**
   * Get PTY session
   *
   * Retrieve detailed information about a specific pseudo-terminal (PTY) session.
   */
  get(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "ptyID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/pty/{ptyID}",
      ...options,
      ...params
    });
  }
  /**
   * Update PTY session
   *
   * Update properties of an existing pseudo-terminal (PTY) session.
   */
  update(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "ptyID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "title" },
          { in: "body", key: "size" }
        ]
      }
    ]);
    return (options?.client ?? this.client).put({
      url: "/pty/{ptyID}",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Create PTY WebSocket token
   *
   * Create a short-lived ticket for opening a PTY WebSocket connection.
   */
  connectToken(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "ptyID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/pty/{ptyID}/connect-token",
      ...options,
      ...params
    });
  }
  /**
   * Connect to PTY session
   *
   * Establish a WebSocket connection to interact with a pseudo-terminal (PTY) session in real-time.
   */
  connect(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "ptyID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "query", key: "cursor" },
          { in: "query", key: "ticket" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/pty/{ptyID}/connect",
      ...options,
      ...params
    });
  }
};
var Question = class extends HeyApiClient {
  /**
   * List pending questions
   *
   * Get all pending question requests across all sessions.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/question",
      ...options,
      ...params
    });
  }
  /**
   * Reply to question request
   *
   * Provide answers to a question request from the AI assistant.
   */
  reply(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "requestID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "answers" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/question/{requestID}/reply",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Reject question request
   *
   * Reject a question request from the AI assistant.
   */
  reject(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "requestID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/question/{requestID}/reject",
      ...options,
      ...params
    });
  }
};
var Permission = class extends HeyApiClient {
  /**
   * List pending permissions
   *
   * Get all pending permission requests across all sessions.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/permission",
      ...options,
      ...params
    });
  }
  /**
   * Respond to permission request
   *
   * Approve or deny a permission request from the AI assistant.
   */
  reply(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "requestID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "reply" },
          { in: "body", key: "message" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/permission/{requestID}/reply",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Respond to permission
   *
   * Approve or deny a permission request from the AI assistant.
   *
   * @deprecated
   */
  respond(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "path", key: "permissionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "response" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/session/{sessionID}/permissions/{permissionID}",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var Oauth = class extends HeyApiClient {
  /**
   * Start OAuth authorization
   *
   * Start the OAuth authorization flow for a provider.
   */
  authorize(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "providerID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "method" },
          { in: "body", key: "inputs" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/provider/{providerID}/oauth/authorize",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Handle OAuth callback
   *
   * Handle the OAuth callback from a provider after user authorization.
   */
  callback(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "providerID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "method" },
          { in: "body", key: "code" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/provider/{providerID}/oauth/callback",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var Provider = class extends HeyApiClient {
  /**
   * List providers
   *
   * Get a list of all available AI providers, including both available and connected ones.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/provider",
      ...options,
      ...params
    });
  }
  /**
   * Get provider auth methods
   *
   * Retrieve available authentication methods for all AI providers.
   */
  auth(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/provider/auth",
      ...options,
      ...params
    });
  }
  _oauth;
  get oauth() {
    return this._oauth ??= new Oauth({ client: this.client });
  }
};
var Session2 = class extends HeyApiClient {
  /**
   * List sessions
   *
   * Get a list of all OpenCode sessions, sorted by most recently updated.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "query", key: "scope" },
          { in: "query", key: "path" },
          { in: "query", key: "roots" },
          { in: "query", key: "start" },
          { in: "query", key: "search" },
          { in: "query", key: "limit" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/session",
      ...options,
      ...params
    });
  }
  /**
   * Create session
   *
   * Create a new OpenCode session for interacting with AI assistants and managing conversations.
   */
  create(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "parentID" },
          { in: "body", key: "title" },
          { in: "body", key: "agent" },
          { in: "body", key: "model" },
          { in: "body", key: "metadata" },
          { in: "body", key: "permission" },
          { in: "body", key: "workspaceID" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/session",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Get session status
   *
   * Retrieve the current status of all sessions, including active, idle, and completed states.
   */
  status(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/session/status",
      ...options,
      ...params
    });
  }
  /**
   * Delete session
   *
   * Delete a session and permanently remove all associated data, including messages and history.
   */
  delete(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).delete({
      url: "/session/{sessionID}",
      ...options,
      ...params
    });
  }
  /**
   * Get session
   *
   * Retrieve detailed information about a specific OpenCode session.
   */
  get(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/session/{sessionID}",
      ...options,
      ...params
    });
  }
  /**
   * Update session
   *
   * Update properties of an existing session, such as title or other metadata.
   */
  update(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "title" },
          { in: "body", key: "metadata" },
          { in: "body", key: "permission" },
          { in: "body", key: "time" }
        ]
      }
    ]);
    return (options?.client ?? this.client).patch({
      url: "/session/{sessionID}",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Get session children
   *
   * Retrieve all child sessions that were forked from the specified parent session.
   */
  children(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/session/{sessionID}/children",
      ...options,
      ...params
    });
  }
  /**
   * Get session todos
   *
   * Retrieve the todo list associated with a specific session, showing tasks and action items.
   */
  todo(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/session/{sessionID}/todo",
      ...options,
      ...params
    });
  }
  /**
   * Get message diff
   *
   * Get the file changes (diff) that resulted from a specific user message in the session.
   */
  diff(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "query", key: "messageID" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/session/{sessionID}/diff",
      ...options,
      ...params
    });
  }
  /**
   * Get session messages
   *
   * Retrieve all messages in a session, including user prompts and AI responses.
   */
  messages(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "query", key: "limit" },
          { in: "query", key: "before" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/session/{sessionID}/message",
      ...options,
      ...params
    });
  }
  /**
   * Send message
   *
   * Create and send a new message to a session, streaming the AI response.
   */
  prompt(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "messageID" },
          { in: "body", key: "model" },
          { in: "body", key: "agent" },
          { in: "body", key: "noReply" },
          { in: "body", key: "tools" },
          { in: "body", key: "format" },
          { in: "body", key: "system" },
          { in: "body", key: "variant" },
          { in: "body", key: "parts" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/session/{sessionID}/message",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Delete message
   *
   * Permanently delete a specific message and all of its parts from a session without reverting file changes.
   */
  deleteMessage(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "path", key: "messageID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).delete({
      url: "/session/{sessionID}/message/{messageID}",
      ...options,
      ...params
    });
  }
  /**
   * Get message
   *
   * Retrieve a specific message from a session by its message ID.
   */
  message(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "path", key: "messageID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/session/{sessionID}/message/{messageID}",
      ...options,
      ...params
    });
  }
  /**
   * Fork session
   *
   * Create a new session by forking an existing session at a specific message point.
   */
  fork(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "messageID" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/session/{sessionID}/fork",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Abort session
   *
   * Abort an active session and stop any ongoing AI processing or command execution.
   */
  abort(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/session/{sessionID}/abort",
      ...options,
      ...params
    });
  }
  /**
   * Initialize session
   *
   * Analyze the current application and create an AGENTS.md file with project-specific agent configurations.
   */
  init(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "modelID" },
          { in: "body", key: "providerID" },
          { in: "body", key: "messageID" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/session/{sessionID}/init",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Unshare session
   *
   * Remove the shareable link for a session, making it private again.
   */
  unshare(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).delete({
      url: "/session/{sessionID}/share",
      ...options,
      ...params
    });
  }
  /**
   * Share session
   *
   * Create a shareable link for a session, allowing others to view the conversation.
   */
  share(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/session/{sessionID}/share",
      ...options,
      ...params
    });
  }
  /**
   * Summarize session
   *
   * Generate a concise summary of the session using AI compaction to preserve key information.
   */
  summarize(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "providerID" },
          { in: "body", key: "modelID" },
          { in: "body", key: "auto" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/session/{sessionID}/summarize",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Send async message
   *
   * Create and send a new message to a session asynchronously, starting the session if needed and returning immediately.
   */
  promptAsync(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "messageID" },
          { in: "body", key: "model" },
          { in: "body", key: "agent" },
          { in: "body", key: "noReply" },
          { in: "body", key: "tools" },
          { in: "body", key: "format" },
          { in: "body", key: "system" },
          { in: "body", key: "variant" },
          { in: "body", key: "parts" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/session/{sessionID}/prompt_async",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Send command
   *
   * Send a new command to a session for execution by the AI assistant.
   */
  command(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "messageID" },
          { in: "body", key: "agent" },
          { in: "body", key: "model" },
          { in: "body", key: "arguments" },
          { in: "body", key: "command" },
          { in: "body", key: "variant" },
          { in: "body", key: "parts" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/session/{sessionID}/command",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Run shell command
   *
   * Execute a shell command within the session context and return the AI's response.
   */
  shell(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "messageID" },
          { in: "body", key: "agent" },
          { in: "body", key: "model" },
          { in: "body", key: "command" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/session/{sessionID}/shell",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Revert message
   *
   * Revert a specific message in a session, undoing its effects and restoring the previous state.
   */
  revert(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "messageID" },
          { in: "body", key: "partID" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/session/{sessionID}/revert",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Restore reverted messages
   *
   * Restore all previously reverted messages in a session.
   */
  unrevert(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/session/{sessionID}/unrevert",
      ...options,
      ...params
    });
  }
};
var Part = class extends HeyApiClient {
  /**
   * Delete a part from a message.
   */
  delete(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "path", key: "messageID" },
          { in: "path", key: "partID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).delete({
      url: "/session/{sessionID}/message/{messageID}/part/{partID}",
      ...options,
      ...params
    });
  }
  /**
   * Update a part in a message.
   */
  update(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "path", key: "messageID" },
          { in: "path", key: "partID" },
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { key: "part", map: "body" }
        ]
      }
    ]);
    return (options?.client ?? this.client).patch({
      url: "/session/{sessionID}/message/{messageID}/part/{partID}",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var History = class extends HeyApiClient {
  /**
   * List sync events
   *
   * List sync events for all aggregates. Keys are aggregate IDs the client already knows about, values are the last known sequence ID. Events with seq > value are returned for those aggregates. Aggregates not listed in the input get their full history.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { key: "body", map: "body" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/sync/history",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var Sync = class extends HeyApiClient {
  /**
   * Start workspace sync
   *
   * Start sync loops for workspaces in the current project that have active sessions.
   */
  start(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/sync/start",
      ...options,
      ...params
    });
  }
  /**
   * Replay sync events
   *
   * Validate and replay a complete sync event history.
   */
  replay(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          {
            in: "query",
            key: "query_directory",
            map: "directory"
          },
          { in: "query", key: "workspace" },
          {
            in: "body",
            key: "body_directory",
            map: "directory"
          },
          { in: "body", key: "events" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/sync/replay",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Steal session into workspace
   *
   * Update a session to belong to the current workspace through the sync event system.
   */
  steal(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "sessionID" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/sync/steal",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  _history;
  get history() {
    return this._history ??= new History({ client: this.client });
  }
};
var Control = class extends HeyApiClient {
  /**
   * Get next TUI request
   *
   * Retrieve the next TUI request from the queue for processing.
   */
  next(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/tui/control/next",
      ...options,
      ...params
    });
  }
  /**
   * Submit TUI response
   *
   * Submit a response to the TUI request queue to complete a pending request.
   */
  response(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { key: "body", map: "body" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/tui/control/response",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var Tui = class extends HeyApiClient {
  /**
   * Append TUI prompt
   *
   * Append prompt to the TUI.
   */
  appendPrompt(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "text" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/tui/append-prompt",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Open help dialog
   *
   * Open the help dialog in the TUI to display user assistance information.
   */
  openHelp(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/tui/open-help",
      ...options,
      ...params
    });
  }
  /**
   * Open sessions dialog
   *
   * Open the session dialog.
   */
  openSessions(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/tui/open-sessions",
      ...options,
      ...params
    });
  }
  /**
   * Open themes dialog
   *
   * Open the theme dialog.
   */
  openThemes(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/tui/open-themes",
      ...options,
      ...params
    });
  }
  /**
   * Open models dialog
   *
   * Open the model dialog.
   */
  openModels(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/tui/open-models",
      ...options,
      ...params
    });
  }
  /**
   * Submit TUI prompt
   *
   * Submit the prompt.
   */
  submitPrompt(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/tui/submit-prompt",
      ...options,
      ...params
    });
  }
  /**
   * Clear TUI prompt
   *
   * Clear the prompt.
   */
  clearPrompt(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/tui/clear-prompt",
      ...options,
      ...params
    });
  }
  /**
   * Execute TUI command
   *
   * Execute a TUI command.
   */
  executeCommand(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "command" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/tui/execute-command",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Show TUI toast
   *
   * Show a toast notification in the TUI.
   */
  showToast(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "title" },
          { in: "body", key: "message" },
          { in: "body", key: "variant" },
          { in: "body", key: "duration" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/tui/show-toast",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Publish TUI event
   *
   * Publish a TUI event.
   */
  publish(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { key: "body", map: "body" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/tui/publish",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Select session
   *
   * Navigate the TUI to display the specified session.
   */
  selectSession(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "directory" },
          { in: "query", key: "workspace" },
          { in: "body", key: "sessionID" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/tui/select-session",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  _control;
  get control() {
    return this._control ??= new Control({ client: this.client });
  }
};
var Health = class extends HeyApiClient {
  /**
   * Check server health
   *
   * Check whether the API server is ready to accept requests.
   */
  get(options) {
    return (options?.client ?? this.client).get({
      url: "/api/health",
      ...options
    });
  }
};
var Location = class extends HeyApiClient {
  /**
   * Get location
   *
   * Resolve the requested location or the server default location.
   */
  get(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "query", key: "location" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/location",
      ...options,
      ...params
    });
  }
};
var Agent = class extends HeyApiClient {
  /**
   * List agents
   *
   * Retrieve currently registered agents.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "query", key: "location" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/agent",
      ...options,
      ...params
    });
  }
};
var Revert = class extends HeyApiClient {
  /**
   * Stage session revert
   *
   * Stage or move a reversible session boundary and optionally apply its file changes.
   */
  stage(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "body", key: "messageID" },
          { in: "body", key: "files" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/api/session/{sessionID}/revert/stage",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Clear staged revert
   */
  clear(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "path", key: "sessionID" }] }]);
    return (options?.client ?? this.client).post({
      url: "/api/session/{sessionID}/revert/clear",
      ...options,
      ...params
    });
  }
  /**
   * Commit staged revert
   */
  commit(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "path", key: "sessionID" }] }]);
    return (options?.client ?? this.client).post({
      url: "/api/session/{sessionID}/revert/commit",
      ...options,
      ...params
    });
  }
};
var Permission2 = class extends HeyApiClient {
  /**
   * List session permission requests
   *
   * Retrieve pending permission requests owned by a session.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "path", key: "sessionID" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/session/{sessionID}/permission",
      ...options,
      ...params
    });
  }
  /**
   * Create permission request
   *
   * Evaluate and, when approval is required, create a permission request for a session.
   */
  create(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "body", key: "id" },
          { in: "body", key: "action" },
          { in: "body", key: "resources" },
          { in: "body", key: "save" },
          { in: "body", key: "metadata" },
          { in: "body", key: "source" },
          { in: "body", key: "agent" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/api/session/{sessionID}/permission",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Get permission request
   *
   * Retrieve a pending permission request owned by a session.
   */
  get(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "path", key: "requestID" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/api/session/{sessionID}/permission/{requestID}",
      ...options,
      ...params
    });
  }
  /**
   * Reply to pending permission request
   *
   * Respond to a pending permission request owned by a session.
   */
  reply(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "path", key: "requestID" },
          { in: "body", key: "reply" },
          { in: "body", key: "message" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/api/session/{sessionID}/permission/{requestID}/reply",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var Question2 = class extends HeyApiClient {
  /**
   * List session question requests
   *
   * Retrieve pending question requests owned by a session.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "path", key: "sessionID" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/session/{sessionID}/question",
      ...options,
      ...params
    });
  }
  /**
   * Reply to pending question request
   *
   * Answer a pending question request owned by a session.
   */
  reply(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "path", key: "requestID" },
          { key: "questionV2Reply", map: "body" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/api/session/{sessionID}/question/{requestID}/reply",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Reject pending question request
   *
   * Reject a pending question request owned by a session.
   */
  reject(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "path", key: "requestID" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/api/session/{sessionID}/question/{requestID}/reject",
      ...options,
      ...params
    });
  }
};
var Session3 = class extends HeyApiClient {
  /**
   * List sessions
   *
   * Retrieve sessions in the requested order. Items keep that order across pages; use cursor.next or cursor.previous to move through the ordered list.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "workspace" },
          { in: "query", key: "limit" },
          { in: "query", key: "order" },
          { in: "query", key: "search" },
          { in: "query", key: "directory" },
          { in: "query", key: "project" },
          { in: "query", key: "subpath" },
          { in: "query", key: "cursor" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/api/session",
      ...options,
      ...params
    });
  }
  /**
   * Create session
   *
   * Create a session at the requested location.
   */
  create(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "body", key: "id" },
          { in: "body", key: "agent" },
          { in: "body", key: "model" },
          { in: "body", key: "location" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/api/session",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * List active sessions
   *
   * Retrieve foreground Session drains currently owned by this OpenCode process. Sessions absent from the result are inactive.
   */
  active(options) {
    return (options?.client ?? this.client).get({
      url: "/api/session/active",
      ...options
    });
  }
  /**
   * Get session
   *
   * Retrieve a session by ID.
   */
  get(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "path", key: "sessionID" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/session/{sessionID}",
      ...options,
      ...params
    });
  }
  /**
   * Switch session agent
   *
   * Switch the agent used by subsequent provider turns.
   */
  switchAgent(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "body", key: "agent" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/api/session/{sessionID}/agent",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Switch session model
   *
   * Switch the model used by subsequent provider turns.
   */
  switchModel(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "body", key: "model" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/api/session/{sessionID}/model",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Send message
   *
   * Durably admit one session input and schedule agent-loop execution unless resume is false.
   */
  prompt(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "body", key: "id" },
          { in: "body", key: "prompt" },
          { in: "body", key: "delivery" },
          { in: "body", key: "resume" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/api/session/{sessionID}/prompt",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Compact session
   *
   * Compact a session conversation.
   */
  compact(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "path", key: "sessionID" }] }]);
    return (options?.client ?? this.client).post({
      url: "/api/session/{sessionID}/compact",
      ...options,
      ...params
    });
  }
  /**
   * Wait for session
   *
   * Wait for a session agent loop to become idle.
   */
  wait(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "path", key: "sessionID" }] }]);
    return (options?.client ?? this.client).post({
      url: "/api/session/{sessionID}/wait",
      ...options,
      ...params
    });
  }
  /**
   * Get session context
   *
   * Retrieve the active context messages for a session (all messages after the last compaction).
   */
  context(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "path", key: "sessionID" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/session/{sessionID}/context",
      ...options,
      ...params
    });
  }
  /**
   * Get session history
   *
   * Read one finite page of public durable Session events after an exclusive aggregate sequence. Newly committed events may appear on later pages.
   */
  history(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "limit" },
          { in: "query", key: "after" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/api/session/{sessionID}/history",
      ...options,
      ...params
    });
  }
  /**
   * Subscribe to session events
   *
   * Replay durable events after an aggregate sequence, then continue with new durable events.
   */
  events(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "after" }
        ]
      }
    ]);
    return (options?.client ?? this.client).sse.get({
      url: "/api/session/{sessionID}/event",
      ...options,
      ...params
    });
  }
  /**
   * Interrupt session execution
   *
   * Interrupt active execution owned by this OpenCode process. Idle interruption is a no-op.
   */
  interrupt(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "path", key: "sessionID" }] }]);
    return (options?.client ?? this.client).post({
      url: "/api/session/{sessionID}/interrupt",
      ...options,
      ...params
    });
  }
  /**
   * Get session message
   *
   * Retrieve one projected message owned by the Session.
   */
  message(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "path", key: "messageID" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/api/session/{sessionID}/message/{messageID}",
      ...options,
      ...params
    });
  }
  /**
   * Get session messages
   *
   * Retrieve projected messages for a session. Items keep the requested order across pages; use cursor.next or cursor.previous to move through the ordered timeline.
   */
  messages(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "sessionID" },
          { in: "query", key: "limit" },
          { in: "query", key: "order" },
          { in: "query", key: "cursor" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/api/session/{sessionID}/message",
      ...options,
      ...params
    });
  }
  _revert;
  get revert() {
    return this._revert ??= new Revert({ client: this.client });
  }
  _permission;
  get permission() {
    return this._permission ??= new Permission2({ client: this.client });
  }
  _question;
  get question() {
    return this._question ??= new Question2({ client: this.client });
  }
};
var Model = class extends HeyApiClient {
  /**
   * List models
   *
   * Retrieve available models ordered by release date.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "query", key: "location" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/model",
      ...options,
      ...params
    });
  }
};
var Provider2 = class extends HeyApiClient {
  /**
   * List providers
   *
   * Retrieve active AI providers so clients can show provider availability and configuration.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "query", key: "location" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/provider",
      ...options,
      ...params
    });
  }
  /**
   * Get provider
   *
   * Retrieve a single AI provider so clients can inspect its availability and endpoint settings.
   */
  get(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "providerID" },
          { in: "query", key: "location" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/api/provider/{providerID}",
      ...options,
      ...params
    });
  }
};
var Connect = class extends HeyApiClient {
  /**
   * Connect with key
   *
   * Run a key authentication method and store the resulting credential.
   */
  key(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "integrationID" },
          { in: "query", key: "location" },
          { in: "body", key: "key" },
          { in: "body", key: "label" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/api/integration/{integrationID}/connect/key",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Begin OAuth connection
   *
   * Start an OAuth attempt and return the authorization details.
   */
  oauth(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "integrationID" },
          { in: "query", key: "location" },
          { in: "body", key: "methodID" },
          { in: "body", key: "inputs" },
          { in: "body", key: "label" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/api/integration/{integrationID}/connect/oauth",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var Attempt = class extends HeyApiClient {
  /**
   * Cancel OAuth connection
   *
   * Cancel an OAuth attempt and release its resources.
   */
  cancel(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "attemptID" },
          { in: "query", key: "location" }
        ]
      }
    ]);
    return (options?.client ?? this.client).delete({
      url: "/api/integration/attempt/{attemptID}",
      ...options,
      ...params
    });
  }
  /**
   * Get OAuth attempt status
   *
   * Poll the current status of an OAuth attempt.
   */
  status(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "attemptID" },
          { in: "query", key: "location" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/api/integration/attempt/{attemptID}",
      ...options,
      ...params
    });
  }
  /**
   * Complete OAuth connection
   *
   * Complete a code-based OAuth attempt and store the resulting credential.
   */
  complete(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "attemptID" },
          { in: "query", key: "location" },
          { in: "body", key: "code" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/api/integration/attempt/{attemptID}/complete",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var Integration = class extends HeyApiClient {
  /**
   * List integrations
   *
   * Retrieve available integrations and their authentication methods.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "query", key: "location" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/integration",
      ...options,
      ...params
    });
  }
  /**
   * Get integration
   *
   * Retrieve one integration and its authentication methods.
   */
  get(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "integrationID" },
          { in: "query", key: "location" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/api/integration/{integrationID}",
      ...options,
      ...params
    });
  }
  _connect;
  get connect() {
    return this._connect ??= new Connect({ client: this.client });
  }
  _attempt;
  get attempt() {
    return this._attempt ??= new Attempt({ client: this.client });
  }
};
var Credential = class extends HeyApiClient {
  /**
   * Remove credential
   *
   * Remove a stored integration credential.
   */
  remove(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "credentialID" },
          { in: "query", key: "location" }
        ]
      }
    ]);
    return (options?.client ?? this.client).delete({
      url: "/api/credential/{credentialID}",
      ...options,
      ...params
    });
  }
  /**
   * Update credential
   *
   * Update a stored credential label.
   */
  update(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "credentialID" },
          { in: "query", key: "location" },
          { in: "body", key: "label" }
        ]
      }
    ]);
    return (options?.client ?? this.client).patch({
      url: "/api/credential/{credentialID}",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
};
var Request2 = class extends HeyApiClient {
  /**
   * List pending permission requests
   *
   * Retrieve pending permission requests for a location.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "query", key: "location" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/permission/request",
      ...options,
      ...params
    });
  }
};
var Saved = class extends HeyApiClient {
  /**
   * List saved permissions
   *
   * Retrieve saved permissions, optionally filtered by project.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "query", key: "projectID" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/permission/saved",
      ...options,
      ...params
    });
  }
  /**
   * Remove saved permission
   *
   * Remove a saved permission by ID.
   */
  remove(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "path", key: "id" }] }]);
    return (options?.client ?? this.client).delete({
      url: "/api/permission/saved/{id}",
      ...options,
      ...params
    });
  }
};
var Permission3 = class extends HeyApiClient {
  _request;
  get request() {
    return this._request ??= new Request2({ client: this.client });
  }
  _saved;
  get saved() {
    return this._saved ??= new Saved({ client: this.client });
  }
};
var Fs = class extends HeyApiClient {
  /**
   * Read file
   *
   * Serve one file relative to the requested location.
   */
  read(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "query", key: "location" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/fs/read/*",
      ...options,
      ...params
    });
  }
  /**
   * List directory
   *
   * List direct children of one directory relative to the requested location.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "location" },
          { in: "query", key: "path" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/api/fs/list",
      ...options,
      ...params
    });
  }
  /**
   * Find files
   *
   * Find recursively ranked filesystem entries relative to the requested location.
   */
  find(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "location" },
          { in: "query", key: "query" },
          { in: "query", key: "type" },
          { in: "query", key: "limit" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/api/fs/find",
      ...options,
      ...params
    });
  }
};
var Command2 = class extends HeyApiClient {
  /**
   * List commands
   *
   * Retrieve currently registered commands.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "query", key: "location" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/command",
      ...options,
      ...params
    });
  }
};
var Skill = class extends HeyApiClient {
  /**
   * List skills
   *
   * Retrieve currently registered skills.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "query", key: "location" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/skill",
      ...options,
      ...params
    });
  }
};
var Event2 = class extends HeyApiClient {
  /**
   * Subscribe to events
   *
   * Subscribe to native event payloads for the server.
   */
  subscribe(options) {
    return (options?.client ?? this.client).sse.get({
      url: "/api/event",
      ...options
    });
  }
};
var Pty2 = class extends HeyApiClient {
  /**
   * List PTY sessions
   *
   * List PTY sessions for a location, including exited sessions retained until removal.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "query", key: "location" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/pty",
      ...options,
      ...params
    });
  }
  /**
   * Create PTY session
   *
   * Create a pseudo-terminal session for a location.
   */
  create(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "query", key: "location" },
          { in: "body", key: "command" },
          { in: "body", key: "args" },
          { in: "body", key: "cwd" },
          { in: "body", key: "title" },
          { in: "body", key: "env" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/api/pty",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Remove PTY session
   *
   * Terminate and remove one PTY session.
   */
  remove(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "ptyID" },
          { in: "query", key: "location" }
        ]
      }
    ]);
    return (options?.client ?? this.client).delete({
      url: "/api/pty/{ptyID}",
      ...options,
      ...params
    });
  }
  /**
   * Get PTY session
   *
   * Get one PTY session, including its exit code once exited.
   */
  get(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "ptyID" },
          { in: "query", key: "location" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/api/pty/{ptyID}",
      ...options,
      ...params
    });
  }
  /**
   * Update PTY session
   *
   * Update the title or viewport size of one PTY session.
   */
  update(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "ptyID" },
          { in: "query", key: "location" },
          { in: "body", key: "title" },
          { in: "body", key: "size" }
        ]
      }
    ]);
    return (options?.client ?? this.client).put({
      url: "/api/pty/{ptyID}",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  /**
   * Create PTY WebSocket token
   *
   * Create a short-lived single-use ticket for opening a PTY WebSocket connection.
   */
  connectToken(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "ptyID" },
          { in: "query", key: "location" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/api/pty/{ptyID}/connect-token",
      ...options,
      ...params
    });
  }
  /**
   * Connect to PTY session
   *
   * Establish a WebSocket connection streaming PTY output and accepting terminal input.
   */
  connect(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "ptyID" },
          { in: "query", key: "location[directory]" },
          { in: "query", key: "location[workspace]" },
          { in: "query", key: "cursor" },
          { in: "query", key: "ticket" }
        ]
      }
    ]);
    return (options?.client ?? this.client).get({
      url: "/api/pty/{ptyID}/connect",
      ...options,
      ...params
    });
  }
};
var Request22 = class extends HeyApiClient {
  /**
   * List pending question requests
   *
   * Retrieve pending question requests for a location.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "query", key: "location" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/question/request",
      ...options,
      ...params
    });
  }
};
var Question3 = class extends HeyApiClient {
  _request;
  get request() {
    return this._request ??= new Request22({ client: this.client });
  }
};
var Reference = class extends HeyApiClient {
  /**
   * List references
   *
   * List references available in the requested location.
   */
  list(parameters, options) {
    const params = buildClientParams([parameters], [{ args: [{ in: "query", key: "location" }] }]);
    return (options?.client ?? this.client).get({
      url: "/api/reference",
      ...options,
      ...params
    });
  }
};
var ProjectCopy2 = class extends HeyApiClient {
  remove(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "projectID" },
          { in: "query", key: "location" },
          { in: "body", key: "directory" },
          { in: "body", key: "force" }
        ]
      }
    ]);
    return (options?.client ?? this.client).delete({
      url: "/experimental/project/{projectID}/copy",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  create(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "projectID" },
          { in: "query", key: "location" },
          { in: "body", key: "strategy" },
          { in: "body", key: "directory" },
          { in: "body", key: "name" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/experimental/project/{projectID}/copy",
      ...options,
      ...params,
      headers: {
        "Content-Type": "application/json",
        ...options?.headers,
        ...params.headers
      }
    });
  }
  refresh(parameters, options) {
    const params = buildClientParams([parameters], [
      {
        args: [
          { in: "path", key: "projectID" },
          { in: "query", key: "location" }
        ]
      }
    ]);
    return (options?.client ?? this.client).post({
      url: "/experimental/project/{projectID}/copy/refresh",
      ...options,
      ...params
    });
  }
};
var V2 = class extends HeyApiClient {
  _health;
  get health() {
    return this._health ??= new Health({ client: this.client });
  }
  _location;
  get location() {
    return this._location ??= new Location({ client: this.client });
  }
  _agent;
  get agent() {
    return this._agent ??= new Agent({ client: this.client });
  }
  _session;
  get session() {
    return this._session ??= new Session3({ client: this.client });
  }
  _model;
  get model() {
    return this._model ??= new Model({ client: this.client });
  }
  _provider;
  get provider() {
    return this._provider ??= new Provider2({ client: this.client });
  }
  _integration;
  get integration() {
    return this._integration ??= new Integration({ client: this.client });
  }
  _credential;
  get credential() {
    return this._credential ??= new Credential({ client: this.client });
  }
  _permission;
  get permission() {
    return this._permission ??= new Permission3({ client: this.client });
  }
  _fs;
  get fs() {
    return this._fs ??= new Fs({ client: this.client });
  }
  _command;
  get command() {
    return this._command ??= new Command2({ client: this.client });
  }
  _skill;
  get skill() {
    return this._skill ??= new Skill({ client: this.client });
  }
  _event;
  get event() {
    return this._event ??= new Event2({ client: this.client });
  }
  _pty;
  get pty() {
    return this._pty ??= new Pty2({ client: this.client });
  }
  _question;
  get question() {
    return this._question ??= new Question3({ client: this.client });
  }
  _reference;
  get reference() {
    return this._reference ??= new Reference({ client: this.client });
  }
  _projectCopy;
  get projectCopy() {
    return this._projectCopy ??= new ProjectCopy2({ client: this.client });
  }
};
var OpencodeClient = class _OpencodeClient extends HeyApiClient {
  static __registry = new HeyApiRegistry();
  constructor(args) {
    super(args);
    _OpencodeClient.__registry.set(this, args?.key);
  }
  _auth;
  get auth() {
    return this._auth ??= new Auth({ client: this.client });
  }
  _app;
  get app() {
    return this._app ??= new App({ client: this.client });
  }
  _experimental;
  get experimental() {
    return this._experimental ??= new Experimental({ client: this.client });
  }
  _global;
  get global() {
    return this._global ??= new Global({ client: this.client });
  }
  _event;
  get event() {
    return this._event ??= new Event({ client: this.client });
  }
  _config;
  get config() {
    return this._config ??= new Config2({ client: this.client });
  }
  _tool;
  get tool() {
    return this._tool ??= new Tool({ client: this.client });
  }
  _worktree;
  get worktree() {
    return this._worktree ??= new Worktree({ client: this.client });
  }
  _find;
  get find() {
    return this._find ??= new Find({ client: this.client });
  }
  _file;
  get file() {
    return this._file ??= new File({ client: this.client });
  }
  _instance;
  get instance() {
    return this._instance ??= new Instance({ client: this.client });
  }
  _path;
  get path() {
    return this._path ??= new Path({ client: this.client });
  }
  _vcs;
  get vcs() {
    return this._vcs ??= new Vcs({ client: this.client });
  }
  _command;
  get command() {
    return this._command ??= new Command({ client: this.client });
  }
  _lsp;
  get lsp() {
    return this._lsp ??= new Lsp({ client: this.client });
  }
  _formatter;
  get formatter() {
    return this._formatter ??= new Formatter({ client: this.client });
  }
  _mcp;
  get mcp() {
    return this._mcp ??= new Mcp({ client: this.client });
  }
  _project;
  get project() {
    return this._project ??= new Project({ client: this.client });
  }
  _pty;
  get pty() {
    return this._pty ??= new Pty({ client: this.client });
  }
  _question;
  get question() {
    return this._question ??= new Question({ client: this.client });
  }
  _permission;
  get permission() {
    return this._permission ??= new Permission({ client: this.client });
  }
  _provider;
  get provider() {
    return this._provider ??= new Provider({ client: this.client });
  }
  _session;
  get session() {
    return this._session ??= new Session2({ client: this.client });
  }
  _part;
  get part() {
    return this._part ??= new Part({ client: this.client });
  }
  _sync;
  get sync() {
    return this._sync ??= new Sync({ client: this.client });
  }
  _tui;
  get tui() {
    return this._tui ??= new Tui({ client: this.client });
  }
  _v2;
  get v2() {
    return this._v2 ??= new V2({ client: this.client });
  }
};

// node_modules/@opencode-ai/sdk/dist/error-interceptor.js
function wrapClientError(error, response, request, opts) {
  if (!opts?.throwOnError)
    return error;
  if (error instanceof Error)
    return error;
  if (typeof error === "object" && error !== null && Object.keys(error).length > 0) {
    const obj = error;
    const message = typeof obj.data?.message === "string" && obj.data.message || typeof obj.message === "string" && obj.message || typeof obj.name === "string" && obj.name || describe(request, response);
    return new Error(message, { cause: { body: error, status: response?.status } });
  }
  if (typeof error === "string" && error.length > 0) {
    return new Error(error, { cause: { body: error, status: response?.status } });
  }
  const reason = response ? "(empty response body)" : "network error (no response)";
  return new Error(`opencode server ${describe(request, response)}: ${reason}`, {
    cause: { body: error, status: response?.status }
  });
}
function describe(request, response) {
  const method = request?.method ?? "?";
  const url = request?.url ?? "?";
  const status = response?.status;
  const statusText = response?.statusText;
  return `${method} ${url}${status ? " \u2192 " + status : ""}${statusText ? " " + statusText : ""}`;
}

// node_modules/@opencode-ai/sdk/dist/v2/client.js
function pick(value, fallback, encode) {
  if (!value)
    return;
  if (!fallback)
    return value;
  if (value === fallback)
    return fallback;
  if (encode && value === encode(fallback))
    return fallback;
  return value;
}
function rewrite(request, values) {
  if (request.method !== "GET" && request.method !== "HEAD")
    return request;
  const url = new URL(request.url);
  let changed = false;
  for (const [name, key] of [
    ["x-opencode-directory", "directory"],
    ["x-opencode-workspace", "workspace"]
  ]) {
    const value = pick(request.headers.get(name), key === "directory" ? values.directory : values.workspace, key === "directory" ? encodeURIComponent : void 0);
    if (!value)
      continue;
    for (const query of url.pathname.startsWith("/api/") ? [key, `location[${key}]`] : [key]) {
      if (!url.searchParams.has(query)) {
        url.searchParams.set(query, value);
      }
    }
    changed = true;
  }
  if (!changed)
    return request;
  const next = new Request(url, request);
  next.headers.delete("x-opencode-directory");
  next.headers.delete("x-opencode-workspace");
  return next;
}
function createOpencodeClient(config) {
  if (!config?.fetch) {
    const customFetch = (req) => {
      req.timeout = false;
      return fetch(req);
    };
    config = {
      ...config,
      fetch: customFetch
    };
  }
  if (config?.directory) {
    config.headers = {
      ...config.headers,
      "x-opencode-directory": encodeURIComponent(config.directory)
    };
  }
  if (config?.experimental_workspaceID) {
    config.headers = {
      ...config.headers,
      "x-opencode-workspace": config.experimental_workspaceID
    };
  }
  const client2 = createClient(config);
  client2.interceptors.request.use((request) => rewrite(request, {
    directory: config?.directory,
    workspace: config?.experimental_workspaceID
  }));
  client2.interceptors.response.use((response) => {
    const contentType = response.headers.get("content-type");
    if (contentType === "text/html")
      throw new Error("Request is not supported by this version of OpenCode Server (Server responded with text/html)");
    return response;
  });
  client2.interceptors.error.use(wrapClientError);
  return new OpencodeClient({ client: client2 });
}

// node_modules/@opencode-ai/sdk/dist/v2/server.js
var import_cross_spawn = __toESM(require_cross_spawn(), 1);

// node_modules/@opencode-ai/sdk/dist/process.js
var import_node_child_process = require("node:child_process");
function stop(proc) {
  if (proc.exitCode !== null || proc.signalCode !== null)
    return;
  if (process.platform === "win32" && proc.pid) {
    const out = (0, import_node_child_process.spawnSync)("taskkill", ["/pid", String(proc.pid), "/T", "/F"], { windowsHide: true });
    if (!out.error && out.status === 0)
      return;
  }
  proc.kill();
}
function bindAbort(proc, signal, onAbort) {
  if (!signal)
    return () => {
    };
  const abort = () => {
    clear();
    stop(proc);
    onAbort?.();
  };
  const clear = () => {
    signal.removeEventListener("abort", abort);
    proc.off("exit", clear);
    proc.off("error", clear);
  };
  signal.addEventListener("abort", abort, { once: true });
  proc.on("exit", clear);
  proc.on("error", clear);
  if (signal.aborted)
    abort();
  return clear;
}

// node_modules/@opencode-ai/sdk/dist/v2/server.js
async function createOpencodeServer(options) {
  options = Object.assign({
    hostname: "127.0.0.1",
    port: 4096,
    timeout: 5e3
  }, options ?? {});
  const args = [`serve`, `--hostname=${options.hostname}`, `--port=${options.port}`];
  if (options.config?.logLevel)
    args.push(`--log-level=${options.config.logLevel}`);
  const proc = (0, import_cross_spawn.default)(`opencode`, args, {
    env: {
      ...process.env,
      OPENCODE_CONFIG_CONTENT: JSON.stringify(options.config ?? {})
    }
  });
  let clear = () => {
  };
  const url = await new Promise((resolve, reject) => {
    const id = setTimeout(() => {
      clear();
      stop(proc);
      reject(new Error(`Timeout waiting for server to start after ${options.timeout}ms`));
    }, options.timeout);
    let output = "";
    let resolved = false;
    proc.stdout?.on("data", (chunk) => {
      if (resolved)
        return;
      output += chunk.toString();
      const lines = output.split("\n");
      for (const line of lines) {
        if (line.startsWith("opencode server listening")) {
          const match = line.match(/on\s+(https?:\/\/[^\s]+)/);
          if (!match) {
            clear();
            stop(proc);
            clearTimeout(id);
            reject(new Error(`Failed to parse server url from output: ${line}`));
            return;
          }
          clearTimeout(id);
          resolved = true;
          resolve(match[1]);
          return;
        }
      }
    });
    proc.stderr?.on("data", (chunk) => {
      output += chunk.toString();
    });
    proc.on("exit", (code) => {
      clearTimeout(id);
      let msg = `Server exited with code ${code}`;
      if (output.trim()) {
        msg += `
Server output: ${output}`;
      }
      reject(new Error(msg));
    });
    proc.on("error", (error) => {
      clearTimeout(id);
      reject(error);
    });
    clear = bindAbort(proc, options.signal, () => {
      clearTimeout(id);
      reject(options.signal?.reason);
    });
  });
  return {
    url,
    close() {
      clear();
      stop(proc);
    }
  };
}

// node_modules/@opencode-ai/sdk/dist/v2/index.js
async function createOpencode(options) {
  const server = await createOpencodeServer({
    ...options
  });
  const client2 = createOpencodeClient({
    baseUrl: server.url
  });
  return {
    client: client2,
    server
  };
}

// scripts/opencode-bridge.mjs
function send(message) {
  process.stdout.write(`${JSON.stringify(message)}
`);
}
async function readRequest(lines) {
  const { value, done } = await lines[Symbol.asyncIterator]().next();
  if (done) throw new Error("Missing request");
  return JSON.parse(value);
}
async function oneShot(client2, request) {
  switch (request.action) {
    case "providers": {
      const providers = (await client2.provider.list()).data;
      const connected = new Set(providers.connected);
      return {
        all: providers.all.filter((provider) => connected.has(provider.id)).map((provider) => ({
          id: provider.id,
          name: provider.name,
          models: Object.fromEntries(
            Object.entries(provider.models).map(([id, model]) => [
              id,
              {
                name: model.name,
                variants: Object.keys(model.variants ?? {}),
                capabilities: {
                  attachment: model.capabilities.attachment,
                  input: {
                    image: model.capabilities.input.image,
                    pdf: model.capabilities.input.pdf
                  }
                }
              }
            ])
          )
        }))
      };
    }
    case "commands":
      return (await client2.command.list()).data.map((command) => ({
        name: command.name,
        description: command.description ?? ""
      }));
    case "title": {
      const sessionId = await ensureSession(client2);
      const body = { parts: [{ type: "text", text: request.prompt }] };
      if (request.model) body.model = request.model;
      if (request.variant) body.variant = request.variant;
      const result = await client2.session.prompt({ sessionID: sessionId, ...body });
      if (result.error) throw new Error(JSON.stringify(result.error));
      return result.data.parts.filter((part) => part.type === "text").map((part) => part.text).join("");
    }
    case "fork": {
      const result = await client2.session.fork({
        sessionID: request.sessionId,
        messageID: request.position
      });
      if (result.error) throw new Error(JSON.stringify(result.error));
      return result.data.id;
    }
    default:
      throw new Error(`Unknown action: ${request.action}`);
  }
}
async function ensureSession(client2, sessionId) {
  if (sessionId) {
    const existing = await client2.session.get({ sessionID: sessionId });
    if (!existing.error) return sessionId;
  }
  const created = await client2.session.create();
  if (created.error) throw new Error(JSON.stringify(created.error));
  return created.data.id;
}
function automaticPermissionReply(mode) {
  return mode === "build" ? "always" : void 0;
}
async function runPrompt(client2, lines, request) {
  const sessionId = await ensureSession(client2, request.sessionId);
  const subscription = await client2.event.subscribe();
  send({ type: "ready", sessionId });
  let cancelled = false;
  const input = (async () => {
    for await (const line of lines) {
      if (!line.trim()) continue;
      const command = JSON.parse(line);
      if (command.action === "permission") {
        const result = await client2.permission.reply({
          requestID: command.requestId,
          reply: command.reply
        });
        if (result.error) send({ type: "error", error: JSON.stringify(result.error) });
      } else if (command.action === "cancel") {
        cancelled = true;
        await client2.session.abort({ sessionID: sessionId });
      }
    }
  })();
  const body = {};
  if (request.model) body.model = request.model;
  if (request.agent) body.agent = request.agent;
  if (request.variant) body.variant = request.variant;
  let started;
  if (request.command) {
    started = client2.session.command({
      sessionID: sessionId,
      command: request.command,
      arguments: request.arguments ?? "",
      parts: request.parts.filter((part) => part.type === "file"),
      model: request.model ? `${request.model.providerID}/${request.model.modelID}` : void 0,
      agent: request.agent,
      variant: request.variant
    });
  } else {
    started = client2.session.promptAsync({
      sessionID: sessionId,
      ...body,
      parts: request.parts
    });
  }
  started.then((result) => {
    if (result.error && !cancelled) send({ type: "error", error: JSON.stringify(result.error) });
  }).catch((error) => {
    if (!cancelled) send({ type: "error", error: error instanceof Error ? error.message : String(error) });
  });
  const assistantMessages = /* @__PURE__ */ new Set();
  const pendingParts = /* @__PURE__ */ new Map();
  for await (const event of subscription.stream) {
    const properties = event.properties ?? {};
    if (properties.sessionID !== sessionId) continue;
    if (event.type === "message.updated") {
      if (properties.info?.role === "assistant") {
        assistantMessages.add(properties.info.id);
        for (const part of pendingParts.get(properties.info.id)?.values() ?? []) send({ type: "part", part });
        pendingParts.delete(properties.info.id);
      }
      continue;
    }
    if (event.type === "message.part.updated") {
      const part = properties.part;
      if (assistantMessages.has(part?.messageID)) {
        send({ type: "part", part });
      } else if (part?.messageID && part?.id) {
        const parts = pendingParts.get(part.messageID) ?? /* @__PURE__ */ new Map();
        parts.set(part.id, part);
        pendingParts.set(part.messageID, parts);
      }
      continue;
    }
    if (event.type === "permission.asked" || event.type === "permission.v2.asked") {
      const reply = automaticPermissionReply(request.mode);
      if (reply) {
        const result = await client2.permission.reply({ requestID: properties.id, reply });
        if (result.error) send({ type: "error", error: JSON.stringify(result.error) });
      } else {
        send({ type: "permission", permission: properties });
      }
      continue;
    }
    if (event.type === "session.error") {
      if (cancelled) break;
      send({ type: "error", error: JSON.stringify(properties.error ?? "OpenCode session error") });
      break;
    }
    if (event.type === "session.idle") {
      const position = [...assistantMessages].at(-1);
      if (position) send({ type: "checkpoint", sessionId, position });
      send({ type: "done" });
      break;
    }
  }
  void input;
}
async function main() {
  const lines = (0, import_node_readline.createInterface)({ input: process.stdin, crlfDelay: Infinity });
  let opencode;
  try {
    const request = await readRequest(lines);
    opencode = await createOpencode({ hostname: "127.0.0.1", port: 0, timeout: 1e4 });
    const { client: client2 } = opencode;
    if (request.action === "prompt") {
      await runPrompt(client2, lines, request);
    } else {
      send({ ok: true, data: await oneShot(client2, request) });
    }
  } catch (error) {
    send({ ok: false, error: error instanceof Error ? error.message : String(error) });
    process.exitCode = 1;
  } finally {
    lines.close();
    opencode?.server.close();
  }
}
if (process.env.NOVA_OPENCODE_BRIDGE_TEST !== "1") void main();
// Annotate the CommonJS export names for ESM import in node:
0 && (module.exports = {
  automaticPermissionReply
});
