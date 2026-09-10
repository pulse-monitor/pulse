<div align="center">

# Pulse

**看住你所有的小鸡**

装上就能用的 VPS 监控面板。谁掉线了、流量跑了多少、哪台快到期了 —— 一页看完。

[![CI](https://github.com/pulse-monitor/pulse/actions/workflows/ci.yml/badge.svg)](https://github.com/pulse-monitor/pulse/actions/workflows/ci.yml)
[![Docker](https://github.com/pulse-monitor/pulse/actions/workflows/docker.yml/badge.svg)](https://github.com/pulse-monitor/pulse/actions/workflows/docker.yml)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**[📖 文档](https://pulse-doc.pages.dev/)** · [快速开始](https://pulse-doc.pages.dev/install/quick-start) · [Docker](https://pulse-doc.pages.dev/install/docker) · [开发指南](https://pulse-doc.pages.dev/dev/build)

<img src="https://pulse-doc.pages.dev/shots/home-dark.webp" alt="Pulse 面板界面" width="820">

</div>

---

## 装起来

**面板**

```bash
curl -fsSL https://raw.githubusercontent.com/pulse-monitor/pulse/main/deploy/scripts/install-server.sh \
  | sudo bash -s -- --url https://panel.example.com
```

装完打开面板，**第一个访问的人设定自己的用户名和密码** —— 安装时不带密码参数，
也不用去日志里翻。所以装完请立刻去设置。

**探针** —— 后台加机器后点「安装命令」，把生成的命令贴到目标机器上。

Docker、反向代理、HTTPS 等见[文档](https://pulse-doc.pages.dev/install/quick-start)。

## 有什么

- 实时监控 CPU / 内存 / 硬盘 / 网速 / 连接数 / 进程数 / 温度，2 秒一帧
- 账单与到期提醒，多币种自动汇率，算得出这批机器还剩多少钱没用完
- 流量按计费周期统计，超配额告警
- 多个探测点的延迟画在同一张图上
- 地理分布：按 IP 自动认国家，GeoIP 库在本地，**IP 不外传**
- 通知：Telegram / 企业微信 / Webhook / 邮件
- 探针 2.3 MB、内存 4 MB，Linux / Windows / macOS

## 两件刻意不做的事

**探针不接受任何远程指令。** 面板暴露在公网上，它被攻陷的概率远高于你的每一台
VPS。如果探针听命于面板，攻陷面板就等于攻陷所有机器。协议里根本没有「执行」这类
消息，CI 里有断言盯着。

**面板不依赖外部数据库。** 单个二进制加 SQLite，50～200 台够用，省掉一整套数据库
运维。更大规模改一个环境变量切 PostgreSQL。

还有一条贯穿全局的规则：**采不到的指标如实标记为不可用，不显示 0**。一个写着
「温度 0°C」的面板比没有这一项更糟 —— 它让你以为自己知道。

## 仓库

本仓库是**面板**（server），协议定义也在这里。另外两块各自独立发版：

| 仓库 | 内容 | 技术栈 |
|---|---|---|
| **pulse**（这里） | 面板 + 协议定义 | Rust · axum · tokio · sqlx（SQLite / PostgreSQL）· rustls |
| [pulse-web](https://github.com/pulse-monitor/pulse-web) | 面板前端 | React 19 · TypeScript · Tailwind v4 · uPlot · Vite |
| [pulse-agent](https://github.com/pulse-monitor/pulse-agent) | 探针 | Rust · 静态链接 musl · 三平台独立采集实现 |
| [pulse-docs](https://github.com/pulse-monitor/pulse-docs) | 文档站 | VitePress |

探针按 tag 引用这里的 `pulse-proto`。**改协议要记得给 proto 打新 tag**，
否则探针那边取不到 —— 面板发版时 Release workflow 会自动打。

## 构建

```bash
cargo build --release        # 面板
```

面板要靠 `PULSE_WEB_DIR` 提供静态文件，本地开发时指向 pulse-web 的产物：

```bash
PULSE_WEB_DIR=../pulse-web/dist cargo run -p pulse-server
```

跨平台用 `cargo zigbuild`，项目结构和开发约定见[开发指南](https://pulse-doc.pages.dev/dev/build)。

## 许可

[MIT](LICENSE)
