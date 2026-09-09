/**
 * 实时推送客户端。
 *
 * 三条纪律：
 * - **页面不可见时断开**，回来再连 —— 后台标签页不该白耗电和带宽
 * - 断线**指数退避 + 抖动**重连，与 agent 侧同一套理由：
 *   面板重启时不能让所有浏览器同时打回来
 * - 连不上时不报错阻塞页面，REST 拿到的数据仍然可用，只是不再刷新
 */

import { API_BASE } from '../api'

export interface Tick {
  t: 'tick'
  ts: number
  servers: Record<
    string,
    {
      online: boolean
      cpu: number
      mem_used: number
      mem_pct: number
      disk_pct: number
      net_in: number
      net_out: number
      rtt: number | null
      loss: number | null
      uptime_s: number
    }
  >
  summary?: { total: number; online: number; offline: number; in_speed: number; out_speed: number }
}

type Handler = (t: Tick) => void

export class Live {
  private ws: WebSocket | null = null
  private attempt = 0
  private timer: ReturnType<typeof setTimeout> | null = null
  private closed = false

  constructor(private readonly onTick: Handler) {}

  start() {
    this.closed = false
    document.addEventListener('visibilitychange', this.onVisibility)
    this.connect()
  }

  stop() {
    this.closed = true
    document.removeEventListener('visibilitychange', this.onVisibility)
    if (this.timer) clearTimeout(this.timer)
    this.ws?.close()
    this.ws = null
  }

  private onVisibility = () => {
    if (document.hidden) {
      // 后台标签页不该白耗电和带宽
      this.ws?.close()
      this.ws = null
    } else if (!this.closed && !this.ws) {
      this.attempt = 0
      this.connect()
    }
  }

  private url(): string {
    const base = API_BASE || location.origin
    return base.replace(/^http/, 'ws') + '/api/v1/public/ws'
  }

  private connect() {
    if (this.closed || document.hidden) return
    try {
      const ws = new WebSocket(this.url())
      this.ws = ws
      ws.onopen = () => {
        this.attempt = 0
        // 空 servers 列表 = 订阅全部（首页就是这个用法）
        ws.send(JSON.stringify({ t: 'subscribe', servers: [], summary: true }))
      }
      ws.onmessage = (e) => {
        try {
          const m = JSON.parse(e.data as string)
          if (m?.t === 'tick') this.onTick(m as Tick)
        } catch {
          /* 忽略解析不了的帧，不断开 */
        }
      }
      ws.onclose = () => {
        this.ws = null
        this.scheduleReconnect()
      }
      ws.onerror = () => ws.close()
    } catch {
      this.scheduleReconnect()
    }
  }

  private scheduleReconnect() {
    if (this.closed || document.hidden) return
    // min(1s × 2^n, 30s) × 抖动 —— 抖动不是可选的：
    // 面板重启后所有浏览器会在同一时刻重连
    const base = Math.min(1000 * 2 ** this.attempt, 30_000)
    const delay = base * (0.8 + Math.random() * 0.4)
    this.attempt++
    if (this.timer) clearTimeout(this.timer)
    this.timer = setTimeout(() => this.connect(), delay)
  }
}
