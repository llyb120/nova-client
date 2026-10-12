export interface MeteorPoint {
  x: number;
  y: number;
}

export interface MeteorRegion extends MeteorPoint {
  width: number;
  height: number;
}

/** Convert viewport CSS pixels to the original screenshot's pixels (including mixed-DPI screens). */
export function meteorRegion(
  start: MeteorPoint,
  end: MeteorPoint,
  bounds: { left: number; top: number; width: number; height: number },
  image: { width: number; height: number },
): MeteorRegion | null {
  if (![start.x, start.y, end.x, end.y, bounds.left, bounds.top, bounds.width, bounds.height, image.width, image.height].every(Number.isFinite)
    || bounds.width <= 0 || bounds.height <= 0 || image.width <= 0 || image.height <= 0) return null;
  const pixel = (point: MeteorPoint) => ({
    x: Math.max(0, Math.min(image.width, (point.x - bounds.left) * image.width / bounds.width)),
    y: Math.max(0, Math.min(image.height, (point.y - bounds.top) * image.height / bounds.height)),
  });
  const a = pixel(start);
  const b = pixel(end);
  if (Math.abs(end.x - start.x) < 4 && Math.abs(end.y - start.y) < 4) {
    return { x: Math.min(image.width - 1, Math.floor(a.x)), y: Math.min(image.height - 1, Math.floor(a.y)), width: 0, height: 0 };
  }
  const x = Math.min(image.width - 1, Math.floor(Math.min(a.x, b.x)));
  const y = Math.min(image.height - 1, Math.floor(Math.min(a.y, b.y)));
  return {
    x,
    y,
    width: Math.max(1, Math.ceil(Math.max(a.x, b.x)) - x),
    height: Math.max(1, Math.ceil(Math.max(a.y, b.y)) - y),
  };
}

export interface MeteorScreen {
  width: number;
  height: number;
  monitors?: MeteorRegion[];
  pointer?: MeteorPoint;
}

/** Prefer the display containing a point, or the largest overlap for a cross-display region. */
export function meteorMonitor(image: MeteorScreen, region?: MeteorRegion): MeteorRegion {
  const displays = image.monitors?.filter((m) => [m.x, m.y, m.width, m.height].every(Number.isFinite)
    && m.x >= 0 && m.y >= 0 && m.width > 0 && m.height > 0 && m.x + m.width <= image.width && m.y + m.height <= image.height);
  if (!displays?.length) return { x: 0, y: 0, width: image.width, height: image.height };
  const target = region ?? { ...(image.pointer ?? { x: 0, y: 0 }), width: 0, height: 0 };
  let best = displays[0];
  let bestArea = -1;
  let bestDistance = Infinity;
  for (const monitor of displays) {
    const area = Math.max(0, Math.min(target.x + target.width, monitor.x + monitor.width) - Math.max(target.x, monitor.x))
      * Math.max(0, Math.min(target.y + target.height, monitor.y + monitor.height) - Math.max(target.y, monitor.y));
    const distance = Math.max(monitor.x - target.x, 0, target.x - (monitor.x + monitor.width)) ** 2
      + Math.max(monitor.y - target.y, 0, target.y - (monitor.y + monitor.height)) ** 2;
    const contains = target.x >= monitor.x && target.x < monitor.x + monitor.width
      && target.y >= monitor.y && target.y < monitor.y + monitor.height;
    if (target.width === 0 && target.height === 0 && contains) return monitor;
    if (area > bestArea || (area === bestArea && distance < bestDistance)) {
      best = monitor;
      bestArea = area;
      bestDistance = distance;
    }
  }
  return best;
}

export interface MeteorAnnotation {
  text: string;
  region: MeteorRegion;
}

export const METEOR_MAX_ANNOTATIONS = 20;
export const METEOR_MAX_TEXT = 16_000;

export function meteorRequest(captureId: string, annotations: readonly MeteorAnnotation[]) {
  if (!captureId || annotations.length === 0 || annotations.length > METEOR_MAX_ANNOTATIONS) return null;
  const normalized = annotations.map(({ text, region }) => ({ text: text.trim(), region: { ...region } }));
  if (!normalized.some(({ text }) => text)
    || normalized.reduce((sum, { text }) => sum + [...text].length, 0) > METEOR_MAX_TEXT
    || normalized.some(({ region }) => ![region.x, region.y, region.width, region.height].every((n) => Number.isFinite(n) && n >= 0)
      || (region.width === 0) !== (region.height === 0))) return null;
  return { captureId, annotations: normalized };
}
