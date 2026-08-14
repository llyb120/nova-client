import type { ThemeColors } from "./base";

/**
 * 星图背景：按当前时间（格林尼治恒星时）旋转天球，绘制接近真实星空的效果。
 *
 * 真实感来自三个层次：
 * 1. 星光不是实心圆点——每颗星由「光晕 + 亮核 + 微弱十字芒」的离屏精灵合成，
 *    按星等以多尺度叠加（模拟大气弥散），并按温度上色（蓝白/白/暖黄）。
 * 2. 银河不是几条光带——沿银道面撒数百个大小不一、色温不同、明暗不一的星团，
 *    叠加出云气般的银河，银心方向（人马座）最亮最宽。
 * 3. 星座连线降到几乎不可见的暗线，只起结构暗示作用，避免"示意图"感。
 *
 * 绘制在会话背景缓存层（文字之下），投影为以当前子午线为中心的球面立体投影
 * （约 160° 广角视场），星空随真实时间自东向西漂移。
 */

const D2R = Math.PI / 180;

/* ===== 天体坐标数据 ===== */

interface Star {
  ra: number; // 赤经，小时
  dec: number; // 赤纬，度
  mag: number; // 视星等
}

interface Constellation {
  /** 中文名 */
  name: string;
  /** 名称标注锚点（放在星群附近的空白处） */
  label: { ra: number; dec: number };
  stars: Star[];
  lines: Array<[number, number]>;
}

