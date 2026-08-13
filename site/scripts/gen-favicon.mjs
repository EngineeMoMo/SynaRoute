/**
 * 生成站点图标：favicon.ico(16+32) / favicon-32.png / logo-256.png。
 *
 * 为什么自己写编码器：仓库里没有 sharp / canvas 这类原生依赖，为了几个图标去装
 * 一个要编译的包不值当。PNG 的最小可用子集（IHDR + 单个 IDAT + IEND）用 node:zlib
 * 就能出，ICO 外层也只是一段定长目录 + 内嵌 PNG。
 *
 * ## 图形来源：lucide-react 的 `Waypoints`（2026-08-12 统一）
 *
 * 应用侧栏用的就是它（`src/components/Sidebar.tsx` 的 `<Waypoints size={18}/>`
 * 套在 `bg-primary rounded-control` 方块里）。此前本脚本画的是另一套图形
 * （一条对角线串三个节点），于是「侧栏 logo」与「托盘/任务栏/favicon」长期是
 * 两个不同的标志 —— 而这种不一致没有任何报错，只能靠肉眼比对发现。
 *
 * 现在坐标直接抄 lucide 的定义（见下方 `LUCIDE_WAYPOINTS`），
 * `site/src/components/ui/Logo.tsx` 的内联 SVG 与之同源。**改图形要两处一起改。**
 *
 * 跑法：node scripts/gen-favicon.mjs
 */
import zlib from "node:zlib";
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const PUB = join(dirname(fileURLToPath(import.meta.url)), "..", "public");

const PRIMARY = [109, 94, 247];
const FG = [255, 255, 255];
const VB = 32; // 输出 viewBox 边长
const RX = 7.5; // 圆角半径 —— 就是这一条让标签页里的图标不再是直角方块
const SS = 4; // 超采样倍数，抗锯齿用

/**
 * lucide `Waypoints` 的原始几何（其 viewBox 是 24×24）。
 *
 * 逐字抄自 `node_modules/lucide-react/dist/esm/icons/waypoints.js`，
 * 升级 lucide 后若该图标改版，这里要重新对一遍。
 *
 * 注意连线端点**刻意不落在圆心上**（如 10.2,6.3 → 6.3,10.2，而两端圆心是
 * 12,4.5 与 4.5,12）：那段留白是原设计的一部分，线不穿进圆里。照抄坐标即可保留。
 */
const LUCIDE_VB = 24;
const LUCIDE_STROKE = 2; // lucide 默认线宽
const LUCIDE_WAYPOINTS = {
  circles: [
    [12, 4.5, 2.5], // 上
    [4.5, 12, 2.5], // 左
    [19.5, 12, 2.5], // 右
    [12, 19.5, 2.5], // 下
  ],
  segments: [
    [10.2, 6.3, 6.3, 10.2], // 上 → 左（斜）
    [7, 12, 17, 12], // 左 → 右（水平中线）
    [13.8, 17.7, 17.7, 13.8], // 下 → 右（斜）
  ],
};

/**
 * 图形在方块里的占比。
 *
 * 侧栏是 32px 方块套 18px 图标（56%）。独立应用图标取 62.5% 略大一点：
 * 托盘只有 16px，56% 时图形实际不到 9px，细节会糊成一团；放大到 20px
 * 仍留 6px 边距，视觉上不顶边。
 */
const GLYPH = 20;
const SCALE = GLYPH / LUCIDE_VB;
const OFFSET = (VB - GLYPH) / 2;
const tx = (v) => OFFSET + v * SCALE;
/** lucide 线宽换算到输出坐标系。 */
const STROKE = LUCIDE_STROKE * SCALE;

// ---------- PNG ----------
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

