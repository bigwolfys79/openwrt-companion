// Generates icons/icon.ico (128x128, 32bpp BMP) and icons/32x32.png / 128x128.png
// without external deps: rounded-square gradient + white router glyph.
const fs = require("fs");
const path = require("path");
const zlib = require("zlib");

function clamp(v, a, b) { return Math.max(a, Math.min(b, v)); }

function lerp(a, b, t) { return a + (b - a) * t; }

function hex(h) { return [parseInt(h.slice(1, 3), 16), parseInt(h.slice(3, 5), 16), parseInt(h.slice(5, 7), 16)]; }

function roundedRectSdf(px, py, cx, cy, hw, hh, r) {
  const dx = Math.abs(px - cx) - (hw - r);
  const dy = Math.abs(py - cy) - (hh - r);
  const ax = Math.max(dx, 0), ay = Math.max(dy, 0);
  return Math.hypot(ax, ay) + Math.min(Math.max(dx, dy), 0) - r;
}

function circleSdf(px, py, cx, cy, r) { return Math.hypot(px - cx, py - cy) - r; }

function capsuleSdf(px, py, ax, ay, bx, by, r) {
  const pax = px - ax, pay = py - ay, bax = bx - ax, bay = by - ay;
  const h = clamp((pax * bax + pay * bay) / (bax * bax + bay * bay), 0, 1);
  return Math.hypot(pax - bax * h, pay - bay * h) - r;
}

function render(size) {
  const buf = Buffer.alloc(size * size * 4);
  const C1 = hex("#4f46e5"), C2 = hex("#8b5cf6"), C3 = hex("#06b6d4");
  const S = size / 128;
  const R = 28 * S;
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const u = x / size, v = y / size;
      const t = clamp((u + v) / 2, 0, 1);
      let r = t < 0.5 ? lerp(C1[0], C2[0], t * 2) : lerp(C2[0], C3[0], (t - 0.5) * 2);
      let g = t < 0.5 ? lerp(C1[1], C2[1], t * 2) : lerp(C2[1], C3[1], (t - 0.5) * 2);
      let b = t < 0.5 ? lerp(C1[2], C2[2], t * 2) : lerp(C2[2], C3[2], (t - 0.5) * 2);
      const bg = clamp(0.5 - roundedRectSdf(x + 0.5, y + 0.5, size / 2, size / 2, size / 2, size / 2, R), 0, 1);
      // white parts: body + antennas + dots
      let shape = 1e9;
      shape = Math.min(shape, roundedRectSdf(x + 0.5, y + 0.5, 64 * S, 124 * S, 36 * S, 17 * S, 9 * S)); // body
      shape = Math.min(shape, capsuleSdf(x + 0.5, y + 0.5, 46 * S, 84 * S, 46 * S, 108 * S, 3.5 * S)); // antenna 1
      shape = Math.min(shape, capsuleSdf(x + 0.5, y + 0.5, 82 * S, 84 * S, 82 * S, 108 * S, 3.5 * S)); // antenna 2
      const dotG = circleSdf(x + 0.5, y + 0.5, 44 * S, 124 * S, 5.5 * S);
      shape = Math.min(shape, dotG);
      shape = Math.min(shape, circleSdf(x + 0.5, y + 0.5, 64 * S, 124 * S, 5.5 * S));
      shape = Math.min(shape, circleSdf(x + 0.5, y + 0.5, 84 * S, 124 * S, 5.5 * S));
      const cov = clamp(0.5 - shape, 0, 1);
      // green accent on the left dot
      if (cov > 0 && dotG < 0) { r = 52; g = 211; b = 153; }
      else if (cov > 0) { r = 255; g = 255; b = 255; }
      const a = Math.max(bg, cov);
      const i = (y * size + x) * 4;
      buf[i] = Math.round(r); buf[i + 1] = Math.round(g); buf[i + 2] = Math.round(b); buf[i + 3] = Math.round(a * 255);
    }
  }
  return buf;
}

function writeIco(size, file) {
  const px = render(size);
  const w = size, h = size;
  const xor = w * h * 4;
  const andRow = Math.ceil(w / 32) * 4;
  const and = andRow * h;
  const dataSize = 40 + xor + and;
  const ico = Buffer.alloc(6 + 16 + dataSize);
  ico.writeUInt16LE(0, 0); ico.writeUInt16LE(1, 2); ico.writeUInt16LE(1, 4);
  ico[6] = w % 256; ico[7] = h % 256; ico[8] = 0; ico[9] = 0;
  ico.writeUInt16LE(1, 10); ico.writeUInt16LE(32, 12);
  ico.writeUInt32LE(dataSize, 14); ico.writeUInt32LE(22, 18);
  const dib = 22;
  ico.writeUInt32LE(40, dib); ico.writeInt32LE(w, dib + 4); ico.writeInt32LE(h * 2, dib + 8);
  ico.writeUInt16LE(1, dib + 12); ico.writeUInt16LE(32, dib + 14);
  ico.writeUInt32LE(0, dib + 16); ico.writeUInt32LE(xor + and, dib + 20);
  // bottom-up BGRA
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const src = ((h - 1 - y) * w + x) * 4;
      const dst = dib + 40 + (y * w + x) * 4;
      ico[dst] = px[src + 2]; ico[dst + 1] = px[src + 1]; ico[dst + 2] = px[src]; ico[dst + 3] = px[src + 3];
    }
  }
  fs.writeFileSync(file, ico);
}

// minimal PNG writer (RGB + alpha via color type 6)
function crc32(buf) {
  let c, table = [];
  for (let n = 0; n < 256; n++) { c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; table[n] = c >>> 0; }
  let crc = 0xffffffff;
  for (const b of buf) crc = table[(crc ^ b) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4); len.writeUInt32LE(data.length, 0); len.swap32();
  const td = Buffer.concat([Buffer.from(type), data]);
  const crc = Buffer.alloc(4); crc.writeUInt32LE(crc32(td), 0); crc.swap32();
  return Buffer.concat([len, td, crc]);
}
function writePng(size, file) {
  const px = render(size);
  const raw = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0;
    px.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0); ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; ihdr[9] = 6; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;
  const png = Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", zlib.deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
  fs.writeFileSync(file, png);
}

const out = path.join(__dirname, "..", "src-tauri", "icons");
fs.mkdirSync(out, { recursive: true });
writeIco(128, path.join(out, "icon.ico"));
writePng(128, path.join(out, "128x128.png"));
writePng(32, path.join(out, "32x32.png"));
console.log("icons written to", out);
