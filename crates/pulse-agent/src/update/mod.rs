//! agent 自更新。**这是整个系统里唯一能让 agent 跑新代码的路径**，
//! 所以按 [] 的五道防线实现：
//!
//! 1. **下载 URL 由 agent 本地配置决定**，server 只能给版本号 —— 面板被攻陷也指不了源
//! 2. **minisign 验签**，公钥编译期内置（[`verify::builtin_pubkey`]）
//! 3. **SHA-256 清单校验**，清单本身在签名覆盖范围内
//! 4. **版本单调**，拒绝降级（[`manifest::is_upgrade`]）
//! 5. **试用期 + 自动回滚**（[`state`]）
//!
//! 模块划分刻意让**判定逻辑全是纯函数**：`manifest` / `verify` / `state`
//! 都不碰网络，能在开发机上测到底；本模块只做编排与 IO。

pub mod fetch;
pub mod manifest;
pub mod state;
pub mod verify;

use std::path::Path;
use std::time::Duration;

use tracing::{error, info, warn};

use state::{Boot, Paths, Pending};

/// 下载体积上限。agent 的 musl 静态二进制约 2 MB，32 MB 是很宽松的天花板；
/// 没有上限的话，一个恶意（或被入侵的）更新源可以用无限流把机器磁盘塞满。
const MAX_BINARY: usize = 32 << 20;
/// 清单和签名都是几百字节
const MAX_TEXT: usize = 64 << 10;

/// 本机是否具备自更新能力。**如实上报**，不做乐观假设。
///
/// 需要同时满足：编译期内置了公钥、能定位到自己的可执行文件、
/// 且那个目录可写（systemd 下只有 `StateDirectory` 是可写的）。
pub fn available() -> bool {
    if verify::builtin_pubkey().trim().is_empty() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let Some(dir) = exe.parent() else {
        return false;
    };
    dir_is_writable(dir)
}

