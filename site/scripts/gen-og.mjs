// 生成社交分享图 public/og.png（1200×630）。
//
// 为什么手写 PNG 编码而不用 sharp / canvas：官网只为这一张图引入一个需要预编译
// 二进制的原生依赖不划算，而且那类依赖在不同平台上装不上就会卡住整个构建。
// 这里只用 Node 自带的 zlib，任何环境都能跑。
//
// 图形内容：中间是与应用图标同形的品牌标记，四周若干节点由细线连向中心 ——
// 表达「一个本地中枢把请求路由到多个上游」。刻意不画任何界面截图或伪造 UI。
//
// 用法：node scripts/gen-og.mjs（改了品牌色或构图后重跑）
import zlib from "node:zlib";
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const OUT = join(dirname(fileURLToPath(import.meta.url)), "..", "public", "og.png");
const W = 1200;
const H = 630;

// —— 品牌色，与 site/src/styles.css 的 token 保持一致 ——
const BG_DARK = [14, 14, 17];
const BG_TINT = [26, 24, 44];
const PRIMARY = [109, 94, 247];
const WHITE = [255, 255, 255];

// ---------- 最小 PNG 编码器 ----------
const crcTable = (() => {
  const t = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c;
  }
  return t;
})();

function crc32(b) {
  let c = -1;
  for (let i = 0; i < b.length; i++) c = crcTable[(c ^ b[i]) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}

function encodePNG(w, h, rgb) {
  const stride = w * 3 + 1;
  const raw = Buffer.alloc(stride * h);
  for (let y = 0; y < h; y++) {
    raw[y * stride] = 0; // 每行的滤波器类型：none
    rgb.copy(raw, y * stride + 1, y * w * 3, (y + 1) * w * 3);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; // 位深
  ihdr[9] = 2; // 颜色类型：真彩色 RGB
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", zlib.deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

// ---------- 绘图 ----------
const buf = Buffer.alloc(W * H * 3);
const lerp = (a, b, t) => a + (b - a) * t;
const mix = (c1, c2, t) => [lerp(c1[0], c2[0], t), lerp(c1[1], c2[1], t), lerp(c1[2], c2[2], t)];

// 背景：对角渐变 + 中心紫色光晕（平方衰减，避免出现生硬的圆边）
for (let y = 0; y < H; y++) {
  for (let x = 0; x < W; x++) {
    let c = mix(BG_TINT, BG_DARK, Math.min(1, ((x / W + y / H) / 2) * 1.35));
    const dx = (x - W / 2) / 520;
    const dy = (y - H / 2) / 380;
    const glow = Math.max(0, 1 - (dx * dx + dy * dy));
    c = mix(c, PRIMARY, glow * glow * 0.22);
    const i = (y * W + x) * 3;
    buf[i] = c[0] | 0;
    buf[i + 1] = c[1] | 0;
    buf[i + 2] = c[2] | 0;
  }
}

// 3×3 超采样抗锯齿：直接判定像素中心会让圆和斜线出现明显锯齿
const SS = 3;
function paint(inside, color, alpha = 1) {
  for (let y = 0; y < H; y++) {
    for (let x = 0; x < W; x++) {
      let hit = 0;
      for (let sy = 0; sy < SS; sy++) {
        for (let sx = 0; sx < SS; sx++) {
          if (inside(x + (sx + 0.5) / SS, y + (sy + 0.5) / SS)) hit++;
        }
      }
      if (!hit) continue;
      const a = (hit / (SS * SS)) * alpha;
      const i = (y * W + x) * 3;
      buf[i] = lerp(buf[i], color[0], a) | 0;
      buf[i + 1] = lerp(buf[i + 1], color[1], a) | 0;
      buf[i + 2] = lerp(buf[i + 2], color[2], a) | 0;
    }
  }
}

const roundedRect = (cx, cy, w, h, r) => (x, y) => {
  const dx = Math.abs(x - cx) - (w / 2 - r);
  const dy = Math.abs(y - cy) - (h / 2 - r);
  if (dx <= 0 && dy <= 0) return true;
  const qx = Math.max(dx, 0);
  const qy = Math.max(dy, 0);
  return qx * qx + qy * qy <= r * r;
};
const circle = (cx, cy, r) => (x, y) => (x - cx) ** 2 + (y - cy) ** 2 <= r * r;
const segment = (x1, y1, x2, y2, w) => (x, y) => {
  const vx = x2 - x1;
  const vy = y2 - y1;
  let t = ((x - x1) * vx + (y - y1) * vy) / (vx * vx + vy * vy);
  t = Math.max(0, Math.min(1, t));
  return (x - (x1 + t * vx)) ** 2 + (y - (y1 + t * vy)) ** 2 <= (w / 2) ** 2;
};

const CX = 600;
const CY = 300;
const S = 224; // 品牌标记边长
const k = S / 32; // 标记输出 viewBox 是 32，按此缩放
const toCanvas = (vx, vy) => [CX - S / 2 + vx * k, CY - S / 2 + vy * k];

/**
 * 品牌标记的内层几何：lucide `Waypoints`（24×24 坐标系），缩到 20 居中（偏移 6）。
 *
 * 与 `gen-favicon.mjs` 及 `src/components/ui/Logo.tsx` **同一套变换**。
 * 三处必须一起改 —— 图形分叉不会报错，只能靠肉眼比对发现。
 */
const GLYPH = 20;
const LUCIDE_VB = 24;
const gs = GLYPH / LUCIDE_VB; // lucide → 输出 viewBox 的缩放
const go = (32 - GLYPH) / 2; // 居中偏移
/** lucide 坐标 → 画布坐标 */
const toG = (lx, ly) => toCanvas(go + lx * gs, go + ly * gs);
const G_STROKE = 2 * gs * k; // lucide 默认线宽 2，两级缩放

// 外围分流节点先画（位于标记下层），低透明度做背景层次
for (const [sx, sy, r] of [
  [205, 150, 17],
  [995, 132, 14],
  [150, 470, 13],
  [1050, 452, 18],
  [345, 545, 11],
  [860, 560, 12],
]) {
  paint(segment(CX, CY, sx, sy, 3), PRIMARY, 0.3);
  paint(circle(sx, sy, r), WHITE, 0.55);
  paint(circle(sx, sy, r + 9), PRIMARY, 0.14);
}

// 品牌标记本体：与 src-tauri/icons 的应用图标同形（lucide Waypoints）
paint(roundedRect(CX, CY, S, S, (S * 7.5) / 32), PRIMARY, 1);
// 连线（端点刻意不落在圆心上，那段留白是原设计的一部分）
for (const [x1, y1, x2, y2] of [
  [10.2, 6.3, 6.3, 10.2],
  [7, 12, 17, 12],
  [13.8, 17.7, 17.7, 13.8],
]) {
  const [px1, py1] = toG(x1, y1);
  const [px2, py2] = toG(x2, y2);
  paint(segment(px1, py1, px2, py2, G_STROKE), WHITE, 1);
}
// 四个节点：上、左、右、下
for (const [lcx, lcy, r] of [
  [12, 4.5, 2.5],
  [4.5, 12, 2.5],
  [19.5, 12, 2.5],
  [12, 19.5, 2.5],
]) {
  const [pcx, pcy] = toG(lcx, lcy);
  paint(circle(pcx, pcy, r * gs * k), WHITE, 1);
}

const png = encodePNG(W, H, buf);
writeFileSync(OUT, png);
console.log(`[gen-og] 已生成 ${OUT}（${W}×${H}, ${(png.length / 1024).toFixed(1)} KB）`);
