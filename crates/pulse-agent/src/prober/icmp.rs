//! 非特权 ICMP 探测。
//!
//! 用 `socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP)` —— 这是**不需要
//! `CAP_NET_RAW`** 的那种 ICMP socket，只要内核的 `net.ipv4.ping_group_range`
//! 覆盖运行用户的 gid 即可（Darwin 上默认可用）。
//!
//! 调用方必须先确认 `Capabilities::icmp_unprivileged`；不可用时上层会回落 TCP。

use std::time::Duration;

#[cfg(unix)]
pub async fn probe(host: &str, timeout: Duration) -> Option<u32> {
    let host = host.to_string();
    // 这是同步的阻塞式收发，放到阻塞线程池里跑，别卡住 runtime
    tokio::task::spawn_blocking(move || probe_blocking(&host, timeout))
        .await
        .ok()
        .flatten()
}

#[cfg(not(unix))]
pub async fn probe(_host: &str, _timeout: Duration) -> Option<u32> {
    // Windows 需要 IcmpSendEcho2，暂未实现 —— capabilities 会报 false，
    // 上层因此回落 TCP，不会走到这里
    None
}

#[cfg(unix)]
fn probe_blocking(host: &str, timeout: Duration) -> Option<u32> {
    use std::net::{IpAddr, ToSocketAddrs};
    use std::time::Instant;

    // 只解析 IPv4：ICMPv6 是另一套协议号与报文格式，暂不支持
    let addr = (host, 0u16)
        .to_socket_addrs()
        .ok()?
        .find_map(|a| match a.ip() {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        })?;

    // SAFETY: 参数是 libc 常量，失败时返回 -1，下面立刻检查
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_ICMP) };
    if fd < 0 {
        return None; // 内核不允许，上层应当已经通过 capabilities 避开这里
    }
    let guard = FdGuard(fd);

    // 用 `as _` 让编译器推导字段类型，不要写死 libc::time_t /suseconds_t ——
    // 那两个别名在 musl 上已标记废弃（正在从 32 位改成 64 位），
    // 写死会在 musl 目标上直接编译失败。timeout 已夹紧到 ≤10 秒，不存在溢出。
    let tv = libc::timeval {
        tv_sec: timeout.as_secs() as _,
        tv_usec: timeout.subsec_micros() as _,
    };
    // SAFETY: fd 有效，tv 是合法的 timeval，长度与类型匹配
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            std::ptr::addr_of!(tv).cast(),
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        );
    }

    // ICMP Echo Request：type=8, code=0, checksum, id, seq
    // SOCK_DGRAM 模式下内核会改写 id 并重算校验和，但我们仍按规范填好
    let seq: u16 = 1;
    let mut pkt = [0u8; 16];
    pkt[0] = 8;
    pkt[6..8].copy_from_slice(&seq.to_be_bytes());
    let ck = checksum(&pkt);
    pkt[2..4].copy_from_slice(&ck.to_be_bytes());

    // SAFETY: sockaddr_in 是纯 POD（全整数字段），全零是合法初始状态，
    // 随后由下面三行完整填好
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    sa.sin_family = libc::AF_INET as libc::sa_family_t;
    sa.sin_addr.s_addr = u32::from_ne_bytes(addr.octets());

    let t0 = Instant::now();
    // SAFETY: pkt 与 sa 都是本函数的栈变量，长度如实传入
    let sent = unsafe {
        libc::sendto(
            fd,
            pkt.as_ptr().cast(),
            pkt.len(),
            0,
            std::ptr::addr_of!(sa).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if sent < 0 {
        return None;
    }

    let mut buf = [0u8; 128];
    // SAFETY: buf 是本函数的栈缓冲，长度如实传入
    let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
    drop(guard);
    if n <= 0 {
        return None; // 超时 = 丢包
    }

    // BSD 系的 SOCK_DGRAM ICMP 会把 IP 头一起给出来，Linux 则已剥掉。
    // 看首字节是不是 IPv4 头（version=4）来决定跳过多少。
    let body = if buf[0] >> 4 == 4 {
        let ihl = usize::from(buf[0] & 0x0f) * 4;
        buf.get(ihl..n as usize)?
    } else {
        &buf[..n as usize]
    };
    // Echo Reply 的 type=0
    if body.first() != Some(&0) {
        return None;
    }
    Some(t0.elapsed().as_micros().min(u128::from(u32::MAX)) as u32)
}

/// 关闭 fd 的守卫。中途 return 时也不会漏。
#[cfg(unix)]
struct FdGuard(libc::c_int);

#[cfg(unix)]
impl Drop for FdGuard {
    fn drop(&mut self) {
        // SAFETY: fd 由本模块创建且只关一次
        unsafe { libc::close(self.0) };
    }
}

/// RFC 1071 的反码求和校验。
#[cfg(unix)]
fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u32::from(u16::from_be_bytes([data[i], data[i + 1]]));
        i += 2;
    }
    if i < data.len() {
        sum += u32::from(data[i]) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn checksum_matches_rfc1071_example() {
        // 一个已知的 Echo Request：type=8 code=0 ck=0 id=0 seq=1 + 8 字节 0
        let mut pkt = [0u8; 16];
        pkt[0] = 8;
        pkt[7] = 1;
        let ck = checksum(&pkt);
        // 把校验和填回去后，整包再算一次必须得 0 —— 这是校验和的定义
        pkt[2..4].copy_from_slice(&ck.to_be_bytes());
        assert_eq!(checksum(&pkt), 0, "填回校验和后重算应为 0");
    }

    #[test]
    fn checksum_handles_odd_length() {
        assert_ne!(checksum(&[1, 2, 3]), 0);
    }

    #[tokio::test]
    async fn probe_of_invalid_host_is_none_not_panic() {
        assert!(probe("not a host\u{0}", Duration::from_millis(300))
            .await
            .is_none());
    }
}
