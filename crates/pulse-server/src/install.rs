//! 一键安装命令生成。
//!
//! 对应需求 R3。关键设计是 的**安装选项三分法**：
//!
//! | 类别 | 去处 | 改的时候要不要碰机器 |
//! |---|---|---|
//! | 安装期（安装目录、禁用自动更新） | 只出现在命令行里 | 要 |
//! | 运行期（间隔、网卡、GPU） | **写进数据库**，WS 下发 | 不用 |
//! | 展示期（含缓冲、流量口径、重置日） | **写进数据库**，只影响计算 | 不用 |
//!
//! 所以后 6 项虽然也出现在安装向导里（心智一致），但保存时是写库，
//! 命令里带上只是作为「首次配置」的冗余。

use serde::{Deserialize, Serialize};

/// 安装向导提交的选项（R3 的 8 项）。
#[derive(Debug, Clone, Deserialize)]
pub struct InstallOptions {
    // ── 安装期 ──
    #[serde(default = "default_install_dir")]
    pub install_dir: String,
    #[serde(default)]
    pub disable_auto_update: bool,

    // ── 运行期（同时写库）──
    #[serde(default = "default_interval")]
    pub interval_s: u8,
    #[serde(default)]
    pub net_include: Vec<String>,
    #[serde(default)]
    pub net_exclude: Vec<String>,
    #[serde(default)]
    pub enable_gpu: bool,

    // ── 展示期（同时写库）──
    #[serde(default)]
    pub include_buffcache: bool,
    #[serde(default = "default_reset_day")]
    pub traffic_reset_day: u8,
}

fn default_install_dir() -> String {
    "/opt/pulse-agent".into()
}
fn default_interval() -> u8 {
    2
}
fn default_reset_day() -> u8 {
    1
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            install_dir: default_install_dir(),
            disable_auto_update: false,
            interval_s: default_interval(),
            net_include: Vec::new(),
            net_exclude: Vec::new(),
            enable_gpu: false,
            include_buffcache: false,
            traffic_reset_day: default_reset_day(),
        }
    }
}

impl InstallOptions {
    /// 校验并夹紧到合法区间。
    ///
    /// 这些值会被拼进 shell 命令，所以**必须在这里就把危险字符挡掉** ——
    /// 不能指望下游转义。
    pub fn sanitize(mut self) -> Self {
        self.interval_s = self.interval_s.clamp(1, 60);
        self.traffic_reset_day = self.traffic_reset_day.clamp(1, 31);
        if !is_safe_path(&self.install_dir) {
            self.install_dir = default_install_dir();
        }
        self.net_include.retain(|p| is_safe_pattern(p));
        self.net_exclude.retain(|p| is_safe_pattern(p));
        self.net_include.truncate(32);
        self.net_exclude.truncate(32);
        self
    }
}

/// 安装目录：绝对路径，只允许安全字符。
///
/// 拒绝掉引号、`$`、反引号、分号、换行等一切能在 shell 里改变语义的字符。
pub fn is_safe_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 200
        && p.starts_with('/')
        && !p.contains("..")
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.'))
}

/// 网卡 glob：字母数字加 `* ? - _ . :` —— 覆盖 `eth0`、`veth*`、`br-*`、`eth0.100`。
pub fn is_safe_pattern(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 64
        && p.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'*' | b'?' | b'-' | b'_' | b'.' | b':')
        })
}

/// 生成的四种安装方式。
#[derive(Debug, Serialize)]
pub struct InstallCommands {
    pub shell: String,
    pub powershell: String,
    pub docker: String,
    pub compose: String,
    pub note: String,
}

