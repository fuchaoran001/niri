// clock.rs
// 此文件定义了可调整速率的时钟系统，用于控制动画时间流。
// 在合成器中，时钟允许全局控制动画速度（如慢动作调试）和即时完成动画（用于测试和配置）。

use std::cell::RefCell;  // Rust内部可变性容器：允许在不可变引用下修改内部数据
use std::rc::Rc;  // 引用计数智能指针：实现多所有权共享
use std::time::Duration;  // 时间间隔类型

use crate::utils::get_monotonic_time;  // 获取单调递增的系统时间（不受系统时间修改影响）

/// Shareable lazy clock that can change rate.
/// 可共享的惰性时钟，支持调整速率
///
/// The clock will fetch the time once and then retain it until explicitly cleared with
/// [`Clock::clear`].
/// 时钟会获取一次时间并缓存，直到显式调用clear清除
#[derive(Debug, Default, Clone)]  // 自动实现Debug、Clone和Default trait
pub struct Clock {
    inner: Rc<RefCell<AdjustableClock>>,  // 内部时钟（通过Rc+RefCell实现共享可变性）
}

// 核心数据结构
#[derive(Debug, Default)]  // 默认实现：time=None
struct LazyClock {
    time: Option<Duration>,  // 可选的时间值（None表示需要重新获取）
}

/// Clock that can adjust its rate.
/// 支持速率调整的时钟
#[derive(Debug)]
struct AdjustableClock {
    inner: LazyClock,          // 底层惰性时钟
    current_time: Duration,     // 当前调整后的时间（考虑速率）
    last_seen_time: Duration,   // 上次看到的原始时间（用于计算差值）
    rate: f64,                  // 时间流速倍数（1.0=正常，0.5=半速，2.0=二倍速）
    complete_instantly: bool,   // 是否立即完成所有动画（用于禁用动画）
}

impl Clock {
    /// Creates a new clock with the given time.
    /// 用指定时间创建新时钟（常用于测试）
    pub fn with_time(time: Duration) -> Self {
        let clock = AdjustableClock::new(LazyClock::with_time(time));
        Self {
            inner: Rc::new(RefCell::new(clock)),  // 包裹在Rc+RefCell中
        }
    }

    /// Returns the current time.
    /// 获取当前时间（已应用速率调整）
    pub fn now(&self) -> Duration {
        // 借用RefCell的可变引用（运行时检查借用规则）
        self.inner.borrow_mut().now()
    }

    /// Returns the underlying time not adjusted for rate change.
    /// 获取未调整速率的原始时间
    pub fn now_unadjusted(&self) -> Duration {
        self.inner.borrow_mut().inner.now()
    }

    /// Sets the unadjusted clock time.
    /// 设置原始时间（用于模拟时间流逝）
    pub fn set_unadjusted(&mut self, time: Duration) {
        self.inner.borrow_mut().inner.set(time);
    }

    /// Clears the stored time so it's re-fetched again next.
    /// 清除缓存的时间（下次访问时重新获取系统时间）
    pub fn clear(&mut self) {
        self.inner.borrow_mut().inner.clear();
    }

    /// Gets the clock rate.
    /// 获取当前速率
    pub fn rate(&self) -> f64 {
        self.inner.borrow().rate()  // 不可变借用
    }

    /// Sets the clock rate.
    /// 设置时间流速（0.0-1000.0）
    pub fn set_rate(&mut self, rate: f64) {
        self.inner.borrow_mut().set_rate(rate);
    }

    /// Returns whether animations should complete instantly.
    /// 检查是否应即时完成动画（用于全局禁用动画）
    pub fn should_complete_instantly(&self) -> bool {
        self.inner.borrow().should_complete_instantly()
    }

    /// Sets whether animations should complete instantly.
    /// 设置即时完成动画标志
    pub fn set_complete_instantly(&mut self, value: bool) {
        self.inner.borrow_mut().set_complete_instantly(value);
    }
}

// 实现相等比较（基于Rc指针相等）
impl PartialEq for Clock {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)  // 比较是否同一指针
    }
}

impl Eq for Clock {}  // 标记trait（等价关系）

impl LazyClock {
    // 创建带初始时间的惰性时钟
    pub fn with_time(time: Duration) -> Self {
        Self { time: Some(time) }
    }

    // 清除缓存时间
    pub fn clear(&mut self) {
        self.time = None;
    }

    // 设置时间（覆盖缓存）
    pub fn set(&mut self, time: Duration) {
        self.time = Some(time);
    }

    // 获取当前时间（惰性获取）
    pub fn now(&mut self) -> Duration {
        // 如果无缓存则获取系统时间
        *self.time.get_or_insert_with(get_monotonic_time)
    }
}

impl AdjustableClock {
    // 构造可调速率时钟
    pub fn new(mut inner: LazyClock) -> Self {
        let time = inner.now();  // 初始化时获取时间
        Self {
            inner,
            current_time: time,     // 调整后时间初始值
            last_seen_time: time,   // 原始时间初始值
            rate: 1.,                // 默认正常速率
            complete_instantly: false, // 默认不禁用动画
        }
    }

    // 获取当前速率
    pub fn rate(&self) -> f64 {
        self.rate
    }

    // 设置速率（钳制在0-1000范围）
    pub fn set_rate(&mut self, rate: f64) {
        self.rate = rate.clamp(0., 1000.);
    }

