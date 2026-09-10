<div align="center">

# Pulse

**一眼，看见你的基础设施。**

Pulse 是一个轻量、自托管的基础设施可视化平台，把服务器的健康状态、性能、网络、流量与成本集中到一个视图里。

[![CI](https://github.com/pulse-monitor/pulse/actions/workflows/ci.yml/badge.svg)](https://github.com/pulse-monitor/pulse/actions/workflows/ci.yml)
[![Docker](https://github.com/pulse-monitor/pulse/actions/workflows/docker.yml/badge.svg)](https://github.com/pulse-monitor/pulse/actions/workflows/docker.yml)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**[📖 文档](https://pulse-doc.pages.dev/)** · [快速开始](https://pulse-doc.pages.dev/install/quick-start) · [Docker](https://pulse-doc.pages.dev/install/docker) · [开发](https://pulse-doc.pages.dev/dev/build)

<img src="https://pulse-doc.pages.dev/shots/home-dark.webp" alt="Pulse Dashboard" width="820">

</div>

---

## 它回答什么问题

服务器一多，状态就散了。Pulse 把它们收回到一处，让你能一眼看出：

- 哪台正在异常？
- 哪些资源在持续升高？
- 哪条线路延迟或丢包不对劲？
- 哪台流量快到额度了？
- 哪台快到期了？

## 看见，而不是控制

这是 Pulse 的设计边界，也是它和「远程运维中心」的分界线：

> **Visibility should not become control.**

Agent 只做两件事：**采集**、**上报**。它不开放入站端口、不提供远程 Shell、不执行来自 Server 的命令、不做远程文件操作，可以用非 root 运行。

理由很直接：Server 暴露在公网上，被攻陷的概率高于你的每一台机器。如果 Agent 听命于 Server，攻陷 Server 就等于攻陷全部机器。协议里根本没有「执行」这类消息，CI 里有断言盯着（[check-agent-hardening.sh](https://github.com/pulse-monitor/pulse-agent/blob/main/tools/check-agent-hardening.sh)）。

还有一条贯穿全局的规则：**采不到的指标如实标记为不可用，不显示 0**。一个写着「温度 0°C」的面板比没有这一项更糟 —— 它让你以为自己知道。

## 架构

```text
                         Pulse
                           │
              ┌────────────┴────────────┐
              │                         │
        Pulse Server               Pulse Web
              │
          WebSocket / WSS
              │
      ┌───────┼───────┐
      │       │       │
    Agent   Agent   Agent
```

| | 职责 |
|---|---|
| **Server** | Agent 连接、身份认证、遥测接收与存储、API、告警通知、提供 Web |
| **Agent** | 采集系统与网络指标，通过 WSS 上报 |
| **Web** | Dashboard、服务器列表、图表、流量与成本、设置 |

## 装起来

```bash
curl -fsSL https://raw.githubusercontent.com/pulse-monitor/pulse/main/deploy/scripts/install-server.sh \
  | sudo bash -s -- --url https://panel.example.com
```

装完打开面板，**第一个访问的人设定自己的用户名和密码** —— 安装时不带密码参数，也不用去日志里翻。所以装完请立刻去设置。

添加服务器后，后台会生成对应的 Agent 安装命令，贴到目标机器上执行即可。

### Docker

```bash
docker run -d --name pulse -p 25774:25774 -v pulse-data:/data \
  -e PULSE_PUBLIC_URL=https://panel.example.com \
  ghcr.io/pulse-monitor/pulse:latest
```

镜像也发在 `jinqians/pulse-server`，`linux/amd64` 与 `linux/arm64` 都有。

**`-v pulse-data:/data` 要用具名卷** —— 容器以非 root（uid 65532）运行，
bind mount 的属主默认是 root，写不进去。compose 模板与备份恢复见
[Docker 部署](https://pulse-doc.pages.dev/install/docker)。

### 不想开入站端口

用 [Cloudflare Tunnel](https://pulse-doc.pages.dev/install/config#cloudflare-tunnel-cloudflared)，
Pulse 只监听 `127.0.0.1`，整台机器可以对公网完全关闭 ——
这和 Agent 主动外连的方向是一致的。

反向代理、HTTPS、自签证书见[文档](https://pulse-doc.pages.dev/install/quick-start)。

## 存储

Server 是单个静态二进制 + SQLite，不需要额外的数据库运维。50～200 台规模够用。

存储层抽象成了 `Storage` trait，方便将来接别的后端；**目前只有 SQLite 一个实现**。

## 仓库

本仓库是 **Server**，协议定义（`pulse-proto`）也在这里。

| 仓库 | 内容 | 技术栈 |
|---|---|---|
| **pulse**（这里） | Server + 协议定义 | Rust · axum · tokio · sqlx · rustls |
| [pulse-web](https://github.com/pulse-monitor/pulse-web) | Web 前端 | React 19 · TypeScript · Tailwind v4 · uPlot · Vite |
| [pulse-agent](https://github.com/pulse-monitor/pulse-agent) | Agent | Rust · 静态链接 musl · 三平台独立采集实现 |
| [pulse-docs](https://github.com/pulse-monitor/pulse-docs) | 文档站 | VitePress |

Agent 按 tag 引用这里的 `pulse-proto`。**改协议要记得给 proto 打新 tag**，否则 Agent 那边取不到 —— Server 发版时 Release workflow 会自动打。

## 构建

```bash
cargo build --release
```

Server 通过 `PULSE_WEB_DIR` 提供静态文件，本地开发时指向 pulse-web 的产物：

```bash
PULSE_WEB_DIR=../pulse-web/dist cargo run -p pulse-server
```

跨平台用 `cargo zigbuild`，项目结构与开发约定见[开发文档](https://pulse-doc.pages.dev/dev/build)。

## 参与

自托管项目，欢迎 Issue 与 Pull Request。

## 许可

[MIT](LICENSE)