/** J2000 近似坐标，只取构成星座形状的亮星 */
const CONSTELLATIONS: Constellation[] = [
  {
    // 北斗七星（大熊座）
    name: "北斗七星",
    label: { ra: 12.2, dec: 51.5 },
    stars: [
      { ra: 11.06, dec: 61.75, mag: 1.8 }, // 天枢 Dubhe
      { ra: 11.03, dec: 56.38, mag: 2.4 }, // 天璇 Merak
      { ra: 11.9, dec: 53.69, mag: 2.4 }, // 天玑 Phecda
      { ra: 12.26, dec: 57.03, mag: 3.3 }, // 天权 Megrez
      { ra: 12.9, dec: 55.96, mag: 1.8 }, // 玉衡 Alioth
      { ra: 13.42, dec: 54.93, mag: 2.2 }, // 开阳 Mizar
      { ra: 13.79, dec: 49.31, mag: 1.9 }, // 摇光 Alkaid
    ],
    lines: [
      [0, 1], [1, 2], [2, 3], [3, 0], [3, 4], [4, 5], [5, 6],
    ],
  },
  {
    // 仙后座（W 形）
    name: "仙后座",
    label: { ra: 1.05, dec: 64.5 },
    stars: [
      { ra: 0.15, dec: 59.15, mag: 2.3 }, // 王良一 Caph
      { ra: 0.68, dec: 56.54, mag: 2.2 }, // 王良四 Schedar
      { ra: 0.95, dec: 60.72, mag: 2.5 }, // 策 Gamma Cas
      { ra: 1.43, dec: 60.24, mag: 2.7 }, // 阁道三 Ruchbah
      { ra: 1.91, dec: 63.67, mag: 3.4 }, // 阁道二 Segin
    ],
    lines: [
      [0, 1], [1, 2], [2, 3], [3, 4],
    ],
  },
  {
    // 猎户座
    name: "猎户座",
    label: { ra: 5.55, dec: 3.6 },
    stars: [
      { ra: 5.92, dec: 7.41, mag: 0.4 }, // 参宿四 Betelgeuse（红超巨星）
      { ra: 5.42, dec: 6.35, mag: 1.6 }, // 参宿五 Bellatrix
      { ra: 5.68, dec: -1.94, mag: 1.7 }, // 参宿一 Alnitak
      { ra: 5.6, dec: -1.2, mag: 1.7 }, // 参宿二 Alnilam
      { ra: 5.53, dec: -0.3, mag: 2.2 }, // 参宿三 Mintaka
      { ra: 5.8, dec: -9.67, mag: 2.1 }, // 参宿六 Saiph
      { ra: 5.24, dec: -8.2, mag: 0.1 }, // 参宿七 Rigel
    ],
    lines: [
      [0, 1], [0, 2], [1, 4], [2, 3], [3, 4], [2, 5], [4, 6], [5, 6],
    ],
  },
  {
    // 天鹅座（北十字）
    name: "天鹅座",
    label: { ra: 20.25, dec: 47.6 },
    stars: [
      { ra: 20.69, dec: 45.28, mag: 1.3 }, // 天津四 Deneb
      { ra: 20.62, dec: 40.26, mag: 2.2 }, // 天津一 Sadr
      { ra: 19.75, dec: 45.13, mag: 2.9 }, // 天津二 δ Cyg
      { ra: 20.77, dec: 33.97, mag: 2.5 }, // 天津九 ε Cyg
      { ra: 19.51, dec: 27.96, mag: 3.1 }, // 辇道增七 Albireo
    ],
    lines: [
      [0, 1], [1, 2], [1, 3], [1, 4],
    ],
  },
  {
    // 天蝎座
    name: "天蝎座",
    label: { ra: 16.35, dec: -21.3 },
    stars: [
      { ra: 16.09, dec: -19.81, mag: 2.6 }, // 房宿三 β Sco
      { ra: 16.0, dec: -22.62, mag: 2.3 }, // 房宿四 Dschubba
      { ra: 15.98, dec: -26.11, mag: 2.9 }, // 房宿一 π Sco
      { ra: 16.35, dec: -25.59, mag: 2.9 }, // 心宿一 σ Sco
      { ra: 16.49, dec: -26.43, mag: 1.1 }, // 心宿二 Antares（红超巨星）
      { ra: 16.6, dec: -28.22, mag: 2.8 }, // 心宿三 τ Sco
      { ra: 16.84, dec: -34.29, mag: 2.3 }, // 尾宿二 ε Sco
      { ra: 17.56, dec: -37.1, mag: 1.6 }, // 尾宿八 Shaula
      { ra: 17.51, dec: -37.3, mag: 2.7 }, // 尾宿九 Lesath
    ],
    lines: [
      [0, 1], [1, 2], [1, 3], [3, 4], [4, 5], [5, 6], [6, 7], [7, 8],
    ],
  },
  {
    // 狮子座（镰刀 + 三角）
    name: "狮子座",
    label: { ra: 10.55, dec: 27.8 },
    stars: [
      { ra: 9.76, dec: 23.77, mag: 3.0 }, // 轩辕九 ε Leo
      { ra: 9.88, dec: 26.01, mag: 3.9 }, // 轩辕十 μ Leo
      { ra: 10.22, dec: 23.42, mag: 3.4 }, // 轩辕十一 ζ Leo
      { ra: 10.33, dec: 19.84, mag: 2.1 }, // 轩辕十二 Algieba
      { ra: 10.14, dec: 11.97, mag: 1.4 }, // 轩辕十四 Regulus
      { ra: 11.24, dec: 15.43, mag: 3.3 }, // 西次相 θ Leo
      { ra: 11.24, dec: 20.53, mag: 2.6 }, // 西上相 Zosma
      { ra: 11.82, dec: 14.57, mag: 2.1 }, // 五帝座一 Denebola
    ],
    lines: [
      [0, 1], [1, 2], [2, 3], [3, 4], [4, 5], [5, 6], [6, 7], [7, 5],
    ],
  },
  {
    // 天琴座
    name: "天琴座",
    label: { ra: 18.55, dec: 41.5 },
    stars: [
      { ra: 18.62, dec: 38.78, mag: 0.0 }, // 织女星 Vega
      { ra: 18.74, dec: 33.67, mag: 4.0 }, // 织女二 ε Lyr
      { ra: 18.75, dec: 37.61, mag: 4.3 }, // 织女一 ζ Lyr
      { ra: 18.83, dec: 33.36, mag: 3.5 }, // 渐台二 Sheliak
      { ra: 18.98, dec: 32.69, mag: 3.2 }, // 渐台三 Sulafat
    ],
    lines: [
      [0, 2], [2, 4], [4, 3], [3, 1], [1, 2],
    ],
  },
  {
    // 天鹰座
    name: "天鹰座",
    label: { ra: 19.9, dec: 12.8 },
    stars: [
      { ra: 19.85, dec: 8.87, mag: 0.8 }, // 牛郎星 Altair
      { ra: 19.77, dec: 10.61, mag: 2.7 }, // 河鼓三 Tarazed
      { ra: 19.92, dec: 6.41, mag: 3.7 }, // 河鼓一 Alshain
    ],
    lines: [
      [1, 0], [0, 2],
    ],
  },
  {
    // 南十字座
    name: "南十字座",
    label: { ra: 12.45, dec: -55 },
    stars: [
      { ra: 12.44, dec: -63.1, mag: 0.8 }, // 十字架二 α Cru
      { ra: 12.79, dec: -59.69, mag: 1.3 }, // 十字架三 β Cru
      { ra: 12.52, dec: -57.11, mag: 1.6 }, // 十字架一 γ Cru
      { ra: 12.25, dec: -58.75, mag: 2.8 }, // 十字架四 δ Cru
    ],
    lines: [
      [0, 2], [1, 3],
    ],
  },
];

