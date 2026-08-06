/**
 * 生成站点图标：favicon.ico(16+32) / favicon-32.png / logo-256.png。
 *
 * 为什么自己写编码器：仓库里没有 sharp / canvas 这类原生依赖，为了几个图标去装
 * 一个要编译的包不值当。PNG 的最小可用子集（IHDR + 单个 IDAT + IEND）用 node:zlib
 * 就能出，ICO 外层也只是一段定长目录 + 内嵌 PNG。
 *
 * 图形与 site/src/components/ui/Logo.tsx 的 LogoMark **同源**：viewBox 32、圆角 7.5、
 * 一条对角线串三个节点。改图形要两边一起改，否则标签页图标和站内 logo 会不一致。
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
const VB = 32; // 原始 viewBox 边长
const RX = 7.5; // 圆角半径 —— 就是这一条让标签页里的图标不再是直角方块
const SS = 4; // 超采样倍数，抗锯齿用

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
  // 16px 下 2.4 的线宽缩到不足 1.2 像素会糊成灰边，小尺寸适当加粗
  const stroke = size <= 16 ? 3.2 : 2.4;
  paint(size, buf, onSeg(9, 8.5, 16, 16, stroke), FG);
  paint(size, buf, onSeg(16, 16, 23, 23.5, stroke), FG);
  paint(size, buf, inCircle(9, 8.5, 2.9), FG);
  paint(size, buf, inCircle(16, 16, 4.4), FG);
  paint(size, buf, inCircle(23, 23.5, 2.9), FG);
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

assertRoundedCorners(16, r16.buf);
assertRoundedCorners(32, r32.buf);
assertRoundedCorners(256, r256.buf);

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
