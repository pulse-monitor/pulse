//! 带抖动的指数退避。
//!
//! 抖动不是可选项：面板重启后如果 200 台 agent 用同一条退避曲线，
//! 它们会在同一时刻一起打回来，把刚起来的面板再打挂一次。

use rand::Rng;
use std::time::Duration;

const BASE: Duration = Duration::from_secs(1);
const MAX: Duration = Duration::from_secs(300);
const JITTER: std::ops::Range<f64> = 0.8..1.2;

#[derive(Debug, Default)]
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    pub fn new() -> Self {
        Self::default()
    }

    /// 连接成功后调用，把退避曲线拉回起点。
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// 下一次重连前应等待的时长：`min(1s × 2^n, 300s) × jitter`。
    pub fn next_delay(&mut self) -> Duration {
        // saturating：attempt 很大时移位会溢出，这里让它停在 MAX 而不是 panic 或绕回
        let exp = BASE.saturating_mul(1u32.checked_shl(self.attempt).unwrap_or(u32::MAX));
        let capped = exp.min(MAX);
        self.attempt = self.attempt.saturating_add(1);

        let factor = rand::rng().random_range(JITTER);
        capped.mul_f64(factor).min(MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_then_caps() {
        let mut b = Backoff::new();
        let mut last = Duration::ZERO;
        for _ in 0..40 {
            let d = b.next_delay();
            assert!(d <= MAX, "永远不能超过上限，实际 {d:?}");
            last = d;
        }
        // 迭代够多次后应该稳定在上限附近（抖动下界 0.8）
        assert!(last >= MAX.mul_f64(0.79), "应已达到上限区间，实际 {last:?}");
    }

    #[test]
    fn first_delay_is_around_base() {
        let mut b = Backoff::new();
        let d = b.next_delay();
        assert!(
            d >= BASE.mul_f64(0.8) && d <= BASE.mul_f64(1.2),
            "实际 {d:?}"
        );
    }

    #[test]
    fn reset_returns_to_start() {
        let mut b = Backoff::new();
        for _ in 0..10 {
            b.next_delay();
        }
        b.reset();
        let d = b.next_delay();
        assert!(d <= BASE.mul_f64(1.2), "reset 后应回到起点，实际 {d:?}");
    }

    #[test]
    fn never_panics_on_extreme_attempt_counts() {
        // 长时间断线会把 attempt 推得很大，移位溢出必须被吃掉
        let mut b = Backoff {
            attempt: u32::MAX - 1,
        };
        for _ in 0..5 {
            assert!(b.next_delay() <= MAX);
        }
    }
}