const POLARIS: Star = { ra: 2.53, dec: 89.26, mag: 2.0 };

/* ===== 伪随机（固定种子，星野可复现） ===== */

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/* ===== 银河数据 ===== */

/**
 * 银道坐标（l, b 均为度）→ 赤道坐标（J2000）。
 * 用正交基向量实现（e1=银心方向、e3=北银极），已对着银心/天鹅座/船底座等已知点校验。
 */
function galacticToEquatorial(lDeg: number, bDeg: number): { ra: number; dec: number } {
  const alphaP = 192.859508 * D2R; // 北银极赤经
  const deltaP = 27.128336 * D2R; // 北银极赤纬
  // 银心方向的赤道矢量（RA 17h45.6m, Dec -28°56′）
  const raC = 17.76 * 15 * D2R;
  const decC = -28.94 * D2R;
  const e1: [number, number, number] = [
    Math.cos(decC) * Math.cos(raC),
    Math.cos(decC) * Math.sin(raC),
    Math.sin(decC),
  ];
  const e3: [number, number, number] = [
    Math.cos(deltaP) * Math.cos(alphaP),
    Math.cos(deltaP) * Math.sin(alphaP),
    Math.sin(deltaP),
  ];
  // e2 = e3 × e1（归一化）
  const cx = e3[1] * e1[2] - e3[2] * e1[1];
  const cy = e3[2] * e1[0] - e3[0] * e1[2];
  const cz = e3[0] * e1[1] - e3[1] * e1[0];
  const n = Math.hypot(cx, cy, cz);
  const e2: [number, number, number] = [cx / n, cy / n, cz / n];
  const l = lDeg * D2R;
  const b = bDeg * D2R;
  const v = [0, 1, 2].map(
    (i) => Math.cos(b) * Math.cos(l) * e1[i] + Math.cos(b) * Math.sin(l) * e2[i] + Math.sin(b) * e3[i],
  );
  const dec = Math.asin(Math.max(-1, Math.min(1, v[2]))) / D2R;
  let ra = Math.atan2(v[1], v[0]) / D2R / 15;
  ra = ((ra % 24) + 24) % 24;
  return { ra, dec };
}

/** 距银心的银经差（0-180 度） */
function dlFromCore(l: number): number {
  return Math.min(l, 360 - l);
}

interface MilkyClump {
  ra: number;
  dec: number;
  size: number; // 角半径，度
  b: number; // 亮度权重 0-1
  cool: number; // 0=中性偏暖 1=冷蓝
}

/**
 * 银河：沿银道面撒数百个星团。银纬用高斯分布（带宽随银经变化），
 * 银心高亮、天鹅座次峰，色温与亮度带抖动，叠出云气感。只生成一次。
 */
