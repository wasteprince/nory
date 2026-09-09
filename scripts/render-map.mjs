#!/usr/bin/env node
// Build a compact, deterministic land mask from Natural Earth's public-domain
// coastlines. Runtime rendering needs no GeoJSON parser or network requests.
import { readFileSync, writeFileSync } from 'node:fs';

const columns = 240;
const rows = 120;
const source = new URL('../assets/maps/ne_110m_land.geojson', import.meta.url);
const target = new URL('../assets/maps/world-land.bin', import.meta.url);
const collection = JSON.parse(readFileSync(source, 'utf8'));
const polygons = collection.features.flatMap(({ geometry }) => {
  if (geometry.type === 'Polygon') return [geometry.coordinates];
  if (geometry.type === 'MultiPolygon') return geometry.coordinates;
  throw new Error(`Unsupported land geometry: ${geometry.type}`);
}).map(rings => ({
  rings,
  minX: Math.min(...rings[0].map(p => p[0])),
  maxX: Math.max(...rings[0].map(p => p[0])),
  minY: Math.min(...rings[0].map(p => p[1])),
  maxY: Math.max(...rings[0].map(p => p[1])),
}));

function insideRing(x, y, ring) {
  let inside = false;
  for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
    const [a, b] = ring[i];
    const [c, d] = ring[j];
    if ((b > y) !== (d > y) && x < (c - a) * (y - b) / (d - b) + a) inside = !inside;
  }
  return inside;
}

const mask = new Uint8Array(columns * rows / 8);
let count = 0;
for (let row = 0; row < rows; row++) {
  for (let column = 0; column < columns; column++) {
    const x = (column + 0.5) / columns * 360 - 180;
    const y = 90 - (row + 0.5) / rows * 180;
    if (!polygons.some(p => x >= p.minX && x <= p.maxX && y >= p.minY && y <= p.maxY
      && insideRing(x, y, p.rings[0]) && !p.rings.slice(1).some(hole => insideRing(x, y, hole)))) continue;
    const index = row * columns + column;
    mask[index >> 3] |= 1 << (index & 7);
    count++;
  }
}
if (count < 5000 || count > 13000) throw new Error(`Unexpected land mask: ${count} dots`);
writeFileSync(target, mask);
console.log(`World map: ${count} land dots, ${mask.length} bytes`);
