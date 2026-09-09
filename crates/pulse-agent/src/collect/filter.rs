//! 网卡与挂载点的白/黑名单过滤。
//!
//! 纯逻辑、平台无关、可完整单测 —— 这是「环境之难」的标准切法：
//! 把只有真机能验的部分（文件读不读得到）和能在本地验到底的部分（规则对不对）分开。

/// 极简 glob：只支持 `*`（任意多个字符）与 `?`（单个字符）。
///
/// 不引 glob crate 是因为网卡名匹配只需要这两个元字符，
/// 而多一个依赖就多一份供应链风险 —— 对一个刻意做小的探针不划算。
///
/// 用经典的双指针回溯算法，最坏 O(n·m) 但模式长度上限 64（协议侧强制），
/// 不存在灾难性回溯。
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    // star: 最近一个 `*` 在模式中的位置；mark: 它当前匹配到文本的哪里
    let (mut star, mut mark) = (usize::MAX, 0usize);

    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            // 回溯：让上一个 `*` 多吃一个字符
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    // 模式尾部剩下的 `*` 可以匹配空串
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

fn matches_any(patterns: &[String], text: &str) -> bool {
    patterns.iter().any(|p| glob_match(p, text))
}

/// 按白/黑名单筛选候选项。
///
/// 顺序是**先白名单后黑名单**：白名单非空时只保留命中的，
/// 然后无论如何都减掉黑名单命中的。两者同时给时黑名单优先级更高 ——
/// 这样「只要 eth0，但排除 eth0.100」这类需求能表达出来。
pub fn select<'a>(all: &'a [String], include: &[String], exclude: &[String]) -> Vec<&'a String> {
    all.iter()
        .filter(|x| include.is_empty() || matches_any(include, x))
        .filter(|x| !matches_any(exclude, x))
        .collect()
}

/// 值得统计的文件系统类型。
///
/// 只统计**真实的块设备文件系统**。tmpfs / overlay / squashfs 这些
/// 要么是内存、要么是同一份数据的另一个视图，算进去会让磁盘占用虚高。
// Windows 上走 sysinfo 的磁盘枚举，用不到这个白名单
#[cfg_attr(target_os = "windows", allow(dead_code))]
pub fn is_real_filesystem(fstype: &str) -> bool {
    matches!(
        fstype,
        "ext2"
            | "ext3"
            | "ext4"
            | "xfs"
            | "btrfs"
            | "zfs"
            | "f2fs"
            | "reiserfs"
            | "jfs"
            | "vfat"
            | "exfat"
            | "ntfs"
            | "ntfs3"
            | "apfs"
            | "hfs"
            | "ufs"
            | "bcachefs"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn glob_exact_and_wildcards() {
        assert!(glob_match("lo", "lo"));
        assert!(!glob_match("lo", "lo0"));
        assert!(glob_match("veth*", "veth1a2b3c"));
        assert!(glob_match("veth*", "veth"));
        assert!(!glob_match("veth*", "vet"));
        assert!(glob_match("br-*", "br-abc123"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*", ""));
        assert!(glob_match("eth?", "eth0"));
        assert!(!glob_match("eth?", "eth10"));
        assert!(glob_match("*eth*", "xxethyy"));
    }

    #[test]
    fn glob_handles_multiple_stars_without_blowing_up() {
        // 双指针算法不会灾难性回溯 —— 这类模式在正则实现里能跑到天荒地老
        assert!(glob_match("*a*a*a*a*b", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaab"));
        assert!(!glob_match("*a*a*a*a*b", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaac"));
    }

    #[test]
    fn glob_empty_cases() {
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
        assert!(!glob_match("x", ""));
    }

    #[test]
    fn select_with_no_rules_keeps_everything() {
        let all = s(&["eth0", "lo", "docker0"]);
        assert_eq!(select(&all, &[], &[]).len(), 3);
    }

    #[test]
    fn select_exclude_only() {
        let all = s(&["eth0", "lo", "docker0", "veth1a2b"]);
        let got = select(&all, &[], &s(&["lo", "docker*", "veth*"]));
        assert_eq!(got, vec!["eth0"]);
    }

    #[test]
    fn select_include_only() {
        let all = s(&["eth0", "eth1", "lo", "docker0"]);
        let got = select(&all, &s(&["eth*"]), &[]);
        assert_eq!(got, vec!["eth0", "eth1"]);
    }

    #[test]
    fn select_exclude_wins_over_include() {
        // 「只要 eth*，但排除 VLAN 子接口」必须能表达出来
        let all = s(&["eth0", "eth0.100", "eth1"]);
        let got = select(&all, &s(&["eth*"]), &s(&["eth?.*"]));
        assert_eq!(got, vec!["eth0", "eth1"]);
    }

    #[test]
    fn select_of_empty_input_is_empty() {
        assert!(select(&[], &s(&["eth*"]), &s(&["lo"])).is_empty());
    }

    #[test]
    fn default_exclude_removes_container_noise() {
        // Docker 主机的真实网卡列表：不排除的话流量会被 veth 重复计算好几遍
        let all = s(&[
            "lo",
            "eth0",
            "docker0",
            "veth1a2b3c",
            "br-9f8e7d",
            "wg0",
            "tun0",
            "ens5",
        ]);
        let excl: Vec<String> = pulse_proto::default_net_exclude()
            .iter()
            .map(|x| x.to_string())
            .collect();
        let got: Vec<_> = select(&all, &[], &excl).into_iter().cloned().collect();
        assert_eq!(got, vec!["eth0", "ens5"]);
    }

    #[test]
    fn filesystem_whitelist_excludes_virtual_ones() {
        assert!(is_real_filesystem("ext4"));
        assert!(is_real_filesystem("xfs"));
        assert!(is_real_filesystem("apfs"));
        // 这几个算进去会让磁盘占用虚高
        assert!(!is_real_filesystem("tmpfs"));
        assert!(!is_real_filesystem("overlay"));
        assert!(!is_real_filesystem("squashfs"));
        assert!(!is_real_filesystem("proc"));
        assert!(!is_real_filesystem("devtmpfs"));
        assert!(!is_real_filesystem("cgroup2"));
    }
}