const MILKY_CLUMPS: MilkyClump[] = (() => {
  const rand = mulberry32(20250615);
  const clumps: MilkyClump[] = [];
  // 高斯采样（Box-Muller）
  const gauss = () => {
    const u = Math.max(rand(), 1e-9);
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * rand());
  };
  for (let i = 0; i < 520; i++) {
    const l = rand() * 360;
    const dl = dlFromCore(l);
    const dlCyg = Math.min(Math.abs(l - 80), 360 - Math.abs(l - 80));
    const bright =
      0.15 + 0.85 * Math.exp(-((dl / 62) ** 2)) + 0.28 * Math.exp(-((dlCyg / 32) ** 2));
    const width = 6.5 + 6.5 * Math.exp(-((dl / 50) ** 2)); // 银心鼓胀
    const b = gauss() * width;
    if (Math.abs(b) > width * 2.2) continue; // 远尾巴裁掉，省得画飞点
    const eq = galacticToEquatorial(l, b);
    clumps.push({
      ra: eq.ra,
      dec: eq.dec,
      size: 3 + rand() * rand() * 11, // 3°-14°，偏小概率大
      b: Math.min(1, bright * (0.55 + rand() * 0.75)),
      cool: Math.min(1, Math.max(0, 0.3 + gauss() * 0.35)),
    });
  }
  return clumps;
})();

/** 银河带里的星场增强：沿银道多撒暗星，让"银河由无数恒星组成"的感觉出来 */
const MILKY_STARS: Star[] = (() => {
  const rand = mulberry32(778899);
  const stars: Star[] = [];
  const gauss = () => {
    const u = Math.max(rand(), 1e-9);
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * rand());
  };
  for (let i = 0; i < 420; i++) {
    const l = rand() * 360;
    const width = 7 + 6 * Math.exp(-((dlFromCore(l) / 55) ** 2));
    const b = gauss() * width * 0.8;
    if (Math.abs(b) > width * 2) continue;
    const eq = galacticToEquatorial(l, b);
    stars.push({ ra: eq.ra, dec: eq.dec, mag: 3.4 + rand() * 2.4 });
  }
  return stars;
})();

/** 全天背景暗星 */
const FIELD_STARS: Star[] = (() => {
  const rand = mulberry32(20240521);
  const stars: Star[] = [];
  for (let i = 0; i < 360; i++) {
    stars.push({
      ra: rand() * 24,
      dec: rand() * 160 - 80,
      mag: 3.0 + rand() * 2.6,
    });
  }
  return stars;
})();

/* ===== 恒星时与投影 ===== */

function siderealDeg(now: number): number {
  const jd = now / 86400000 + 2440587.5;
  const d = jd - 2451545.0;
  const gmst = 280.46061837 + 360.98564736629 * d;
  return ((gmst % 360) + 360) % 360;
}

interface Projection {
  cx: number;
  cy: number;
  s: number;
  raC: number;
  sinC: number;
  cosC: number;
}

function makeProjection(width: number, height: number, now: number): Projection {
  const decC = 18 * D2R; // 中心赤纬固定在中北纬天空
  return {
    cx: width * 0.5,
    cy: height * 0.5,
    s: Math.min(width, height) * 0.3,
    raC: siderealDeg(now) * D2R,
    sinC: Math.sin(decC),
    cosC: Math.cos(decC),
  };
}

/** 球面立体投影（约 160° 广角） */
function project(p: Projection, star: { ra: number; dec: number }): { x: number; y: number } {
  const dec = star.dec * D2R;
  const dRa = star.ra * 15 * D2R - p.raC;
  const sinD = Math.sin(dec);
  const cosD = Math.cos(dec);
  const denom = 1 + p.sinC * sinD + p.cosC * cosD * Math.cos(dRa);
  const k = (2 * p.s) / Math.max(denom, 0.02);
  return {
    x: p.cx + k * cosD * Math.sin(dRa),
    y: p.cy - k * (p.cosC * sinD - p.sinC * cosD * Math.cos(dRa)),
  };
}

/* ===== 星光精灵 ===== */

type Rgb = [number, number, number];

/**
 * 生成一颗星的离屏精灵：多层径向渐变（光晕→中晕→亮核）+ 微弱十字衍射芒。
 * 比单圆点柔和得多，多尺度叠加后接近真实相机里的星点。
 */
