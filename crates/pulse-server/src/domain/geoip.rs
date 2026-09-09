//! IP → 国家。**纯逻辑，不碰网络也不碰文件系统。**
//!
//! 数据来自 **DB-IP Lite**（经 ip-location-db 分发，CC-BY-4.0）。
//!
//! 一开始用的是五家 RIR 的 delegated 分配文件 —— 权威、无许可证限制。
//! **但它给的是 IP 段持有者的注册国，不是机房所在地**：真机实测
//! `31.22.111.30` 被判成 SC（塞舌尔，注册地），而这台机器实际在 US。
//! 很多 VPS 商注册在离岸地区，这个偏差对「这台机器在哪」这个问题是致命的。
//! DB-IP 是真正的地理定位库（精度到 /24），同一个 IP 判为 US。
//!
//! 选它的另外两个理由：
//!
//! - **可再分发**：CC-BY-4.0，注明来源即可。MaxMind GeoLite2 要账号且禁止再分发
//! - **IP 不出本机**：查库全在本地做。用第三方 API 等于把用户所有 VPS 的
//!   IP 挨个报给别人 —— 一个监控面板不该这么干
//!
//! 精度只到国家级，正好够用：面板要的是「这台机器在哪个国家」，
//! 用来点亮地图和显示国旗，不需要城市。用户手填过位置时以用户为准。
//!
//! 格式是 CSV：`起始IP,结束IP,国家码`，按起始 IP 升序。

use std::net::{Ipv4Addr, Ipv6Addr};

/// 一段连续的 IPv4，闭区间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V4Range {
    pub start: u32,
    pub end: u32,
    pub cc: [u8; 2],
}

/// 一段 IPv6 前缀。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V6Range {
    pub start: u128,
    pub end: u128,
    pub cc: [u8; 2],
}

/// 查表。构造时保证按 `start` 升序且互不重叠。
#[derive(Debug, Default, Clone)]
pub struct GeoTable {
    v4: Vec<V4Range>,
    v6: Vec<V6Range>,
}

impl GeoTable {
    pub fn len(&self) -> (usize, usize) {
        (self.v4.len(), self.v6.len())
    }

    pub fn is_empty(&self) -> bool {
        self.v4.is_empty() && self.v6.is_empty()
    }

    /// 解析若干个 RIR delegated 文件的内容，合并成一张表。
    ///
    /// 无法解析的行**直接跳过**，不报错：这些文件里混着注释、汇总行、
    /// asn 记录和偶发的格式变体，为其中一行失败而丢掉整张表是不划算的。
    /// 解析 DB-IP 的 CSV（`起始IP,结束IP,国家码`）。v4 与 v6 分两个文件。
    ///
    /// 解析不了的行**直接跳过**，不报错：为其中一行失败而丢掉整张表不划算。
    pub fn parse(files: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let mut v4 = Vec::new();
        let mut v6 = Vec::new();
        for f in files {
            for line in f.as_ref().lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let mut p = line.split(',');
                let (Some(a), Some(b), Some(cc)) = (p.next(), p.next(), p.next()) else {
                    continue;
                };
                let Some(cc) = parse_cc(cc) else { continue };
                // 同一份文件里不会混 v4 和 v6，但按能不能解析来分派最省事
                if let (Ok(a), Ok(b)) = (a.parse::<Ipv4Addr>(), b.parse::<Ipv4Addr>()) {
                    let (s, e) = (u32::from(a), u32::from(b));
                    if s <= e {
                        v4.push(V4Range {
                            start: s,
                            end: e,
                            cc,
                        });
                    }
                } else if let (Ok(a), Ok(b)) = (a.parse::<Ipv6Addr>(), b.parse::<Ipv6Addr>()) {
                    let (s, e) = (u128::from(a), u128::from(b));
                    if s <= e {
                        v6.push(V6Range {
                            start: s,
                            end: e,
                            cc,
                        });
                    }
                }
            }
        }
        v4.sort_unstable_by_key(|r| r.start);
        v6.sort_unstable_by_key(|r| r.start);
        Self {
            v4: merge_v4(v4),
            v6: merge_v6(v6),
        }
    }

    /// 查一个 IP 属于哪个国家。查不到返回 `None`（不猜）。
    pub fn lookup(&self, ip: std::net::IpAddr) -> Option<&str> {
        match ip {
            std::net::IpAddr::V4(a) => {
                let k = u32::from(a);
                let i = self.v4.partition_point(|r| r.start <= k).checked_sub(1)?;
                let r = &self.v4[i];
                (k <= r.end).then(|| cc_str(&r.cc))
            }
            std::net::IpAddr::V6(a) => {
                // IPv4 映射地址（::ffff:1.2.3.4）要按 IPv4 查，
                // 否则一台走 v4 但被 accept 成 v6 socket 的机器会查不到
                if let Some(v4) = a.to_ipv4_mapped() {
                    return self.lookup(std::net::IpAddr::V4(v4));
                }
                let k = u128::from(a);
                let i = self.v6.partition_point(|r| r.start <= k).checked_sub(1)?;
                let r = &self.v6[i];
                (k <= r.end).then(|| cc_str(&r.cc))
            }
        }
    }
}