/** RGBA（色彩类型 6）编码。圆角外那圈像素 alpha=0，浏览器才会透出标签页底色。 */
function encodePNG(size, rgba) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // RGBA
  const stride = size * 4;
  const raw = Buffer.alloc(size * (stride + 1));
  for (let y = 0; y < size; y++) {
    raw[y * (stride + 1)] = 0; // filter: none
    rgba.copy(raw, y * (stride + 1) + 1, y * stride, (y + 1) * stride);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", zlib.deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

// ---------- 形状（一律在 viewBox 坐标系里判定，与 SVG 一一对应）----------
const inRounded = (x, y) => {
  if (x < 0 || y < 0 || x > VB || y > VB) return false;
  const cx = Math.min(Math.max(x, RX), VB - RX);
  const cy = Math.min(Math.max(y, RX), VB - RX);
  return (x - cx) ** 2 + (y - cy) ** 2 <= RX * RX;
};
const inCircle = (cx, cy, r) => (x, y) => (x - cx) ** 2 + (y - cy) ** 2 <= r * r;
const onSeg = (x1, y1, x2, y2, w) => (x, y) => {
  const dx = x2 - x1;
  const dy = y2 - y1;
  const t = Math.max(0, Math.min(1, ((x - x1) * dx + (y - y1) * dy) / (dx * dx + dy * dy)));
  return (x - (x1 + t * dx)) ** 2 + (y - (y1 + t * dy)) ** 2 <= (w / 2) ** 2;
};

/** 把 `inside` 判定的形状以 `color` 叠加到 buf 上（source-over，带 alpha 合成）。 */
function paint(size, buf, inside, color) {
  const k = size / VB;
  for (let py = 0; py < size; py++) {
    for (let px = 0; px < size; px++) {
      let hit = 0;
      for (let sy = 0; sy < SS; sy++) {
        for (let sx = 0; sx < SS; sx++) {
          if (inside((px + (sx + 0.5) / SS) / k, (py + (sy + 0.5) / SS) / k)) hit++;
        }
      }
      if (!hit) continue;
      const a = hit / (SS * SS);
      const o = (py * size + px) * 4;
      const dstA = buf[o + 3] / 255;
      const outA = a + dstA * (1 - a);
      for (let c = 0; c < 3; c++) {
        const src = color[c] / 255;
        const dst = buf[o + c] / 255;
        buf[o + c] = Math.round(((src * a + dst * dstA * (1 - a)) / outA) * 255);
      }
      buf[o + 3] = Math.round(outA * 255);
    }
  }
}

function render(size) {
  const buf = Buffer.alloc(size * size * 4); // 全透明起步
  paint(size, buf, inRounded, PRIMARY);

  // 小尺寸补偿：16px 下换算后的线宽只有 ~1.4 像素，抗锯齿会把它糊成灰边。
  // 乘 1.35 让它在托盘里仍看得出是一条线（大尺寸不动，免得显得笨重）。
  const k = size <= 16 ? 1.35 : 1;
  const stroke = STROKE * k;

  // 先画线后画圆：圆要压在线端之上，盖掉端点的圆头（lucide 的线本就不进圆心）
  for (const [x1, y1, x2, y2] of LUCIDE_WAYPOINTS.segments) {
    paint(size, buf, onSeg(tx(x1), tx(y1), tx(x2), tx(y2), stroke), FG);
  }
  for (const [cx, cy, r] of LUCIDE_WAYPOINTS.circles) {
    paint(size, buf, inCircle(tx(cx), tx(cy), r * SCALE * k), FG);
  }
  return { png: encodePNG(size, buf), buf };
}

// ---------- ICO（内嵌 PNG，现代浏览器都认）----------
function ico(entries) {
  const dir = Buffer.alloc(6 + entries.length * 16);
  dir.writeUInt16LE(0, 0);
  dir.writeUInt16LE(1, 2); // type: icon
  dir.writeUInt16LE(entries.length, 4);
  let offset = dir.length;
  entries.forEach(({ size, png }, i) => {
    const e = 6 + i * 16;
    dir[e] = size >= 256 ? 0 : size; // 0 表示 256
    dir[e + 1] = size >= 256 ? 0 : size;
    dir.writeUInt16LE(1, e + 4); // planes
    dir.writeUInt16LE(32, e + 6); // bpp
    dir.writeUInt32LE(png.length, e + 8);
    dir.writeUInt32LE(offset, e + 12);
    offset += png.length;
  });
  return Buffer.concat([dir, ...entries.map((x) => x.png)]);
}

const r16 = render(16);
const r32 = render(32);
const r256 = render(256);

/**
 * 判据：四角像素必须完全透明。
 * 圆角没生效时这四个点是不透明的主色 —— 那正是「标签页里看着还是直角方块」的样子。
 * 生成即校验，不通过就退出，避免又发一版直角图标上去。
 */
function assertRoundedCorners(size, buf) {
  const at = (x, y) => buf[(y * size + x) * 4 + 3];
  const corners = [at(0, 0), at(size - 1, 0), at(0, size - 1), at(size - 1, size - 1)];
  if (corners.some((a) => a !== 0)) {
    console.error(`[favicon] ✗ ${size}px 四角 alpha = ${corners.join(",")}，圆角未生效`);
    process.exit(1);
  }
  // 中心必须是实心的，否则整张图是空的也能「四角透明」通过上面那条
  if (at(size >> 1, size >> 1) !== 255) {
    console.error(`[favicon] ✗ ${size}px 中心 alpha = ${at(size >> 1, size >> 1)}，图形没画上`);
    process.exit(1);
  }
  console.log(`[favicon] ✓ ${size}px 四角 alpha=0、中心 alpha=255`);
}

/**
 * 判据：白色图形必须真的画上去了。
 *
 * 上面那条「中心 alpha=255」只证明**底色**在 —— 圆角矩形铺满时它恒成立。
 * 若图形坐标算错（比如换图形时变换写反、整个 glyph 落到了画布外），
 * 产出会是一个纯紫方块，而那条断言照样通过。这正是本次换图形最可能踩的坑，
 * 故按「白色像素占比」单独判一次。
 *
 * 阈值 3%：Waypoints 是细线 + 四个小圆，实测 32px 下白色占比约 12%；
 * 取 3% 作下限，既能兜住「一个白点都没有」，也不会因线宽微调而误报。
 */
function assertGlyphPainted(size, buf) {
  let white = 0;
  for (let i = 0; i < size * size; i++) {
    const o = i * 4;
    // 判「接近白」而非严格 255：抗锯齿边缘是主色与白的混合
    if (buf[o] > 200 && buf[o + 1] > 200 && buf[o + 2] > 200 && buf[o + 3] > 200) white++;
  }
  const ratio = white / (size * size);
  if (ratio < 0.03) {
    console.error(
      `[favicon] ✗ ${size}px 白色像素仅 ${(ratio * 100).toFixed(1)}%，图形没画上（只有底色）`,
    );
    process.exit(1);
  }
  console.log(`[favicon] ✓ ${size}px 图形已绘制（白色占比 ${(ratio * 100).toFixed(1)}%）`);
}

assertRoundedCorners(16, r16.buf);
assertRoundedCorners(32, r32.buf);
assertRoundedCorners(256, r256.buf);
assertGlyphPainted(16, r16.buf);
assertGlyphPainted(32, r32.buf);
assertGlyphPainted(256, r256.buf);

const icoBuf = ico([
  { size: 16, png: r16.png },
  { size: 32, png: r32.png },
]);
writeFileSync(join(PUB, "favicon.ico"), icoBuf);
writeFileSync(join(PUB, "favicon-32.png"), r32.png);
writeFileSync(join(PUB, "logo-256.png"), r256.png);

console.log(
  `[favicon] favicon.ico ${icoBuf.length}B · favicon-32.png ${r32.png.length}B · logo-256.png ${r256.png.length}B（圆角 ${RX}/${VB}）`
);
