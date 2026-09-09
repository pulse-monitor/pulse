<div align="center">

# Pulse

**轻量级 VPS 监控面板** · Rust 写的探针，2 MB 二进制，4 MB 内存

[![CI](https://github.com/pulse-monitor/pulse/actions/workflows/ci.yml/badge.svg)](https://github.com/pulse-monitor/pulse/actions/workflows/ci.yml)
[![Docker](https://img.shields.io/docker/v/jinqians/pulse-server?label=docker)](https://hub.docker.com/r/jinqians/pulse-server)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[文档](https://pulse-docs.pages.dev) · [快速开始](#快速开始) · [Docker 部署](#docker)

</div>

---

## 这是什么

给自己那几十台 VPS 用的监控面板。做它的原因很简单：现有方案（komari、哪吒）功能都不错，
但对「我就想看看机器还活着吗、流量跑了多少、什么时候到期」这个需求来说，**太重了**。

| | Pulse | 常见方案 |
|---|---|---|
| 探针二进制 | **2.3 MB** | 15～30 MB |
| 探针内存 | **4 MB** | 30～80 MB |
| 探针权限 | **非 root，零 capabilities，不监听端口** | 常需 root |
| 远程执行 / Web 终端 | **明确不做** | 有 |
| 面板依赖 | 单个二进制 + SQLite | 需要数据库 / Redis |

最后两行是取舍，不是遗漏：

- **不做远程执行**。探针只上报、不接受指令，面板即使被攻陷也不能在你的机器上跑命令。
  这条是设计约束，写进了 CI 断言里。
- **单文件部署**。50～200 台机器的规模下 SQLite 够用，省掉一整套数据库运维。

## 功能

- **实时监控** — CPU / 内存 / 硬盘 / 网速 / 连接数 / 进程数 / 温度，2 秒一帧
- **延迟监测** — 多个探测点画在同一张图上，按名称分色
- **分层存储** — 内存环形缓冲 → 分钟表（7 天）→ 小时表（13 个月），自动上卷与清理
- **账单管理** — 价格、周期、到期提醒、剩余价值折算、多币种自动汇率
- **流量统计** — 按计费周期统计，支持配额与超额告警
- **地理分布** — 自绘 SVG 地球，按 IP 自动判定国家（GeoIP 库在本地，**IP 不出机器**）
- **通知** — Telegram / 企业微信 / Webhook / 邮件，可按分组或单机设阈值
- **自动更新** — 探针自更新有 5 道防线：签名校验、摘要比对、拒绝降级、试用期回滚、下载源只从本地读

## 快速开始

### 面板

```bash
curl -fsSL https://github.com/pulse-monitor/pulse/releases/latest/download/pulse-server-x86_64-unknown-linux-musl \
  -o pulse-server && chmod +x pulse-server

PULSE_ADMIN_PASSWORD='换成你自己的密码' \
PULSE_BIND=0.0.0.0:25774 \
PULSE_PUBLIC_URL=https://panel.example.com \
./pulse-server
```

首次启动会创建管理员 `admin`。没设 `PULSE_ADMIN_PASSWORD` 时会随机生成一个并打进日志。

> **务必走 HTTPS。** 探针的 token 放在 WebSocket 握手头里，明文链路上任何一跳都能拿到。
> 面板自带 TLS（配 `PULSE_TLS_CERT` / `PULSE_TLS_KEY`），也可以放在反代后面。

### 探针

后台 → 服务器 → 添加 → 点「安装命令」，把生成的命令贴到目标机器上：

```bash
curl -fsSL https://panel.example.com/install.sh | sudo bash -s -- \
  --server wss://panel.example.com --token <TOKEN>
```

安装需要 sudo（写 systemd unit、建专用用户），**装完之后探针以非特权用户运行**。

支持 Linux（x86_64 / aarch64，glibc 与 musl）、Windows、macOS。

## Docker

```yaml
services:
  pulse:
    image: jinqians/pulse-server:latest
    restart: unless-stopped
    ports: ["25774:25774"]
    volumes: ["./data:/data"]
    environment:
      PULSE_BIND: 0.0.0.0:25774
      PULSE_DATA_DIR: /data
      PULSE_DATABASE_URL: sqlite:///data/pulse.db
      PULSE_PUBLIC_URL: https://panel.example.com
      PULSE_ADMIN_PASSWORD: 换成你自己的密码
```

## 配置

| 变量 | 默认 | 说明 |
|---|---|---|
| `PULSE_BIND` | `127.0.0.1:25774` | 监听地址 |
| `PULSE_PUBLIC_URL` | 由监听地址推导 | **生成安装命令时写进去的地址**，装在别的机器上必须设对 |
| `PULSE_DATABASE_URL` | `sqlite://data/pulse.db` | 数据库位置 |
| `PULSE_DATA_DIR` | `data` | 数据目录（GeoIP 缓存、密钥） |
| `PULSE_WEB_DIR` | `web/dist` | 前端静态文件目录 |
| `PULSE_ADMIN_PASSWORD` | 随机 | 首次启动创建管理员用，≥8 位 |
| `PULSE_TLS_CERT` / `PULSE_TLS_KEY` | 空 | 配上就直接跑 HTTPS |
| `PULSE_TRUSTED_PROXY_HOPS` | `0` | 放在反代后面时设成代理层数 |
| `PULSE_TIMEZONE` | `Asia/Shanghai` | 面板时区，影响账单周期 |
| `PULSE_LOG` | `info` | 日志级别 |

完整说明见[文档站](https://pulse-docs.pages.dev)。

## 从源码构建

```bash
# 面板 + 探针
cargo build --release

# 前端
cd web && npm install && npm run build
```

跨平台构建用 `cargo zigbuild`（不需要装各平台的 C 交叉编译器）：

```bash
cargo zigbuild --release --target x86_64-unknown-linux-musl
```

## 技术栈

**后端** Rust · axum · tokio · sqlx（SQLite / PostgreSQL 双方言）· rustls
**前端** React 19 · TypeScript · Tailwind v4 · uPlot · Vite

地球是自己写的正交投影（约一百行，无 WebGL、无 d3-geo），国家多边形来自
Natural Earth；国旗用 [flag-icons](https://github.com/lipis/flag-icons)；
GeoIP 用 [DB-IP Lite](https://db-ip.com/db/lite.php)（CC BY 4.0）。

## 许可

MIT
