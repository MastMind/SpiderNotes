import { deflateSync } from "node:zlib";
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

function crc32Table() {
  const t = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c;
  }
  return t;
}
const TABLE = crc32Table();
function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const t = Buffer.from(type, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([t, data])));
  return Buffer.concat([len, t, data, crc]);
}

function draw(size) {
  const px = Buffer.alloc(size * size * 4, 0);
  const s = size / 32;
  const set = (x, y, r, g, b, a = 255) => {
    if (a === 0 || x < 0 || y < 0 || x >= size || y >= size) return;
    const i = (y * size + x) * 4;
    if (px[i + 3] === 0) {
      px[i] = r; px[i + 1] = g; px[i + 2] = b; px[i + 3] = a;
      return;
    }
    const a1 = px[i + 3] / 255;
    const a2 = a / 255;
    const at = a2 + a1 * (1 - a2);
    px[i] = Math.round((r * a2 + px[i] * a1 * (1 - a2)) / at);
    px[i + 1] = Math.round((g * a2 + px[i + 1] * a1 * (1 - a2)) / at);
    px[i + 2] = Math.round((b * a2 + px[i + 2] * a1 * (1 - a2)) / at);
    px[i + 3] = Math.round(at * 255);
  };

  // rounded-rect background
  const rad = 6 * s;
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const cx = Math.min(Math.max(x + 0.5, rad), size - rad);
      const cy = Math.min(Math.max(y + 0.5, rad), size - rad);
      const dx = x + 0.5 - cx;
      const dy = y + 0.5 - cy;
      if (dx * dx + dy * dy <= rad * rad) set(x, y, 26, 22, 40);
    }
  }

  const line = (x0, y0, x1, y1, col, w = 1) => {
    const steps = Math.max(Math.abs(x1 - x0), Math.abs(y1 - y0), 1) * 2;
    for (let k = 0; k <= steps; k++) {
      const t = k / steps;
      const x = x0 + (x1 - x0) * t;
      const y = y0 + (y1 - y0) * t;
      for (let ox = -Math.floor(w / 2); ox < Math.ceil(w / 2); ox++)
        for (let oy = -Math.floor(w / 2); oy < Math.ceil(w / 2); oy++)
          set(Math.round(x) + ox, Math.round(y) + oy, col[0], col[1], col[2]);
    }
  };
  const disc = (cx, cy, r, col) => {
    for (let y = Math.floor(cy - r) - 1; y <= Math.ceil(cy + r) + 1; y++)
      for (let x = Math.floor(cx - r) - 1; x <= Math.ceil(cx + r) + 1; x++) {
        const dx = x + 0.5 - cx;
        const dy = y + 0.5 - cy;
        if (dx * dx + dy * dy <= r * r) set(x, y, col[0], col[1], col[2]);
      }
  };

  const SP = [184, 148, 246];
  const SP2 = [232, 220, 255];

  disc(16 * s, 19 * s, 3.4 * s, SP);
  disc(16 * s, 13.5 * s, 2.4 * s, SP);
  const legs = [
    [13.5, 17, 8, 11],
    [13.2, 19, 7, 19],
    [13.4, 21, 8, 26],
    [14, 22.5, 11, 29],
    [18.5, 17, 24, 11],
    [18.8, 19, 25, 19],
    [18.6, 21, 24, 26],
    [18, 22.5, 21, 29],
  ];
  for (const [x0, y0, x1, y1] of legs)
    line(x0 * s, y0 * s, x1 * s, y1 * s, SP, Math.max(2, Math.round(2 * s / 2)));
  set(Math.round(15 * s), Math.round(13 * s), ...SP2);
  set(Math.round(17 * s), Math.round(13 * s), ...SP2);

  const raw = Buffer.alloc(size * (1 + size * 4));
  for (let y = 0; y < size; y++) {
    raw[y * (1 + size * 4)] = 0;
    px.copy(raw, y * (1 + size * 4) + 1, y * size * 4, (y + 1) * size * 4);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

const out32 = resolve(here, "../src-tauri/icons/icon.png");
mkdirSync(dirname(out32), { recursive: true });
writeFileSync(out32, draw(32));
const out1024 = resolve(here, "../src-tauri/icons/icon-1024.png");
writeFileSync(out1024, draw(1024));
console.log("wrote", out32, "and", out1024);
