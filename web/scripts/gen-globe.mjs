/**
 * 生成 `public/world-geo.json`：国家代码 → 经纬度多边形环。
 *
 * 和之前的 `world.json` 不同 —— 那个存的是**已经投影好的** SVG 路径（平面图），
 * 没法拿来画球。球面渲染必须要原始经纬度，投影在浏览器里现算。
 *
 * 数据：world-atlas 的 countries-110m（Natural Earth，公有领域）。
 * 110m 是最粗的一档，对一个三四百像素的球来说绰绰有余。
 *
 * **降精度到小数点后 1 位**：0.1° 在赤道约 11 km，而球上 1 像素约等于 60 km，
 * 再精确一位都落在同一个像素里。体积能省一多半。
 *
 * 用法：node scripts/gen-globe.mjs
 */
import { writeFileSync, readdirSync } from 'node:fs'
import { createRequire } from 'node:module'
import { gzipSync } from 'node:zlib'
import { ISO_NUMERIC_TO_ALPHA2 } from './iso-numeric.mjs'

const require = createRequire(import.meta.url)
const topology = require('world-atlas/countries-110m.json')
const { feature } = require('topojson-client')

const fc = feature(topology, topology.objects.countries)

/** flag-icons 认得的 alpha-2 全集 —— 拿来校验数字码映射表有没有打错字。 */
const known = new Set(
  readdirSync(new URL('../node_modules/flag-icons/flags/4x3', import.meta.url))
    .filter((f) => f.endsWith('.svg'))
    .map((f) => f.slice(0, -4).toUpperCase()),
)

const r1 = (n) => Math.round(n * 10) / 10

/** 环上相邻点降精度后会撞成同一个点，去掉重复的，顺便丢掉退化成线的环。 */
function thin(ring) {
  const out = []
  for (const [lon, lat] of ring) {
    const p = [r1(lon), r1(lat)]
    const last = out[out.length - 1]
    if (!last || last[0] !== p[0] || last[1] !== p[1]) out.push(p)
  }
  // 首尾相同的话去掉尾巴，渲染时自己闭合
  if (out.length > 1) {
    const a = out[0]
    const b = out[out.length - 1]
    if (a[0] === b[0] && a[1] === b[1]) out.pop()
  }
  return out.length >= 3 ? out : null
}

const countries = {}
const unmapped = []
for (const f of fc.features) {
  const a2 = ISO_NUMERIC_TO_ALPHA2[String(f.id).padStart(3, '0')]
  if (!a2) {
    unmapped.push(`${f.id} ${f.properties?.name}`)
    continue
  }
  if (!known.has(a2)) throw new Error(`映射表里的 ${a2}（来自 ${f.id}）不是有效国家码`)

  const polys = f.geometry.type === 'Polygon' ? [f.geometry.coordinates] : f.geometry.coordinates
  const rings = []
  for (const poly of polys) {
    // 只要外环。内环（湖泊、飞地空洞）在这个尺度上看不出来，
    // 而且带上它们球面裁剪会复杂一大截。
    const t = thin(poly[0])
    if (t) rings.push(t)
  }
  if (rings.length) countries[a2] = rings
}

const n = Object.keys(countries).length
if (n < 160) throw new Error(`只生成了 ${n} 个国家，太少了，映射表大概有问题`)
if (unmapped.length) console.warn(`  未映射（数字码不在表里）：${unmapped.join(', ')}`)

const json = JSON.stringify(countries)
writeFileSync(new URL('../public/world-geo.json', import.meta.url), json)
console.log(
  `  world-geo.json：${n} 个国家，` +
    `${(json.length / 1024).toFixed(0)} KB，gzip ${(gzipSync(json).length / 1024).toFixed(0)} KB`,
)