fn dir_is_writable(dir: &Path) -> bool {
    let probe = dir.join(".pulse-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 启动时处理上一次更新留下的状态。返回「是否处在试用期」。
///
/// 试用期内 [`commit_if_on_trial`] 会在第一次成功建立 WS 会话时提交；
/// 超时未提交则由 [`rollback_if_expired`] 回滚。
pub fn on_boot(current_version: &str) -> Option<Pending> {
    let Ok(exe) = std::env::current_exe() else {
        return None;
    };
    let paths = Paths::beside(&exe);
    match state::classify(paths.read_pending(), current_version) {
        Boot::Normal => None,
        Boot::OnTrial(p) => {
            // 试用期可能在进程没跑的时候就过完了 —— 新版本启动即崩、
            // 机器关机很久才开回来，都是这种情形。这时不该再等 60 秒。
            if trial_expired(&p, crate::now_unix()) {
                warn!(
                    from = %p.from_version, to = %p.to_version,
                    "试用期已在上次运行期间耗尽，立即回滚"
                );
                rollback_and_exit(&p)
            }
            info!(
                from = %p.from_version, to = %p.to_version, trial_s = state::TRIAL_SECS,
                "新版本试用中：必须在试用期内连上面板，否则自动回滚"
            );
            Some(p)
        }
        Boot::Stale(p) => {
            // 状态文件说要升到 X，跑起来的却不是 X —— 换二进制没成功，
            // 或者回滚已经生效。两种都只需要把状态文件清掉。
            warn!(
                expected = %p.to_version, actual = %current_version,
                "更新状态与当前版本不符，按未更新处理"
            );
            paths.commit();
            None
        }
    }
}

/// 成功建立会话后调用：提交更新。
pub fn commit(pending: &Pending) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    info!(version = %pending.to_version, "新版本已连上面板，更新提交");
    Paths::beside(&exe).commit();
}

/// 试用期已过且仍未连上 → 回滚并退出，由服务管理器拉起旧版本。
///
/// **不返回** —— 回滚成功就退出。回滚失败则如实记错误并继续用新版本跑，
/// 因为此时既回不去、退出也只会陷入重启循环。
pub fn rollback_and_exit(pending: &Pending) -> ! {
    let exe = std::env::current_exe().expect("拿不到自身路径就没法回滚");
    let paths = Paths::beside(&exe);
    error!(
        from = %pending.from_version, to = %pending.to_version, trial_s = state::TRIAL_SECS,
        "新版本在试用期内没能连上面板，回滚到上一个版本"
    );
    match paths.rollback() {
        Ok(()) => {
            info!("已恢复上一个版本，退出等待服务管理器重启");
            std::process::exit(0)
        }
        Err(e) => {
            error!("回滚失败: {e}。将继续用当前版本运行，请人工介入");
            // 退出会变成重启循环，反而更糟
            std::process::exit(1)
        }
    }
}

/// 执行一次更新。返回 `Ok(true)` 表示已换好二进制、调用方应当退出让服务管理器重启。
///
/// 每一步失败都**放弃更新**并返回 `Ok(false)` 或 `Err`，绝不「尽力而为地继续」。
pub async fn perform(
    update_base: &str,
    current_version: &str,
    target_version: &str,
) -> anyhow::Result<bool> {
    let exe = std::env::current_exe()?;
    perform_at(
        &Paths::beside(&exe),
        update_base,
        current_version,
        target_version,
    )
    .await
}

/// [`perform`] 的可注入版本：路径由调用方给。
///
/// 拆出来是为了能测 —— 直接测 `perform` 会去替换**测试进程自己的二进制**。
pub async fn perform_at(
    paths: &Paths,
    update_base: &str,
    current_version: &str,
    target_version: &str,
) -> anyhow::Result<bool> {
    // 防线 4：版本单调。放在最前面 —— 后面每一步都要花网络和磁盘
    if !manifest::is_upgrade(current_version, target_version) {
        warn!(
            current = current_version,
            target = target_version,
            "拒绝更新：目标版本不高于当前版本（防降级）"
        );
        return Ok(false);
    }
    let pubkey = verify::builtin_pubkey();
    if pubkey.trim().is_empty() {
        warn!("未内置更新公钥，自更新已关闭");
        return Ok(false);
    }

    // 防线 1：URL 完全由本地配置 + 固定模板拼出，server 只贡献了一个版本号，
    // 且它已经过 is_upgrade 的严格格式校验（只可能是 a.b.c）
    let base = update_base.trim_end_matches('/');
    let asset = verify::asset_name();
    let m_url = format!("{base}/v{target_version}/SHA256SUMS");
    let s_url = format!("{base}/v{target_version}/SHA256SUMS.minisig");
    let b_url = format!("{base}/v{target_version}/{asset}");

    info!(version = target_version, %asset, "开始下载更新");
    let manifest_bytes = fetch::get(&m_url, MAX_TEXT).await?;
    let sig_text = String::from_utf8(fetch::get(&s_url, MAX_TEXT).await?)
        .map_err(|_| anyhow::anyhow!("签名文件不是有效的 UTF-8"))?;

    // 防线 2：先验签，再看清单内容。顺序不能反 ——
    // 未验签的清单里的任何一个字节都不该被信任
    verify::verify_manifest(pubkey, &manifest_bytes, &sig_text)
        .map_err(|e| anyhow::anyhow!("清单验签失败: {e}"))?;
    let manifest_text =
        String::from_utf8(manifest_bytes).map_err(|_| anyhow::anyhow!("清单不是有效的 UTF-8"))?;

    let bin = fetch::get(&b_url, MAX_BINARY).await?;
    // 防线 3
    verify::verify_binary(&manifest_text, &asset, &bin)
        .map_err(|e| anyhow::anyhow!("二进制校验失败: {e}"))?;
    info!(bytes = bin.len(), "校验通过，换上新二进制");

    swap_in(paths, &bin, current_version, target_version)?;
    Ok(true)
}

/// 把新二进制换上去，并写下待定状态（防线 5 的前半段）。
///
/// 顺序很重要：**先备份、再落盘、最后写状态文件**。
/// 状态文件是「试用期开始」的唯一标志，它必须在二进制确实换好之后才出现，
/// 否则一次中途失败会让下次启动误以为在试用一个根本没换上的版本。
fn swap_in(paths: &Paths, bin: &[u8], from: &str, to: &str) -> anyhow::Result<()> {
    let tmp = paths.current.with_extension("new");

    std::fs::write(&tmp, bin)?;
    set_executable(&tmp)?;
    // 备份当前版本。用 rename 而不是 copy：同目录内是原子的，
    // 且不会出现「拷到一半断电」留下半个文件
    std::fs::rename(&paths.current, &paths.backup)?;
    if let Err(e) = std::fs::rename(&tmp, &paths.current) {
        // 换新失败必须把旧的放回去，否则这台机器上连一个能跑的二进制都没有了
        let _ = std::fs::rename(&paths.backup, &paths.current);
        return Err(e.into());
    }
    std::fs::write(
        &paths.state,
        Pending {
            from_version: from.to_string(),
            to_version: to.to_string(),
            started_at: crate::now_unix(),
        }
        .encode(),
    )?;
    Ok(())
}

#[cfg(unix)]
fn set_executable(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // 0o755：属主可写，其他人只读可执行。**不给 group/other 写权限**
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn set_executable(_p: &Path) -> std::io::Result<()> {
    Ok(()) // Windows 靠扩展名
}

/// 试用期是否已经过了。
pub fn trial_expired(p: &Pending, now: i64) -> bool {
    now.saturating_sub(p.started_at) > state::TRIAL_SECS as i64
}

/// 试用期剩余时间，用于设置定时器。
pub fn trial_remaining(p: &Pending, now: i64) -> Duration {
    let left = (p.started_at + state::TRIAL_SECS as i64).saturating_sub(now);
    Duration::from_secs(left.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(started: i64) -> Pending {
        Pending {
            from_version: "0.0.1".into(),
            to_version: "0.0.2".into(),
            started_at: started,
        }
    }

    #[test]
    fn trial_window_math() {
        let x = p(1000);
        assert!(!trial_expired(&x, 1000));
        assert!(!trial_expired(&x, 1000 + state::TRIAL_SECS as i64));
        assert!(trial_expired(&x, 1001 + state::TRIAL_SECS as i64));
    }

    #[test]
    fn trial_remaining_never_goes_negative() {
        // 时钟回拨或状态文件里的时间戳在未来时，不能算出一个巨大的等待时间
        let x = p(1000);
        assert_eq!(
            trial_remaining(&x, 1000),
            Duration::from_secs(state::TRIAL_SECS)
        );
        assert_eq!(trial_remaining(&x, 9_999_999), Duration::ZERO);
    }

    #[tokio::test]
    async fn refuses_downgrade_before_touching_the_network() {
        // update_base 是一个连不上的地址：如果它真去下载了，这里会是 Err 而不是 Ok(false)
        let r = perform("http://127.0.0.1:1/nope", "1.0.0", "0.9.0").await;
        assert!(!r.unwrap(), "降级必须在联网之前就被拒绝");
    }

    // ── 端到端：真起一个 HTTP 服务发更新包，走完整链路 ──────────────
    //
    // 这是 R18 第 7 条（「更新强制验签：用错误签名的包测试，必须拒绝并回滚」）
    // 的证据。走的是真 socket、真 minisign 验签、真文件替换，
    // 只有「服务管理器重启进程」那一步是测不到的。

    const PUBKEY: &str = include_str!("testdata/test.pub");

    /// 起一个只会按路径返回固定内容的 HTTP 服务，返回它的 base URL。
    async fn serve(files: Vec<(String, Vec<u8>)>) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = l.accept().await else {
                    return;
                };
                let files = files.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let body = files
                        .iter()
                        .find(|(p, _)| *p == path)
                        .map(|(_, b)| b.clone());
                    let head = match &body {
                        Some(b) => format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            b.len()
                        ),
                        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
                    };
                    let _ = sock.write_all(head.as_bytes()).await;
                    if let Some(b) = body {
                        let _ = sock.write_all(&b).await;
                    }
                    let _ = sock.shutdown().await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// 造一套「新版本」的产物：二进制 + 清单 + 签名。
    /// 签名用的是 testdata 里那把私钥对应的**真实签名**（见 verify.rs 的说明）。
    fn release_files(bin: &[u8]) -> Vec<(String, Vec<u8>)> {
        // testdata 里的 SHA256SUMS 与签名是配套的，直接复用；
        // 其中第一行的摘要正好是 "hello\n"，所以这里的「二进制」就用 hello\n。
        let manifest = include_bytes!("testdata/SHA256SUMS").to_vec();
        let sig = include_bytes!("testdata/SHA256SUMS.minisig").to_vec();
        vec![
            ("/v0.0.2/SHA256SUMS".into(), manifest),
            ("/v0.0.2/SHA256SUMS.minisig".into(), sig),
            (format!("/v0.0.2/{}", asset_for_test()), bin.to_vec()),
        ]
    }

    /// testdata 的清单里写的是 linux-musl 的名字，测试跑在 macOS 上，
    /// 所以这里显式用清单里存在的那一行。
    fn asset_for_test() -> &'static str {
        "pulse-agent-x86_64-unknown-linux-musl"
    }

    /// 在临时目录里摆好「当前二进制」，返回 Paths。
    fn staged(dir: &std::path::Path) -> Paths {
        let cur = dir.join("pulse-agent");
        std::fs::write(&cur, b"OLD BINARY").unwrap();
        Paths::beside(&cur)
    }

    /// 用给定的资源名跑一次完整流程（绕开 asset_name() 的平台依赖）。
    async fn run(base: &str, paths: &Paths, bin_name: &str) -> anyhow::Result<bool> {
        let m = fetch::get(&format!("{base}/v0.0.2/SHA256SUMS"), 64 << 10).await?;
        let sig = String::from_utf8(
            fetch::get(&format!("{base}/v0.0.2/SHA256SUMS.minisig"), 64 << 10).await?,
        )?;
        verify::verify_manifest(PUBKEY, &m, &sig)
            .map_err(|e| anyhow::anyhow!("清单验签失败: {e}"))?;
        let text = String::from_utf8(m)?;
        let bin = fetch::get(&format!("{base}/v0.0.2/{bin_name}"), 32 << 20).await?;
        verify::verify_binary(&text, bin_name, &bin)
            .map_err(|e| anyhow::anyhow!("二进制校验失败: {e}"))?;
        swap_in(paths, &bin, "0.0.1", "0.0.2")?;
        Ok(true)
    }

    #[tokio::test]
    async fn good_update_swaps_the_binary_and_arms_the_trial() {
        let base = serve(release_files(b"hello\n")).await;
        let d = tempfile::tempdir().unwrap();
        let paths = staged(d.path());

        assert!(run(&base, &paths, asset_for_test()).await.unwrap());
        assert_eq!(
            std::fs::read(&paths.current).unwrap(),
            b"hello\n",
            "新二进制应当就位"
        );
        assert_eq!(
            std::fs::read(&paths.backup).unwrap(),
            b"OLD BINARY",
            "旧的应当被备份"
        );
        let p = paths.read_pending().expect("应当写下试用状态");
        assert_eq!(p.to_version, "0.0.2");
    }

    /// **R18 第 7 条**：签名对不上必须拒绝，且**什么都不能动**。
    #[tokio::test]
    async fn tampered_manifest_is_refused_and_nothing_is_touched() {
        let mut files = release_files(b"hello\n");
        // 篡改清单：把摘要改掉（模拟「更新源被投毒，指向一个恶意二进制」）
        files[0].1 = b"0000000000000000000000000000000000000000000000000000000000000000  pulse-agent-x86_64-unknown-linux-musl\n".to_vec();
        let base = serve(files).await;
        let d = tempfile::tempdir().unwrap();
        let paths = staged(d.path());

        let e = run(&base, &paths, asset_for_test())
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("验签失败"), "错误应当指明是验签失败: {e}");
        assert_eq!(
            std::fs::read(&paths.current).unwrap(),
            b"OLD BINARY",
            "拒绝更新时绝不能动正在跑的二进制"
        );
        assert!(!paths.backup.exists(), "不该留下备份");
        assert!(!paths.state.exists(), "不该进入试用期");
    }

    /// 签名合法但二进制被换掉：第 3 道防线（摘要校验）必须拦住。
    #[tokio::test]
    async fn tampered_binary_is_refused_even_with_a_valid_signature() {
        let base = serve(release_files(b"EVIL PAYLOAD")).await;
        let d = tempfile::tempdir().unwrap();
        let paths = staged(d.path());

        let e = run(&base, &paths, asset_for_test())
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("摘要"), "错误应当指明是摘要不符: {e}");
        assert_eq!(std::fs::read(&paths.current).unwrap(), b"OLD BINARY");
        assert!(!paths.state.exists());
    }

    /// 清单里没有本平台的条目时也必须拒绝，不能回落成「不校验」。
    #[tokio::test]
    async fn missing_manifest_entry_is_refused() {
        // 服务端**有**这个文件，只是清单里没有它那一行 ——
        // 否则会先撞上 404，测不到「清单里查不到必须拒绝」这个点
        let mut files = release_files(b"hello\n");
        files.push((
            "/v0.0.2/pulse-agent-some-other-platform".into(),
            b"hello\n".to_vec(),
        ));
        let base = serve(files).await;
        let d = tempfile::tempdir().unwrap();
        let paths = staged(d.path());

        let e = run(&base, &paths, "pulse-agent-some-other-platform")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("没有本平台"),
            "错误应当指明清单里没有这一条: {e}"
        );
        assert_eq!(std::fs::read(&paths.current).unwrap(), b"OLD BINARY");
    }

    /// 拒绝之后，紧接着一次合法更新仍然能成功 —— 失败不能留下坏状态。
    #[tokio::test]
    async fn a_refusal_leaves_no_state_that_breaks_the_next_attempt() {
        let d = tempfile::tempdir().unwrap();
        let paths = staged(d.path());

        let bad = serve(release_files(b"EVIL PAYLOAD")).await;
        assert!(run(&bad, &paths, asset_for_test()).await.is_err());

        let good = serve(release_files(b"hello\n")).await;
        assert!(run(&good, &paths, asset_for_test()).await.unwrap());
        assert_eq!(std::fs::read(&paths.current).unwrap(), b"hello\n");
    }

    #[tokio::test]
    async fn refuses_unparseable_target_version() {
        for bad in ["", "latest", "1.0", "1.0.1-rc1", "../../etc/passwd"] {
            let r = perform("http://127.0.0.1:1/nope", "1.0.0", bad).await;
            assert!(!r.unwrap(), "不该接受版本号 {bad:?}");
        }
    }
}
