<div align="center">

<img src="https://raw.githubusercontent.com/pulse-monitor/pulse-docs/main/public/favicon.svg" width="72" alt="Pulse">

# Pulse

**轻量级 VPS 监控面板**

探针 2.3 MB · 常驻内存 4 MB · 非 root 运行 · 不监听端口 · 不接受远程指令

[![CI](https://github.com/pulse-monitor/pulse/actions/workflows/ci.yml/badge.svg)](https://github.com/pulse-monitor/pulse/actions/workflows/ci.yml)
[![Docker](https://github.com/pulse-monitor/pulse/actions/workflows/docker.yml/badge.svg)](https://github.com/pulse-monitor/pulse/actions/workflows/docker.yml)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**[📖 在线文档](https://pulse-doc.pages.dev/)** · [快速开始](#快速开始) · [Docker](#docker-部署) · [功能](#功能)

</div>

---

## 快速开始

### 1. 装面板

```bash
curl -fsSL https://raw.githubusercontent.com/pulse-monitor/pulse/main/deploy/scripts/install-server.sh \
  | sudo bash -s -- --url https://panel.example.com
```

脚本会下载并校验摘要、建专用用户、写一份加固过的 systemd unit、起服务，
最后打印随机生成的管理员密码。

> `--url` 是面板的对外地址，**生成探针安装命令时会写进去**，装在公网上必须给对。

### 2. 装探针

后台 → 服务器 → 添加 → 点「安装命令」，把生成的命令贴到目标机器上：

```bash
curl -fsSL https://panel.example.com/install.sh | sudo bash -s -- \
  --server wss://panel.example.com --token <TOKEN>
```

安装需要 sudo（写 systemd unit、建专用用户），**装完之后探针以非特权用户运行**。

支持 Linux（x86_64 / aarch64）、Windows、macOS。

### 3. 打开面板

浏览器访问 `https://panel.example.com`，机器一两秒内变绿。

更详细的步骤、反向代理、HTTPS 配置见 **[在线文档](https://pulse-doc.pages.dev/install/quick-start)**。

---

## Docker 部署

镜像同时发在 GHCR 和 Docker Hub，推荐 GHCR —— 公开镜像**没有拉取限额**。
支持 `linux/amd64` 与 `linux/arm64`。

```yaml
services:
  pulse:
    image: ghcr.io/pulse-monitor/pulse:latest
    restart: unless-stopped
    ports: ["25774:25774"]
    volumes: ["./data:/data"]
    environment:
      PULSE_PUBLIC_URL: https://panel.example.com
      PULSE_ADMIN_PASSWORD: 换成你自己的密码
```

镜像基于 distroless，**没有 shell**，以非 root 用户运行，数据全在 `/data` 一个卷里。

---

## 功能

| | |
|---|---|
| **实时监控** | CPU、内存、硬盘、网速、连接数、进程数、温度、GPU，2 秒一帧 |
| **延迟监测** | 多个探测点画在同一张图上，按名称分色 |
| **分层存储** | 内存环形缓冲 → 分钟表（7 天）→ 小时表（13 个月），自动上卷与清理 |
| **账单管理** | 价格、周期、到期提醒、剩余价值折算、多币种自动汇率 |
| **流量统计** | 按计费周期统计，支持配额与超额告警；探针重启不丢数据 |
| **地理分布** | 自绘 SVG 地球，有机器的国家整块点亮 |
| **通知** | Telegram、企业微信、Webhook、邮件；阈值分全局 / 分组 / 单机三层 |
| **自动更新** | 探针自更新有 5 道防线：签名校验、摘要比对、拒绝降级、试用期回滚、下载源只从本地读 |

---

## 设计取舍

有两件事是**刻意不做**的，写进了 CI 断言，不是还没来得及：

**探针不接受任何远程指令。** 面板暴露在公网上，它被攻陷的概率远高于你的每一台 VPS。
如果探针听命于面板，攻陷面板就等于攻陷所有机器。协议里根本没有「执行」这类消息。

**面板不依赖外部数据库。** 50～200 台机器的规模下 SQLite 完全够用，
省掉一整套数据库运维。规模更大时改一个环境变量就能切 PostgreSQL。

还有一条贯穿全局的规则：**采不到的指标如实标记为不可用，不显示 0**。
一个写着「温度 0°C」「进程数 0」的面板比没有这一项更糟 ——
它让你以为自己知道，其实不知道。

---

## 配置

| 变量 | 默认 | 说明 |
|---|---|---|
| `PULSE_BIND` | `127.0.0.1:25774` | 监听地址 |
| `PULSE_PUBLIC_URL` | 由监听地址推导 | 对外地址，**生成安装命令时写进去** |
| `PULSE_DATABASE_URL` | `sqlite://data/pulse.db` | 数据库，也支持 `postgres://` |
| `PULSE_DATA_DIR` | `data` | 数据目录（GeoIP 缓存、服务端密钥） |
| `PULSE_ADMIN_PASSWORD` | 随机 | 首次启动创建管理员用 |
| `PULSE_TLS_CERT` / `PULSE_TLS_KEY` | 空 | 配上就直接跑 HTTPS |
| `PULSE_TRUSTED_PROXY_HOPS` | `0` | 放在反代后面时设成代理层数 |
| `PULSE_TIMEZONE` | `Asia/Shanghai` | 面板时区，影响账单周期日界 |

完整清单见[文档](https://pulse-doc.pages.dev/install/config)。

> **务必走 HTTPS。** 探针的 token 放在 WebSocket 握手头里，明文链路上任何一跳都能拿到。
> 面板启动时会检查这一点并告警。

---

## 技术栈

| | |
|---|---|
| **后端** | Rust · axum · tokio · sqlx（SQLite / PostgreSQL 双方言）· rustls |
| **前端** | React 19 · TypeScript · Tailwind v4 · uPlot · Vite |
| **探针** | Rust · 静态链接 musl · 三平台独立采集实现 |
| **文档** | VitePress |

几处第三方资源：地球是自己写的正交投影（约一百行，无 WebGL、无 d3-geo），
国家多边形来自 [Natural Earth](https://www.naturalearthdata.com/)（公有领域）；
国旗用 [flag-icons](https://github.com/lipis/flag-icons)（MIT）；
GeoIP 用 [DB-IP Lite](https://db-ip.com/db/lite.php)（CC BY 4.0，**本地离线查询**）。

---

## 从源码构建

```bash
cargo build --release                    # 面板 + 探针
cd web && npm install && npm run build   # 前端
```

跨平台构建用 `cargo zigbuild`，不必装各平台的 C 交叉编译器：

```bash
cargo zigbuild --release --target x86_64-unknown-linux-musl
```

开发相关的约定、项目结构、协议说明见 **[开发指南](https://pulse-doc.pages.dev/dev/build)**。

---

## 许可

[MIT](LICENSE)
