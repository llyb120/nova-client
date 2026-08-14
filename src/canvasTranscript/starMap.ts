import type { ThemeColors } from "./base";

/**
 * 星图背景：按当前时间（格林尼治恒星时）旋转天球，把常见星座投影到画布上。
 * 绘制在会话背景缓存层（文字之下），透明度很低，不影响正文阅读。
 * 未获取观察者经纬度时以 GMST 为准：星座相对画面的方位随真实时间每天旋转一圈。
 */

/** 恒星：赤经 ra（小时）、赤纬 dec（度）、视星等 mag */
interface Star {
  ra: number;
  dec: number;
  mag: number;
}

/** 星座：主星列表 + 连线（主星下标对） */
interface Constellation {
  stars: Star[];
  lines: Array<[number, number]>;
}

/** J2000 近似坐标，只取构成星座形状的亮星 */
const CONSTELLATIONS: Constellation[] = [
  {
    // 北斗七星（大熊座）
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
      [0, 1],
      [1, 2],
      [2, 3],
      [3, 0],
      [3, 4],
      [4, 5],
      [5, 6],
    ],
  },
  {
    // 仙后座（W 形）
    stars: [
      { ra: 0.15, dec: 59.15, mag: 2.3 }, // 王良一 Caph
      { ra: 0.68, dec: 56.54, mag: 2.2 }, // 王良四 Schedar
      { ra: 0.95, dec: 60.72, mag: 2.5 }, // 策 Gamma Cas
      { ra: 1.43, dec: 60.24, mag: 2.7 }, // 阁道三 Ruchbah
      { ra: 1.91, dec: 63.67, mag: 3.4 }, // 阁道二 Segin
    ],
    lines: [
      [0, 1],
      [1, 2],
      [2, 3],
      [3, 4],
    ],
  },
  {
    // 猎户座
    stars: [
      { ra: 5.92, dec: 7.41, mag: 0.4 }, // 参宿四 Betelgeuse
      { ra: 5.42, dec: 6.35, mag: 1.6 }, // 参宿五 Bellatrix
      { ra: 5.68, dec: -1.94, mag: 1.7 }, // 参宿一 Alnitak
      { ra: 5.6, dec: -1.2, mag: 1.7 }, // 参宿二 Alnilam
      { ra: 5.53, dec: -0.3, mag: 2.2 }, // 参宿三 Mintaka
      { ra: 5.8, dec: -9.67, mag: 2.1 }, // 参宿六 Saiph
      { ra: 5.24, dec: -8.2, mag: 0.1 }, // 参宿七 Rigel
    ],
    lines: [
      [0, 1],
      [0, 2],
      [1, 4],
      [2, 3],
      [3, 4],
      [2, 5],
      [4, 6],
      [5, 6],
    ],
  },
  {
    // 天鹅座（北十字）
    stars: [
      { ra: 20.69, dec: 45.28, mag: 1.3 }, // 天津四 Deneb
      { ra: 20.62, dec: 40.26, mag: 2.2 }, // 天津一 Sadr
      { ra: 19.75, dec: 45.13, mag: 2.9 }, // 天津二 δ Cyg
      { ra: 20.77, dec: 33.97, mag: 2.5 }, // 天津九 ε Cyg
      { ra: 19.51, dec: 27.96, mag: 3.1 }, // 辇道增七 Albireo
    ],
    lines: [
      [0, 1],
      [1, 2],
      [1, 3],
      [1, 4],
    ],
  },
  {
    // 天蝎座
    stars: [
      { ra: 16.09, dec: -19.81, mag: 2.6 }, // 房宿三 β Sco
      { ra: 16.0, dec: -22.62, mag: 2.3 }, // 房宿四 Dschubba
      { ra: 15.98, dec: -26.11, mag: 2.9 }, // 房宿一 π Sco
      { ra: 16.35, dec: -25.59, mag: 2.9 }, // 心宿一 σ Sco
      { ra: 16.49, dec: -26.43, mag: 1.1 }, // 心宿二 Antares
      { ra: 16.6, dec: -28.22, mag: 2.8 }, // 心宿三 τ Sco
      { ra: 16.84, dec: -34.29, mag: 2.3 }, // 尾宿二 ε Sco
      { ra: 17.56, dec: -37.1, mag: 1.6 }, // 尾宿八 Shaula
      { ra: 17.51, dec: -37.3, mag: 2.7 }, // 尾宿九 Lesath
    ],
    lines: [
      [0, 1],
      [1, 2],
      [1, 3],
      [3, 4],
      [4, 5],
      [5, 6],
      [6, 7],
      [7, 8],
    ],
  },
  {
    // 狮子座（镰刀 + 三角）
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
      [0, 1],
      [1, 2],
      [2, 3],
      [3, 4],
      [4, 5],
      [5, 6],
      [6, 7],
      [7, 5],
    ],
  },
  {
    // 天琴座
    stars: [
      { ra: 18.62, dec: 38.78, mag: 0.0 }, // 织女星 Vega
      { ra: 18.74, dec: 33.67, mag: 4.0 }, // 织女二 ε Lyr
      { ra: 18.75, dec: 37.61, mag: 4.3 }, // 织女一 ζ Lyr
      { ra: 18.83, dec: 33.36, mag: 3.5 }, // 渐台二 Sheliak
      { ra: 18.98, dec: 32.69, mag: 3.2 }, // 渐台三 Sulafat
    ],
    lines: [
      [0, 2],
      [2, 4],
      [4, 3],
      [3, 1],
      [1, 2],
    ],
  },
  {
    // 天鹰座
    stars: [
      { ra: 19.85, dec: 8.87, mag: 0.8 }, // 牛郎星 Altair
      { ra: 19.77, dec: 10.61, mag: 2.7 }, // 河鼓三 Tarazed
      { ra: 19.92, dec: 6.41, mag: 3.7 }, // 河鼓一 Alshain
    ],
    lines: [
      [1, 0],
      [0, 2],
    ],
  },
  {
    // 南十字座
    stars: [
      { ra: 12.44, dec: -63.1, mag: 0.8 }, // 十字架二 α Cru
      { ra: 12.79, dec: -59.69, mag: 1.3 }, // 十字架三 β Cru
      { ra: 12.52, dec: -57.11, mag: 1.6 }, // 十字架一 γ Cru
      { ra: 12.25, dec: -58.75, mag: 2.8 }, // 十字架四 δ Cru
    ],
    lines: [
      [0, 2],
      [1, 3],
    ],
  },
];

