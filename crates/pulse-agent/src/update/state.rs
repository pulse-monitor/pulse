//! 更新的落盘状态与回滚判定。**纯逻辑 + 明确的文件操作，都能测。**
//!
//! 对应第 5 道防线：新版本启动后若干秒内没能建立 WS 连接 → 恢复上一个版本。
//!
//! 状态机（`state_dir/update.json` 是唯一的持久状态）：
//!
//! ```text
//! 无文件            正常运行
//!   │ 换上新二进制、写文件、退出
//!   ▼
//! 有文件 + 当前版本 == to     试用中：连上 → 提交（删文件与 .old）
//!   │                                  超时 → 回滚（.old 覆盖回去）并退出
//!   ▼
//! 有文件 + 当前版本 == from   回滚已生效（或换二进制根本没成功）→ 清掉文件
//! ```

use std::path::{Path, PathBuf};

/// 试用期：新版本必须在这段时间内成功建立一次 WS 会话，否则回滚。
///
/// 60 秒足够覆盖一次退避重连，又不至于让一台坏掉的机器长时间失联。
pub const TRIAL_SECS: u64 = 60;

/// `update.json` 的内容。手写序列化 —— 只有三个字段，
/// 为它引一个 JSON 库不值得，而且**这个文件必须在 agent 启动最早期就能读**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub from_version: String,
    pub to_version: String,
    pub started_at: i64,
}

impl Pending {
    pub fn encode(&self) -> String {
        format!(
            "{}\n{}\n{}\n",
            self.from_version, self.to_version, self.started_at
        )
    }

    /// 解析失败一律返回 `None` —— 状态文件坏了就当作「没有待定更新」，
    /// 这是安全的一侧：最坏是少做一次回滚，不会误删正在跑的二进制。
    pub fn decode(s: &str) -> Option<Self> {
        let mut it = s.lines();
        let from_version = it.next()?.trim().to_string();
        let to_version = it.next()?.trim().to_string();
        let started_at = it.next()?.trim().parse().ok()?;
        if from_version.is_empty() || to_version.is_empty() {
            return None;
        }
        Some(Pending {
            from_version,
            to_version,
            started_at,
        })
    }
}

/// 启动时该做什么。
#[derive(Debug, PartialEq, Eq)]
pub enum Boot {
    /// 没有待定更新，正常跑
    Normal,
    /// 我们是刚换上的新版本，处在试用期
    OnTrial(Pending),
    /// 状态文件说的版本和我们不符 —— 换二进制没成功，或回滚已经生效。
    /// 清掉状态文件即可。
    Stale(Pending),
}

/// 根据状态文件和当前版本判断启动动作。
pub fn classify(pending: Option<Pending>, current_version: &str) -> Boot {
    match pending {
        None => Boot::Normal,
        Some(p) if p.to_version == current_version => Boot::OnTrial(p),
        Some(p) => Boot::Stale(p),
    }
}

/// agent 自更新用到的三个路径。
pub struct Paths {
    /// 正在运行的二进制
    pub current: PathBuf,
    /// 上一个版本的备份，回滚时覆盖回 `current`
    pub backup: PathBuf,
    /// 待定状态文件
    pub state: PathBuf,
}

impl Paths {
    /// `exe` 是当前可执行文件路径；状态文件与备份都放在它旁边 ——
    /// systemd unit 里 `ReadWritePaths` 只开了这一个目录。
    pub fn beside(exe: &Path) -> Self {
        let dir = exe.parent().unwrap_or(Path::new("."));
        let name = exe.file_name().unwrap_or_default().to_string_lossy();
        Paths {
            current: exe.to_path_buf(),
            backup: dir.join(format!("{name}.old")),
            state: dir.join("update.json"),
        }
    }

    pub fn read_pending(&self) -> Option<Pending> {
        Pending::decode(&std::fs::read_to_string(&self.state).ok()?)
    }

    /// 提交：新版本已经连上了，删掉备份与状态文件。
    ///
    /// 删不掉只记日志不报错 —— 更新本身已经成功，
    /// 为了两个残留文件让 agent 起不来是本末倒置。
    pub fn commit(&self) {
        let _ = std::fs::remove_file(&self.backup);
        let _ = std::fs::remove_file(&self.state);
    }