    // 检查是否应即时完成动画
    pub fn should_complete_instantly(&self) -> bool {
        self.complete_instantly
    }

    // 设置即时完成标志
    pub fn set_complete_instantly(&mut self, value: bool) {
        self.complete_instantly = value;
    }

    // 计算当前时间（核心逻辑）
    pub fn now(&mut self) -> Duration {
        let time = self.inner.now();  // 获取当前原始时间

        // 时间未变化时返回缓存值（优化性能）
        if self.last_seen_time == time {
            return self.current_time;
        }

        /* 时间计算流程图：
           +-----------------------+
           | 获取新原始时间 (time)  |
           +----------+------------+
                      |
           +----------v----------+
           | 与上次时间比较        |
           | - 新时间 > 上次: 正向流逝 |
           | - 新时间 < 上次: 时间回退 |
           +----------+----------+
                      |
           +----------v----------+
           | 计算原始时间差 (delta) |
           +----------+----------+
                      |
           +----------v----------+
           | 应用速率:            |
           |   adjusted_delta =  |
           |      delta * rate   |
           +----------+----------+
                      |
           +----------v----------+
           | 更新当前时间:        |
           | - 正向: 当前时间 + 调整差 |
           | - 反向: 当前时间 - 调整差 |
           +----------+----------+
                      |
           +----------v----------+
           | 更新上次记录时间为当前  |
           +---------------------+
        */
        
        // 处理时间前进
        if self.last_seen_time < time {
            let delta = time - self.last_seen_time;  // 计算原始时间差
            let delta = delta.mul_f64(self.rate);    // 应用速率调整
            self.current_time = self.current_time.saturating_add(delta);  // 避免溢出
        } else {
            // 处理时间回退（罕见情况）
            let delta = self.last_seen_time - time;
            let delta = delta.mul_f64(self.rate);
            self.current_time = self.current_time.saturating_sub(delta);  // 避免下溢
        }

        // 更新记录的时间点
        self.last_seen_time = time;
        self.current_time  // 返回调整后的时间
    }
}

// 默认实现（使用默认的LazyClock）
impl Default for AdjustableClock {
    fn default() -> Self {
        Self::new(LazyClock::default())  // time=None
    }
}

// 单元测试模块
#[cfg(test)]  // 条件编译：仅在测试时包含
mod tests {
    use super::*;  // 导入父模块所有内容

    // 测试固定时钟
    #[test]
    fn frozen_clock() {
        let mut clock = Clock::with_time(Duration::ZERO);  // 创建0时刻时钟
        assert_eq!(clock.now(), Duration::ZERO);  // 验证初始时间

        // 模拟时间流逝到100ms
        clock.set_unadjusted(Duration::from_millis(100));
        assert_eq!(clock.now(), Duration::from_millis(100));

        // 模拟时间流逝到200ms
        clock.set_unadjusted(Duration::from_millis(200));
        assert_eq!(clock.now(), Duration::from_millis(200));
    }

    // 测试速率调整
    #[test]
    fn rate_change() {
        let mut clock = Clock::with_time(Duration::ZERO);
        clock.set_rate(0.5);  // 设置半速

        // 前进100ms → 实际前进50ms
        clock.set_unadjusted(Duration::from_millis(100));
        assert_eq!(clock.now_unadjusted(), Duration::from_millis(100));
        assert_eq!(clock.now(), Duration::from_millis(50));

        // 再前进100ms → 累计前进100ms
        clock.set_unadjusted(Duration::from_millis(200));
        assert_eq!(clock.now_unadjusted(), Duration::from_millis(200));
        assert_eq!(clock.now(), Duration::from_millis(100));

        // 时间回退到150ms → 回退50ms原始时间 → 应用半速回退25ms
        clock.set_unadjusted(Duration::from_millis(150));
        assert_eq!(clock.now_unadjusted(), Duration::from_millis(150));
        assert_eq!(clock.now(), Duration::from_millis(75));

        // 切换到二倍速
        clock.set_rate(2.0);

        // 前进到250ms → 原始前进100ms → 二倍速前进200ms
        clock.set_unadjusted(Duration::from_millis(250));
        assert_eq!(clock.now_unadjusted(), Duration::from_millis(250));
        assert_eq!(clock.now(), Duration::from_millis(275));  // 75 + 200 = 275
    }
}

/* 时钟系统工作原理
1. 时间获取
   +---------------------+
   | 调用clock.now()     |
   | 触发内部更新逻辑      |
   +----------+----------+
              |
   +----------v----------+
   | LazyClock:          |
   | - 有缓存? 用缓存     |
   | - 无缓存? 获取系统时间 |
   +----------+----------+
              |
   +----------v----------+
   | AdjustableClock:     |
   | 1. 计算原始时间差     |
   | 2. 乘以当前速率       |
   | 3. 更新调整后时间     |
   +---------------------+

2. 速率调整应用
   - 速率=0.5: 所有时间间隔减半 → 动画变慢
   - 速率=2.0: 所有时间间隔加倍 → 动画变快
   - 速率=0.0: 时间停止 → 动画冻结

3. 即时完成模式
   - 当complete_instantly=true时
   - 动画系统会跳过计算直接返回最终值
   - 用于用户禁用动画的场景
*/