/** 北极星（小熊座 α），作为天极附近的标志星 */
const POLARIS: Star = { ra: 2.53, dec: 89.26, mag: 2.0 };

/** 背景暗星：固定种子的伪随机星野，只生成一次 */
interface FieldStar {
  ra: number;
  dec: number;
  mag: number;
}

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

const FIELD_STARS: FieldStar[] = (() => {
  const rand = mulberry32(20240521);
  const stars: FieldStar[] = [];
  for (let i = 0; i < 150; i++) {
    stars.push({
      ra: rand() * 24,
      // 赤纬集中在 ±75° 内，避免全部堆到天极
      dec: rand() * 150 - 75,
      mag: 3.4 + rand() * 1.6,
    });
  }
  return stars;
})();

/**
 * 格林尼治平恒星时（角度，0-360）。
 * 地球自转使星空相对观察者每天转一圈，这给出了"当前时刻星座应在的位置"。
 */
function siderealDeg(now: number): number {
  const jd = now / 86400000 + 2440587.5; // Unix ms → 儒略日
  const d = jd - 2451545.0; // 距 J2000.0 的天数
  const gmst = 280.46061837 + 360.98564736629 * d;
  return ((gmst % 360) + 360) % 360;
}

/** 解析 #rgb / #rrggbb / rgb(a)，失败返回 null */
function luminanceOf(color: string): number | null {
  const s = color.trim();
  let r: number, g: number, b: number;
  const hex = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(s);
  if (hex) {
    let h = hex[1];
    if (h.length === 3) h = h.split("").map((c) => c + c).join("");
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

/**
 * 在背景层绘制星图。以天北极为中心的平面星图（planisphere）投影：
 * 赤纬决定到画面中心的半径，时角（GMST − 赤经）决定方位角。
 */
export function paintStarMap(
  ctx: CanvasRenderingContext2D,
  width: number,
  height: number,
  theme: Pick<ThemeColors, "bg" | "glowAccent">,
  now: number,
): void {
  if (width < 80 || height < 80) return;
  const lum = luminanceOf(theme.bg);
  const dark = lum == null || lum < 0.5;
  // 深色主题用微亮的冷白星点；浅色主题用暗蓝灰，透明度都压得很低，避免干扰文字。
  const starRgb = dark ? "226,233,255" : "52,64,110";
  const lineColor = dark ? theme.glowAccent : `rgba(${starRgb},0.13)`;

  const gmst = siderealDeg(now);
  const cx = width * 0.5;
  const cy = height * 0.46;
  const scale = Math.min(width, height) * 0.46;
  const margin = 24;

  const project = (star: Star): { x: number; y: number } => {
    // 时角：星空随恒星时绕天极旋转；负号让星星随时间向西（顺时针）移动，符合北半球仰头看天的方向
    const ha = ((gmst - star.ra * 15) * Math.PI) / 180;
    const r = ((90 - star.dec) / 90) * scale;
    return { x: cx + r * Math.sin(ha), y: cy - r * Math.cos(ha) };
  };

  const inView = (x: number, y: number) =>
    x >= -margin && x <= width + margin && y >= -margin && y <= height + margin;

  const starRadius = (mag: number) => Math.max(0.6, 2.3 - mag * 0.55);
  const starAlpha = (mag: number) => Math.min(0.42, Math.max(0.1, 0.34 - mag * 0.055));

  // 背景暗星（最弱一层）
  for (const s of FIELD_STARS) {
    const { x, y } = project(s);
    if (!inView(x, y)) continue;
    ctx.globalAlpha = starAlpha(s.mag) * 0.55;
    ctx.fillStyle = `rgb(${starRgb})`;
    ctx.fillRect(x, y, 1, 1);
  }

  // 星座连线（先画线，星点压在线上）
  ctx.strokeStyle = lineColor;
  ctx.lineWidth = 1;
  for (const con of CONSTELLATIONS) {
    const pts = con.stars.map(project);
    ctx.globalAlpha = 1;
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

  // 星座主星
  ctx.fillStyle = `rgb(${starRgb})`;
  for (const con of CONSTELLATIONS) {
    for (const s of con.stars) {
      const { x, y } = project(s);
      if (!inView(x, y)) continue;
      ctx.globalAlpha = starAlpha(s.mag);
      ctx.beginPath();
      ctx.arc(x, y, starRadius(s.mag), 0, Math.PI * 2);
      ctx.fill();
    }
  }

  // 北极星：天极附近的锚点，稍亮并带一小圈光晕
  {
    const { x, y } = project(POLARIS);
    if (inView(x, y)) {
      ctx.globalAlpha = 0.1;
      ctx.beginPath();
      ctx.arc(x, y, 5, 0, Math.PI * 2);
      ctx.fill();
      ctx.globalAlpha = starAlpha(POLARIS.mag) + 0.08;
      ctx.beginPath();
      ctx.arc(x, y, starRadius(POLARIS.mag) + 0.4, 0, Math.PI * 2);
      ctx.fill();
    }
  }

  ctx.globalAlpha = 1;
}