function makeStarSprite(color: Rgb, spiky: boolean): HTMLCanvasElement {
  const size = 64;
  const half = size / 2;
  const c = document.createElement("canvas");
  c.width = size;
  c.height = size;
  const g = c.getContext("2d")!;
  const [r, gr, b] = color;
  const rgba = (a: number) => `rgba(${r},${gr},${b},${a.toFixed(3)})`;

  const blob = (radius: number, alpha: number, stops: Array<[number, number]>) => {
    const grad = g.createRadialGradient(half, half, 0, half, half, radius);
    for (const [pos, a] of stops) grad.addColorStop(pos, rgba(a * alpha));
    g.fillStyle = grad;
    g.beginPath();
    g.arc(half, half, radius, 0, Math.PI * 2);
    g.fill();
  };

  blob(half, 0.28, [[0, 1], [0.3, 0.45], [1, 0]]); // 外晕
  blob(half * 0.36, 0.85, [[0, 1], [0.5, 0.5], [1, 0]]); // 中晕
  blob(half * 0.11, 1, [[0, 1], [0.6, 0.85], [1, 0]]); // 亮核

  if (spiky) {
    // 十字衍射芒：沿水平/垂直的两条极细渐变线
    const spike = (angle: number) => {
      g.save();
      g.translate(half, half);
      g.rotate(angle);
      const grad = g.createLinearGradient(-half, 0, half, 0);
      grad.addColorStop(0, rgba(0));
      grad.addColorStop(0.5, rgba(0.5));
      grad.addColorStop(1, rgba(0));
      g.fillStyle = grad;
      g.fillRect(-half, -0.7, size, 1.4);
      g.restore();
    };
    spike(0);
    spike(Math.PI / 2);
  }
  return c;
}

/* ===== 主题适配 ===== */

