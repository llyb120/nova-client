// Playwright 页面内操作采集器。保持独立文件，避免宿主模板字符串的多层转义破坏脚本。
(() => {
  if (window.__novaRecorderInstalled) return;
  window.__novaRecorderInstalled = true;

  const cssEscape = (value) =>
    window.CSS?.escape ? CSS.escape(value) : String(value).replace(/[^a-zA-Z0-9_-]/g, (ch) => `\\${ch}`);

  const unique = (selector) => {
    try {
      return document.querySelectorAll(selector).length === 1;
    } catch {
      return false;
    }
  };

  const attrSelector = (element, attr) => {
    const value = element.getAttribute(attr);
    if (!value) return null;
    return `${element.tagName.toLowerCase()}[${attr}="${value.replace(/"/g, '\\"')}"]`;
  };

  const shortText = (element) => {
    const text = (element.innerText || element.textContent || "").trim().replace(/\s+/g, " ");
    return text && text.length <= 48 ? text : null;
  };

  const buildSelector = (element) => {
    if (!(element instanceof Element)) return "";
    const tag = element.tagName.toLowerCase();
    if (element.id) {
      const selector = `#${cssEscape(element.id)}`;
      if (unique(selector)) return selector;
    }
    for (const attr of ["data-testid", "data-test", "data-cy", "aria-label", "name", "placeholder", "role"]) {
      const selector = attrSelector(element, attr);
      if (selector && unique(selector)) return selector;
    }
    if (tag === "button" || tag === "a" || element.getAttribute("role") === "button") {
      const text = shortText(element);
      if (text) return `${tag}:has-text("${text.replace(/"/g, '\\"')}")`;
    }
    const path = [];
    let current = element;
    while (current && current !== document.documentElement && path.length < 7) {
      let part = current.tagName.toLowerCase();
      if (current.id) {
        part += `#${cssEscape(current.id)}`;
        path.unshift(part);
        break;
      }
      const parent = current.parentElement;
      if (parent) {
        const siblings = Array.from(parent.children).filter((child) => child.tagName === current.tagName);
        if (siblings.length > 1) part += `:nth-of-type(${siblings.indexOf(current) + 1})`;
      }
      path.unshift(part);
      current = parent;
    }
    return path.join(" > ");
  };

  const describe = (element) => {
    const rect = element?.getBoundingClientRect?.();
    return {
      selector: buildSelector(element),
      tag: element?.tagName?.toLowerCase?.() || "",
      id: element?.id || undefined,
      name: element?.name || undefined,
      ariaLabel: element?.getAttribute?.("aria-label") || undefined,
      text: element ? shortText(element) || undefined : undefined,
      href: element?.tagName === "A" ? element.href : undefined,
      rect: rect ? { x: rect.x, y: rect.y, width: rect.width, height: rect.height } : undefined,
    };
  };

  const record = (kind, element, data) => {
    if (!window.__novaRecording || window.__novaRecordingPaused) return;
    Promise.resolve(window.__novaPush({
      kind,
      url: location.href,
      target: element ? describe(element) : null,
      data: data || null,
    })).catch(() => {});
  };

  document.addEventListener("click", (event) => {
    const source = event.target;
    const element = source?.closest?.('a,button,[role="button"],input[type="submit"],[onclick]') || source;
    record("click", element);
  }, true);

  document.addEventListener("input", (event) => {
    const element = event.target;
    const tag = element?.tagName?.toLowerCase?.();
    if (tag === "input" || tag === "textarea") record("input", element, { value: element.value });
    else if (element?.isContentEditable) record("input", element, { value: element.innerText });
  }, true);

  // paste 触发时浏览器尚未必把剪贴板内容写入 value；延后一轮读取最终结果。
  // 随后产生的原生 input 与这里的记录会按同一 selector 在后端合并，只留下最终值。
  document.addEventListener("paste", (event) => {
    const element = event.target;
    const tag = element?.tagName?.toLowerCase?.();
    if (tag !== "input" && tag !== "textarea" && !element?.isContentEditable) return;
    setTimeout(() => {
      const value = element.isContentEditable ? element.innerText : element.value;
      record("input", element, { value, inputType: "paste" });
    }, 0);
  }, true);

  document.addEventListener("change", (event) => {
    const element = event.target;
    const tag = element?.tagName?.toLowerCase?.();
    if (tag === "select") record("change", element, { value: element.value });
    else if (tag === "input" && (element.type === "checkbox" || element.type === "radio")) {
      record("change", element, { checked: Boolean(element.checked), value: element.value });
    }
  }, true);

  document.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === "Tab") record("key", event.target, { key: event.key });
  }, true);

  document.addEventListener("submit", (event) => record("submit", event.target), true);
  Promise.resolve(window.__novaCollectorReady?.({ url: location.href })).catch(() => {});
})();