/// 合并相邻且同国家的段，表能小一大截。
fn merge_v4(mut v: Vec<V4Range>) -> Vec<V4Range> {
    if v.is_empty() {
        return v;
    }
    let mut out: Vec<V4Range> = Vec::with_capacity(v.len());
    for r in v.drain(..) {
        match out.last_mut() {
            // 紧邻或重叠、且同国家 → 并进去
            Some(p) if p.cc == r.cc && r.start <= p.end.saturating_add(1) => {
                p.end = p.end.max(r.end);
            }
            _ => out.push(r),
        }
    }
    out
}

/// v6 版本的相邻合并。
fn merge_v6(mut v: Vec<V6Range>) -> Vec<V6Range> {
    if v.is_empty() {
        return v;
    }
    let mut out: Vec<V6Range> = Vec::with_capacity(v.len());
    for r in v.drain(..) {
        match out.last_mut() {
            Some(p) if p.cc == r.cc && r.start <= p.end.saturating_add(1) => {
                p.end = p.end.max(r.end);
            }
            _ => out.push(r),
        }
    }
    out
}

/// 两位大写字母才算合法国家码。`ZZ` 是占位符，不是国家。
fn parse_cc(s: &str) -> Option<[u8; 2]> {
    let b = s.as_bytes();
    if b.len() != 2 || !b.iter().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let cc = [b[0].to_ascii_uppercase(), b[1].to_ascii_uppercase()];
    (cc != *b"ZZ").then_some(cc)
}

fn cc_str(cc: &[u8; 2]) -> &str {
    // SAFETY 不需要 unsafe：parse_cc 保证了是 ASCII 字母
    std::str::from_utf8(cc).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    /// DB-IP 的格式：起始IP,结束IP,国家码
    const V4: &str = "\
1.0.0.0,1.0.0.255,AU
1.0.1.0,1.0.3.255,CN
3.0.0.0,3.255.255.255,US
5.9.0.0,5.9.255.255,DE
31.22.111.0,31.22.111.255,US
255.255.255.0,255.255.255.255,AU
";
    const V6: &str = "\
2001:200::,2001:200:ffff:ffff:ffff:ffff:ffff:ffff,JP
2600::,260f:ffff:ffff:ffff:ffff:ffff:ffff:ffff,US
";

    fn table() -> GeoTable {
        GeoTable::parse([V4, V6])
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn parses_and_looks_up_ipv4() {
        let t = table();
        assert_eq!(t.lookup(ip("1.0.0.5")), Some("AU"));
        assert_eq!(t.lookup(ip("1.0.1.200")), Some("CN"));
        assert_eq!(t.lookup(ip("3.255.255.255")), Some("US"));
        assert_eq!(t.lookup(ip("5.9.0.1")), Some("DE"));
    }

    #[test]
    fn parses_and_looks_up_ipv6() {
        let t = table();
        assert_eq!(t.lookup(ip("2001:200::1")), Some("JP"));
        assert_eq!(t.lookup(ip("2600:1f00::abcd")), Some("US"));
    }

    /// 真机上的那台 VPS。RIR 的分配文件把它判成 SC（持有者注册地），
    /// DB-IP 判成 US（机房所在地）—— 换数据源就是为了这个差别。
    #[test]
    fn resolves_the_real_vps_to_its_datacenter_country() {
        assert_eq!(table().lookup(ip("31.22.111.30")), Some("US"));
    }

    #[test]
    fn ipv4_mapped_v6_falls_back_to_the_v4_table() {
        // 监听 :: 的 socket 会把 v4 连接报成 ::ffff:a.b.c.d，
        // 不处理的话这类机器会全部查不到国家
        assert_eq!(table().lookup(ip("::ffff:5.9.0.1")), Some("DE"));
    }

    #[test]
    fn skips_garbage_without_panicking() {
        let t = GeoTable::parse([
            "# 注释\n坏行\n1.2.3.4\n1.0.0.0,1.0.0.255,ZZ\n1.1.0.0,1.1.0.255,US\n不是IP,也不是IP,US\n",
        ]);
        // ZZ 是占位符，不是国家
        assert_eq!(t.lookup(ip("1.0.0.1")), None);
        assert_eq!(t.lookup(ip("1.1.0.1")), Some("US"));
    }

    #[test]
    fn unknown_addresses_return_none_rather_than_guessing() {
        let t = table();
        assert_eq!(t.lookup(ip("9.9.9.9")), None);
        assert_eq!(t.lookup(ip("::1")), None);
    }

    #[test]
    fn merges_adjacent_same_country_ranges() {
        let t = GeoTable::parse(["1.0.1.0,1.0.1.255,CN\n1.0.2.0,1.0.3.255,CN\n"]);
        assert_eq!(t.len().0, 1, "相邻同国家的段应当合并");
        assert_eq!(t.lookup(ip("1.0.3.255")), Some("CN"));
    }

    #[test]
    fn empty_input_is_empty_not_a_panic() {
        let t = GeoTable::parse(Vec::<String>::new());
        assert!(t.is_empty());
        assert_eq!(t.lookup(ip("1.1.1.1")), None);
    }

    #[test]
    fn handles_the_top_of_the_address_space() {
        assert_eq!(table().lookup(ip("255.255.255.255")), Some("AU"));
    }

    #[test]
    fn rejects_reversed_ranges() {
        // 起点大于终点的行是坏数据，收下会让二分查找给出乱七八糟的结果
        let t = GeoTable::parse(["9.0.0.0,1.0.0.0,US\n"]);
        assert!(t.is_empty());
    }
}