/// 生成安装命令。
///
/// `panel_url` 是面板的对外地址（http/https），会被换算成对应的 ws/wss。
pub fn render(panel_url: &str, token: &str, o: &InstallOptions) -> InstallCommands {
    let http = panel_url.trim_end_matches('/');
    let ws = to_ws_url(http);
    let img = "ghcr.io/pulse-monitor/pulse-agent:latest";

    let mut flags: Vec<String> = vec![
        format!("--server {ws}"),
        format!("--token {token}"),
        format!("--install-dir {}", o.install_dir),
        format!("--interval {}", o.interval_s),
    ];
    if o.disable_auto_update {
        flags.push("--disable-auto-update".into());
    }
    if o.enable_gpu {
        flags.push("--enable-gpu".into());
    }
    if !o.net_include.is_empty() {
        flags.push(format!("--net-include '{}'", o.net_include.join(",")));
    }
    if !o.net_exclude.is_empty() {
        flags.push(format!("--net-exclude '{}'", o.net_exclude.join(",")));
    }

    // 一条命令同时适配 Debian 系与 Alpine。原生 Alpine **没有 curl、sudo、bash**
    // （只有 busybox 的 wget 和 ash），原先的 `curl … | sudo bash` 在那上面一步都走不动：
    // - 下载：curl 不在就退回 wget；
    // - 提权：`$(command -v sudo)` 在有 sudo 时展开成 sudo，没有时为空 ——
    //   Alpine 上通常本来就是 root；既非 root 又没 sudo 时，脚本自己会报「需要 root」；
    // - 解释器：脚本是 POSIX sh，不需要 bash。
    let url = format!("{http}/install.sh");
    let shell = format!(
        "(curl -fsSL {url} || wget -qO- {url}) \\\n  | $(command -v sudo) sh -s -- \\\n  {}",
        flags.join(" \\\n  ")
    );

    let powershell = format!(
        "irm {http}/install.ps1 -OutFile install.ps1; \
         .\\install.ps1 -Server {ws} -Token {token}"
    );

    // Docker 方式需要三个参数才能看到宿主机的指标，但**不需要 --privileged**。
    //
    // 不要给 bind mount 加 `rslave`：它要求宿主机的 `/` 已经是
    // shared/slave mount，而 Alpine（以及不少精简发行版）默认是 private，
    // Docker 会在创建容器前直接报错。Agent 只需读取启动时可见的宿主文件系统，
    // 不需要接收之后新增挂载点的传播。
    // 各参数去掉后的后果
    let docker = format!(
        "docker run -d --name pulse-agent --restart=always \\\n  \
         --network host \\\n  --pid host \\\n  -v /:/rootfs:ro \\\n  \
         -e PULSE_SERVER={ws} \\\n  -e PULSE_TOKEN={token} \\\n  \
         -e PULSE_ROOTFS=/rootfs \\\n  {img}"
    );

    let compose = format!(
        "services:\n  \
         pulse-agent:\n    \
         image: {img}\n    \
         restart: always\n    \
         network_mode: host\n    \
         pid: host\n    \
         volumes:\n      - \"/:/rootfs:ro\"\n    \
         environment:\n      \
         PULSE_SERVER: \"{ws}\"\n      \
         PULSE_TOKEN: \"{token}\"\n      \
         PULSE_ROOTFS: \"/rootfs\"\n"
    );

    InstallCommands {
        shell,
        powershell,
        docker,
        compose,
        note: format!(
            "**这台机器原来的 token 已经作废了**：库里只存哈希、取不回明文，\
             所以每次生成安装命令都会换一把新钥匙。\
             如果这台机器上已经装了探针，它会掉线，直到用下面的新命令重装。\
             token 只显示这一次，请立即复制；命令里带着 token，执行后建议从 shell 历史中清除。\
             安装需要 root（写服务定义、建专用用户；不是 root 时命令会自动走 sudo），\
             但 agent 运行时是非特权用户。支持 systemd 与 OpenRC（Alpine）。\
             **只是想升级已装的探针，不必来这里生成新命令**（那会让旧 token 作废）：\
             在那台机器上执行 `(curl -fsSL {url} || wget -qO- {url}) | $(command -v sudo) sh -s --`，\
             不带参数即沿用原 token 与配置、升到最新版；末尾加 `--uninstall` 就是卸载。"
        ),
    }
}