function luminanceOf(color: string): number | null {
  const s = color.trim();
  let r: number, g: number, b: number;
  const hex = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(s);
  if (hex) {
    let h = hex[1];
    if (h.length === 3)
      h = h
        .split("")
        .map((c) => c + c)
        .join("");
    r = parseInt(h.slice(0, 2), 16);
    g = parseInt(h.slice(2, 4), 16);
    b = parseInt(h.slice(4, 6), 16);
  } else {
    const m = /^rgba?\(\s*(\d+)[,\s]+(\d+)[,\s]+(\d+)/i.exec(s);
    if (!m) return null;
    r = +m[1];
    g = +m[2];
    b = +m[3];
  }
  return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
}

/** 星光色温：随机分配蓝白/白/暖黄，真实星野的颜色分布 */
function starTint(rand: number): Rgb {
  if (rand < 0.3) return [198, 214, 255]; // 蓝白（B/A 型）
  if (rand < 0.62) return [240, 244, 255]; // 白（F/G 型）
  return [255, 228, 196]; // 暖黄（K/M 型）
}

/* ===== 主绘制 ===== */

export function paintStarMap(
  ctx: CanvasRenderingContext2D,
  width: number,
  height: number,
  theme: Pick<ThemeColors, "bg" | "glowAccent">,
  now: number,
): void {
  if (width < 80 || height < 80) return;
  const lum = luminanceOf(theme.bg);
  // 仅暗色主题绘制星图：浅色主题直接跳过，零开销也避免任何视觉噪声
  if (lum != null && lum >= 0.5) return;

  const proj = makeProjection(width, height, now);
  const margin = 48;
  const inView = (x: number, y: number) =>
    x >= -margin && x <= width + margin && y >= -margin && y <= height + margin;

  // ── 银河（最底层）：星团云气叠加 ────────────────────────────────────────
  {
    // 颜色混合：暖白 ↔ 冷蓝
    const warm: Rgb = [224, 216, 235];
    const cool: Rgb = [186, 206, 250];
    const mix = (t: number): Rgb => [
      Math.round(warm[0] + (cool[0] - warm[0]) * t),
      Math.round(warm[1] + (cool[1] - warm[1]) * t),
      Math.round(warm[2] + (cool[2] - warm[2]) * t),
    ];
    const baseA = 0.055;
    for (const c of MILKY_CLUMPS) {
      const { x, y } = project(proj, c);
      const r = proj.s * c.size * D2R * 1.5;
      if (!inView(x - r, y - r) && !inView(x + r, y + r)) continue;
      const [cr, cg, cb] = mix(c.cool);
      const a = baseA * c.b;
      const g = ctx.createRadialGradient(x, y, 0, x, y, r);
      g.addColorStop(0, `rgba(${cr},${cg},${cb},${a.toFixed(3)})`);
      g.addColorStop(0.45, `rgba(${cr},${cg},${cb},${(a * 0.55).toFixed(3)})`);
      g.addColorStop(1, `rgba(${cr},${cg},${cb},0)`);
      ctx.fillStyle = g;
      ctx.beginPath();
      ctx.arc(x, y, r, 0, Math.PI * 2);
      ctx.fill();
    }
  }

  // ── 星光精灵缓存（按颜色分）────────────────────────────────────────────
  // 亮星带衍射芒、暗星不带；两种各三套色温
  const tintRand = mulberry32(4451);
  const spriteCache = new Map<string, HTMLCanvasElement>();
  const spriteFor = (color: Rgb, spiky: boolean): HTMLCanvasElement => {
    const key = `${color.join(",")}|${spiky ? 1 : 0}`;
    let s = spriteCache.get(key);
    if (!s) {
      s = makeStarSprite(color, spiky);
      spriteCache.set(key, s);
    }
    return s;
  };

  const drawStar = (star: Star, boost = 0) => {
    const { x, y } = project(proj, star);
    if (!inView(x, y)) return;
    // 星等 → 亮度/尺寸：非线性，亮星差异拉开
    const t = Math.max(0, Math.min(1, (5.6 - star.mag) / 5.6));
    const size = 3 + 17 * t * t; // 3-20px 的精灵
    const alpha = Math.min(1, 0.22 + 0.78 * t * t + boost);
    const bright = star.mag <= 2.3;
    // 已知红超巨星固定暖色，其余按随机色温
    const isWarmGiant =
      Math.abs(star.ra - 5.92) < 0.05 || Math.abs(star.ra - 16.49) < 0.05; // 参宿四 / 心宿二
    const color: Rgb = isWarmGiant ? [255, 206, 160] : starTint(tintRand());
    const sprite = spriteFor(color, bright);
    ctx.globalAlpha = alpha;
    ctx.drawImage(sprite, x - size / 2, y - size / 2, size, size);
    ctx.globalAlpha = 1;
  };

  // 全天暗星 + 银河带星场增强
  for (const s of FIELD_STARS) drawStar(s, -0.04);
  for (const s of MILKY_STARS) drawStar(s);

  // ── 星座连线：极暗的结构暗示 ────────────────────────────────────────────
  ctx.strokeStyle = "rgba(200,212,240,0.09)";
  ctx.lineWidth = 1;
  for (const con of CONSTELLATIONS) {
    const pts = con.stars.map((s) => project(proj, s));
    ctx.beginPath();
    for (const [a, b] of con.lines) {
      const p = pts[a];
      const q = pts[b];
      if (!inView(p.x, p.y) && !inView(q.x, q.y)) continue;
      ctx.moveTo(p.x, p.y);
      ctx.lineTo(q.x, q.y);
    }
    ctx.stroke();
  }

  // ── 星座名称：低透明度小字，跟随投影位置 ──────────────────────────────
  {
    ctx.fillStyle = "rgba(198,210,238,0.30)";
    ctx.font =
      '500 10.5px "Inter Variable","Noto Sans SC Variable","Segoe UI","Microsoft YaHei UI","PingFang SC",system-ui,sans-serif';
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    const spaced = ctx as CanvasRenderingContext2D & { letterSpacing?: string };
    if (spaced.letterSpacing !== undefined) spaced.letterSpacing = "2px";
    for (const con of CONSTELLATIONS) {
      const { x, y } = project(proj, con.label);
      if (!inView(x, y)) continue;
      ctx.fillText(con.name, x, y);
    }
    if (spaced.letterSpacing !== undefined) spaced.letterSpacing = "0px";
  }

  // ── 星座主星（压在线之上）───────────────────────────────────────────────
  for (const con of CONSTELLATIONS) {
    for (const s of con.stars) drawStar(s, 0.1);
  }
  drawStar(POLARIS, 0.15);
}