    /// 回滚：把备份覆盖回去。
    ///
    /// 备份不存在时**不删状态文件**也不假装成功 —— 返回错误让调用方记日志，
    /// 否则会陷入「反复重启但永远回不去」且日志里什么都看不到的状态。
    pub fn rollback(&self) -> std::io::Result<()> {
        if !self.backup.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("找不到备份 {}，无法回滚", self.backup.display()),
            ));
        }
        std::fs::rename(&self.backup, &self.current)?;
        let _ = std::fs::remove_file(&self.state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(from: &str, to: &str) -> Pending {
        Pending {
            from_version: from.into(),
            to_version: to.into(),
            started_at: 1_700_000_000,
        }
    }

    #[test]
    fn pending_roundtrips() {
        let x = p("0.0.1", "0.0.2");
        assert_eq!(Pending::decode(&x.encode()), Some(x));
    }

    #[test]
    fn corrupt_state_reads_as_no_pending_update() {
        // 坏掉的状态文件必须当成「没有待定更新」——
        // 这一侧最坏是少做一次回滚，不会误删正在跑的二进制
        for bad in ["", "只有一行\n", "a\nb\n不是数字\n", "\n\n1\n"] {
            assert!(Pending::decode(bad).is_none(), "不该接受 {bad:?}");
        }
    }

    #[test]
    fn classify_covers_the_three_states() {
        assert_eq!(classify(None, "0.0.1"), Boot::Normal);
        assert_eq!(
            classify(Some(p("0.0.1", "0.0.2")), "0.0.2"),
            Boot::OnTrial(p("0.0.1", "0.0.2"))
        );
        // 换二进制没成功：状态文件说要升到 0.0.2，跑起来的还是 0.0.1
        assert_eq!(
            classify(Some(p("0.0.1", "0.0.2")), "0.0.1"),
            Boot::Stale(p("0.0.1", "0.0.2"))
        );
    }

    #[test]
    fn paths_are_all_beside_the_binary() {
        // systemd 的 ReadWritePaths 只开了这一个目录，写到别处会被拒
        let ps = Paths::beside(Path::new("/var/lib/pulse-agent/bin/pulse-agent"));
        assert_eq!(
            ps.backup,
            Path::new("/var/lib/pulse-agent/bin/pulse-agent.old")
        );
        assert_eq!(ps.state, Path::new("/var/lib/pulse-agent/bin/update.json"));
    }

    #[test]
    fn rollback_restores_the_backup() {
        let d = tempfile::tempdir().unwrap();
        let cur = d.path().join("pulse-agent");
        std::fs::write(&cur, b"new").unwrap();
        std::fs::write(d.path().join("pulse-agent.old"), b"old").unwrap();
        std::fs::write(d.path().join("update.json"), p("0.0.1", "0.0.2").encode()).unwrap();

        let ps = Paths::beside(&cur);
        ps.rollback().unwrap();
        assert_eq!(std::fs::read(&cur).unwrap(), b"old", "旧二进制应当被恢复");
        assert!(!ps.state.exists(), "状态文件应当被清掉");
        assert!(!ps.backup.exists(), "备份已经被 rename 走了");
    }

    #[test]
    fn rollback_without_backup_errors_loudly() {
        // 没有备份还假装回滚成功，会陷入「反复重启但永远回不去」且日志里什么都看不到
        let d = tempfile::tempdir().unwrap();
        let cur = d.path().join("pulse-agent");
        std::fs::write(&cur, b"new").unwrap();
        let ps = Paths::beside(&cur);
        let e = ps.rollback().unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(std::fs::read(&cur).unwrap(), b"new", "不该动正在跑的二进制");
    }

    #[test]
    fn commit_removes_backup_and_state() {
        let d = tempfile::tempdir().unwrap();
        let cur = d.path().join("pulse-agent");
        std::fs::write(&cur, b"new").unwrap();
        std::fs::write(d.path().join("pulse-agent.old"), b"old").unwrap();
        std::fs::write(d.path().join("update.json"), p("0.0.1", "0.0.2").encode()).unwrap();

        let ps = Paths::beside(&cur);
        ps.commit();
        assert!(!ps.backup.exists());
        assert!(!ps.state.exists());
        assert_eq!(std::fs::read(&cur).unwrap(), b"new", "新二进制必须留着");
    }
}