/// `http(s)://host` → `ws(s)://host`。
fn to_ws_url(http: &str) -> String {
    if let Some(rest) = http.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = http.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        // 已经是 ws/wss 或没有 scheme：原样返回，由用户自己保证
        http.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_is_translated() {
        assert_eq!(to_ws_url("https://p.example.com"), "wss://p.example.com");
        assert_eq!(to_ws_url("http://127.0.0.1:25774"), "ws://127.0.0.1:25774");
        assert_eq!(to_ws_url("wss://p.example.com"), "wss://p.example.com");
    }

    #[test]
    fn all_four_forms_carry_server_and_token() {
        let c = render(
            "https://p.example.com",
            "TOK123",
            &InstallOptions::default(),
        );
        for (name, s) in [
            ("shell", &c.shell),
            ("powershell", &c.powershell),
            ("docker", &c.docker),
            ("compose", &c.compose),
        ] {
            assert!(s.contains("TOK123"), "{name} 缺少 token");
            assert!(s.contains("p.example.com"), "{name} 缺少面板地址");
        }
        assert!(c.shell.contains("wss://"), "shell 应当用 wss");
        assert!(
            c.docker.contains("--network host"),
            "Docker 需要 host 网络才能看到宿主机网卡"
        );
        assert!(
            c.docker.contains("-v /:/rootfs:ro"),
            "Docker 需要只读挂载宿主根目录才能采集宿主磁盘"
        );
        assert!(
            !c.docker.contains("rslave") && !c.compose.contains("rslave"),
            "rslave 要求宿主根目录是 shared/slave，在 Alpine 上会导致 Docker 创建失败"
        );
        assert!(!c.docker.contains("--privileged"), "绝不能要求 privileged");
    }

    #[test]
    fn 提示里必须写清楚旧_token_会作废() {
        // 「生成安装命令」= 「换一把新钥匙」，已经在跑的探针会当场掉线。
        // 这件事只有提示词能告诉用户 —— 真机上就是这么把一台在线机器踢下线的，
        // 而当时的提示只说了「token 只显示一次」，完全没提旧的会作废。
        let c = render("https://p.example.com", "tok", &InstallOptions::default());
        assert!(
            c.note.contains("作废"),
            "提示要说明旧 token 作废：{}",
            c.note
        );
        assert!(
            c.note.contains("掉线"),
            "提示要说明已装的探针会掉线：{}",
            c.note
        );
    }

    #[test]
    fn shell_命令在原生_alpine_上也能跑() {
        // 原生 Alpine 没有 curl / sudo / bash。命令里任何一个写死，
        // 贴过去就是 `not found`，一步都走不动。
        let c = render("https://p", "T", &InstallOptions::default());
        assert!(
            c.shell.contains("wget -qO-"),
            "curl 不在时要能退回 wget：{}",
            c.shell
        );
        assert!(
            c.shell.contains("$(command -v sudo)"),
            "sudo 不能写死：{}",
            c.shell
        );
        assert!(
            !c.shell.contains("bash"),
            "脚本是 POSIX sh，不该依赖 bash：{}",
            c.shell
        );
        assert!(c.shell.contains("sh -s --"), "{}", c.shell);
    }

    #[test]
    fn optional_flags_only_appear_when_enabled() {
        let off = render("https://p", "T", &InstallOptions::default());
        assert!(!off.shell.contains("--disable-auto-update"));
        assert!(!off.shell.contains("--enable-gpu"));
        assert!(!off.shell.contains("--net-include"));

        let on = render(
            "https://p",
            "T",
            &InstallOptions {
                disable_auto_update: true,
                enable_gpu: true,
                net_include: vec!["eth0".into()],
                net_exclude: vec!["docker*".into(), "veth*".into()],
                ..Default::default()
            },
        );
        assert!(on.shell.contains("--disable-auto-update"));
        assert!(on.shell.contains("--enable-gpu"));
        assert!(on.shell.contains("--net-include 'eth0'"));
        assert!(on.shell.contains("--net-exclude 'docker*,veth*'"));
    }

    #[test]
    fn shell_injection_is_rejected_at_the_source() {
        // 这些值会被拼进 shell 命令，必须在生成前就挡掉，
        // 不能指望下游转义
        for bad in [
            "/opt/x; rm -rf /",
            "/opt/$(whoami)",
            "/opt/`id`",
            "/opt/x'\"",
            "/opt/x\nrm -rf /",
            "relative/path",
            "/opt/../../etc",
        ] {
            assert!(!is_safe_path(bad), "不该接受 {bad:?}");
        }
        assert!(is_safe_path("/opt/pulse-agent"));
        assert!(is_safe_path("/var/lib/pulse_agent.d"));
    }

    #[test]
    fn unsafe_options_fall_back_to_defaults() {
        let o = InstallOptions {
            install_dir: "/opt/x; rm -rf /".into(),
            net_exclude: vec!["docker*".into(), "$(id)".into(), "a;b".into()],
            interval_s: 0,
            traffic_reset_day: 99,
            ..Default::default()
        }
        .sanitize();

        assert_eq!(
            o.install_dir, "/opt/pulse-agent",
            "危险路径必须回落到默认值"
        );
        assert_eq!(o.net_exclude, vec!["docker*"], "含危险字符的模式必须被丢弃");
        assert_eq!(o.interval_s, 1);
        assert_eq!(o.traffic_reset_day, 31);

        // 清理之后生成的命令里不能再有注入痕迹
        let c = render("https://p", "T", &o);
        assert!(!c.shell.contains("rm -rf"));
        assert!(!c.shell.contains("$(id)"));
        // 命令里唯一的命令替换是我们自己写的 `$(command -v sudo)`
        assert_eq!(c.shell.matches("$(").count(), 1, "{}", c.shell);
    }

    #[test]
    fn pattern_whitelist_covers_real_interface_names() {
        for good in [
            "eth0", "veth*", "br-*", "eth0.100", "ens5", "en0", "wg0", "utun*",
        ] {
            assert!(is_safe_pattern(good), "应当接受 {good}");
        }
        for bad in ["", "a b", "a;b", "$(x)", "`x`", "a\nb", &"x".repeat(65)] {
            assert!(!is_safe_pattern(bad), "不该接受 {bad:?}");
        }
    }

    #[test]
    fn 提示里要告诉用户升级和卸载不需要新_token() {
        // 升级不该走「生成安装命令」：那会让旧 token 作废、机器先掉线
        let c = render("https://p.example.com", "T", &InstallOptions::default());
        assert!(c.note.contains("升级"), "{}", c.note);
        assert!(
            c.note.contains("https://p.example.com/install.sh"),
            "升级命令要带上真实的面板地址，能直接复制：{}",
            c.note
        );
        assert!(c.note.contains("--uninstall"), "{}", c.note);
    }

    #[test]
    fn note_warns_about_shell_history_and_sudo() {
        let c = render("https://p", "T", &InstallOptions::default());
        assert!(
            c.note.contains("历史"),
            "必须提醒 token 会留在 shell 历史里"
        );
        assert!(c.note.contains("sudo"), "必须说清安装需要 sudo、运行不需要");
    }
}
