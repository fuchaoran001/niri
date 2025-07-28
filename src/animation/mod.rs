// animation/mod.rs
// 此文件是动画系统的核心模块，定义了动画类型、曲线和计算逻辑。
// 在合成器中，动画用于平滑过渡效果（如窗口移动、工作区切换），提升用户体验。

use std::time::Duration;  // Rust标准库中的时间类型，表示持续时间

// 导入关键帧动画库的功能
use keyframe::functions::{EaseOutCubic, EaseOutQuad};  // 缓动函数：三次缓出和二次缓出
use keyframe::EasingFunction;  // 缓动函数trait

mod spring;  // 定义弹簧动画的子模块
pub use spring::{Spring, SpringParams};  // 公开导出弹簧动画结构体和参数

mod clock;  // 定义动画时钟的子模块
pub use clock::Clock;  // 公开导出时钟结构体

// 动画主结构体
// 合成器中的动画实例，管理从起始值到目标值的过渡过程
#[derive(Debug, Clone)]  // 自动实现Debug和Clone trait
pub struct Animation {
    from: f64,  // 动画起始值
    to: f64,  // 动画目标值
    initial_velocity: f64,  // 初始速度（用于物理动画）
    is_off: bool,  // 是否禁用动画（立即跳转）
    duration: Duration,  // 动画总持续时间
    /// Time until the animation first reaches `to`.
    /// 首次到达目标值的时间（近似值）
    ///
    /// Best effort; not always exactly precise.
    /// 尽力精确，但不保证完全准确
    clamped_duration: Duration,  // 首次达到目标值的预估时间
    start_time: Duration,  // 动画开始时间点
    clock: Clock,  // 时间源（用于获取当前时间）
    kind: Kind,  // 动画类型（缓动/弹簧/减速）
}

// 动画类型枚举
// 决定动画的数学计算模型
#[derive(Debug, Clone, Copy)]  // 自动实现Debug、Clone和Copy trait
enum Kind {
    Easing {  // 缓动函数动画
        curve: Curve,  // 使用的缓动曲线
    },
    Spring(Spring),  // 弹簧物理动画
    Deceleration {  // 减速动画（如惯性滚动）
        initial_velocity: f64,  // 初始速度
        deceleration_rate: f64,  // 减速率
    },
}

// 公开的缓动曲线枚举
// 定义动画的进度-时间关系
#[derive(Debug, Clone, Copy)]
pub enum Curve {
    Linear,  // 线性变化
    EaseOutQuad,  // 二次缓出（先快后慢）
    EaseOutCubic,  // 三次缓出（更平滑的减速）
    EaseOutExpo,  // 指数缓出（末端急停）
}

impl Animation {
    // 创建新动画
    // 参数说明:
    //   clock: 时间源
    //   from/to: 起始/目标值
    //   initial_velocity: 初始速度（通常来自手势）
    //   config: 动画配置（是否禁用/类型参数）
    pub fn new(
        clock: Clock,
        from: f64,
        to: f64,
        initial_velocity: f64,
        config: niri_config::Animation,  // 来自配置系统的动画参数
    ) -> Self {
        // 根据刷新率调整速度，确保触摸板手势感觉一致
        // Rust概念：f64浮点运算，除法
        let initial_velocity = initial_velocity / clock.rate().max(0.001);

        // 默认创建三次缓出动画（后续根据配置覆盖）
        // Rust概念：变量遮蔽（重新绑定同名变量）
        let mut rv = Self::ease(clock, from, to, initial_velocity, 0, Curve::EaseOutCubic);
        // 检查配置是否禁用动画
        if config.off {
            rv.is_off = true;
            return rv;
        }

        // 应用实际配置
        rv.replace_config(config);
        rv
    }

    // 动态更新动画配置
    // 允许运行时改变动画类型（如从缓动切换到弹簧）
    pub fn replace_config(&mut self, config: niri_config::Animation) {
        self.is_off = config.off;
        if config.off {
            // 禁用时设持续时间为零（立即完成）
            self.duration = Duration::ZERO;
            self.clamped_duration = Duration::ZERO;
            return;
        }

        // 保留原始开始时间（避免动画跳变）
        let start_time = self.start_time;

        match config.kind {
            niri_config::AnimationKind::Spring(p) => {
                // 创建弹簧参数
                // 合成器概念：阻尼比/刚度/精度阈值控制弹簧行为
                let params = SpringParams::new(p.damping_ratio, f64::from(p.stiffness), p.epsilon);

                // 构建新弹簧动画实例
                let spring = Spring {
                    from: self.from,
                    to: self.to,
                    initial_velocity: self.initial_velocity,
                    params,
                };
                // 替换当前动画
                *self = Self::spring(self.clock.clone(), spring);
            }
            niri_config::AnimationKind::Easing(p) => {
                // 创建缓动动画
                *self = Self::ease(
                    self.clock.clone(),
                    self.from,
                    self.to,
                    self.initial_velocity,
                    u64::from(p.duration_ms),  // 配置中的毫秒时长
                    Curve::from(p.curve),  // 配置中的曲线类型
                );
            }
        }

        // 恢复开始时间
        self.start_time = start_time;
    }

    /// Restarts the animation using the previous config.
    /// 使用相同配置重启动画（可改变起始/目标值）
    pub fn restarted(&self, from: f64, to: f64, initial_velocity: f64) -> Self {
        // 禁用时直接返回副本（无动画）
        if self.is_off {
            return self.clone();  // Rust概念：克隆语义（深拷贝）
        }

        // 速度调整（同new方法）
        let initial_velocity = initial_velocity / self.clock.rate().max(0.001);

        // 根据当前动画类型创建新实例
        match self.kind {
            Kind::Easing { curve } => Self::ease(
                self.clock.clone(),
                from,
                to,
                initial_velocity,
                self.duration.as_millis() as u64,  // 保留原持续时间
                curve,  // 保留原曲线
            ),
            Kind::Spring(spring) => {
                // 创建新弹簧（保留参数，更新起止点）
                let spring = Spring {
                    from,
                    to,
                    initial_velocity: self.initial_velocity,
                    params: spring.params,
                };
                Self::spring(self.clock.clone(), spring)
            }
            Kind::Deceleration {
                initial_velocity,
                deceleration_rate,
            } => {
                let threshold = 0.001; // 速度阈值（低于此值视为停止）
                // 创建新减速动画
                Self::decelerate(
                    self.clock.clone(),
                    from,
                    initial_velocity,
                    deceleration_rate,
                    threshold,
                )
            }
        }
    }

    // 创建缓动动画的构造方法
    pub fn ease(
        clock: Clock,
        from: f64,
        to: f64,
        initial_velocity: f64,
        duration_ms: u64,  // 毫秒为单位的持续时间
        curve: Curve,  // 缓动曲线类型
    ) -> Self {
        let duration = Duration::from_millis(duration_ms);  // 转换为Duration
        let kind = Kind::Easing { curve };  // 设置动画类型

        Self {
            from,
            to,
            initial_velocity,
            is_off: false,
            duration,
            // 缓动动画不超调，首次到达时间等于总时间
            clamped_duration: duration,
            start_time: clock.now(),  // 记录开始时间点
            clock,
            kind,
        }
    }

    // 创建弹簧动画的构造方法
    pub fn spring(clock: Clock, spring: Spring) -> Self {
        let _span = tracy_client::span!("Animation::spring");  // 性能分析标记

        // 计算总时长和首次到达目标值的时间
        let duration = spring.duration();
        let clamped_duration = spring.clamped_duration().unwrap_or(duration);
        let kind = Kind::Spring(spring);  // 设置动画类型

        Self {
            from: spring.from,
            to: spring.to,
            initial_velocity: spring.initial_velocity,
            is_off: false,
            duration,
            clamped_duration,
            start_time: clock.now(),
            clock,
            kind,
        }
    }

    // 创建减速动画（用于惯性滚动）
    // 物理模型：速度随时间指数衰减
    pub fn decelerate(
        clock: Clock,
        from: f64,
        initial_velocity: f64,
        deceleration_rate: f64,  // 减速率（>1的值）
        threshold: f64,  // 停止阈值
    ) -> Self {
        // 计算动画持续时间（基于物理公式）
        let duration_s = if initial_velocity == 0. {
            0.  // 速度为0时立即完成
        } else {
            // 物理公式：t = ln(-c * threshold / |v0|) / c
            // 其中 c = 1000 * ln(deceleration_rate)
            let coeff = 1000. * deceleration_rate.ln();
            (-coeff * threshold / initial_velocity.abs()).ln() / coeff
        };
        let duration = Duration::from_secs_f64(duration_s);

        // 计算最终停止位置
        // 公式：to = from - v0 / (1000 * ln(deceleration_rate))
        let to = from - initial_velocity / (1000. * deceleration_rate.ln());

        let kind = Kind::Deceleration {
            initial_velocity,
            deceleration_rate,
        };

        Self {
            from,
            to,
            initial_velocity,
            is_off: false,
            duration,
            clamped_duration: duration,  // 减速动画首次到达即最终位置
            start_time: clock.now(),
            clock,
            kind,
        }
    }

    // 检查动画是否已完成
    pub fn is_done(&self) -> bool {
        // 特殊处理：时钟要求立即完成
        if self.clock.should_complete_instantly() {
            return true;
        }

        // 当前时间 >= 开始时间 + 总持续时间
        self.clock.now() >= self.start_time + self.duration
    }

    // 检查动画是否已首次到达目标值
    pub fn is_clamped_done(&self) -> bool {
        if self.clock.should_complete_instantly() {
            return true;
        }

        self.clock.now() >= self.start_time + self.clamped_duration
    }

    // 计算指定时间点的动画值
    pub fn value_at(&self, at: Duration) -> f64 {
        // 时间点早于开始时间：返回起始值
        if at <= self.start_time {
            return self.from;
        // 时间点晚于结束时间：返回目标值
        } else if self.start_time + self.duration <= at {
            return self.to;
        }

        // 特殊处理：立即完成要求
        if self.clock.should_complete_instantly() {
            return self.to;
        }

        // 计算已过去的时间
        let passed = at.saturating_sub(self.start_time);  // 使用饱和减法避免负数

        // 根据动画类型计算当前值
        match self.kind {
            Kind::Easing { curve } => {
                // 将时间转换为进度比例 [0, 1]
                let passed = passed.as_secs_f64();
                let total = self.duration.as_secs_f64();
                let x = (passed / total).clamp(0., 1.);  // 限制在0-1范围
                // 应用缓动曲线公式：value = curve(x) * (to - from) + from
                curve.y(x) * (self.to - self.from) + self.from
            }
            Kind::Spring(spring) => {
                // 委托给弹簧动画计算
                let value = spring.value_at(passed);

                // 数值稳定性保护：防止计算误差导致极端值
                let range = (self.to - self.from) * 10.;  // 允许10倍值域波动
                let a = self.from - range;
                let b = self.to + range;
                // 根据方向决定钳制范围
                if self.from <= self.to {
                    value.clamp(a, b)
                } else {
                    value.clamp(b, a)
                }
            }
            Kind::Deceleration {
                initial_velocity,
                deceleration_rate,
            } => {
                // 减速动画公式：position = from + (rate^(1000*t) - 1) * v0 / c
                // 其中 c = 1000 * ln(rate)
                let passed = passed.as_secs_f64();
                let coeff = 1000. * deceleration_rate.ln();
                self.from + (deceleration_rate.powf(1000. * passed) - 1.) / coeff * initial_velocity
            }
        }
    }

    // 获取当前时间的动画值（最常用方法）
    pub fn value(&self) -> f64 {
        self.value_at(self.clock.now())
    }

    /// Returns a value that stops at the target value after first reaching it.
    /// 返回首次到达目标值后保持目标值的动画值
    ///
    /// Best effort; not always exactly precise.
    /// 尽力精确，但不保证完全准确
    pub fn clamped_value(&self) -> f64 {
        if self.is_clamped_done() {
            return self.to;
        }

        self.value()
    }

    // Getter方法：目标值
    pub fn to(&self) -> f64 {
        self.to
    }

    // Getter方法：起始值
    pub fn from(&self) -> f64 {
        self.from
    }

    // Getter方法：开始时间
    pub fn start_time(&self) -> Duration {
        self.start_time
    }

    // 计算结束时间
    pub fn end_time(&self) -> Duration {
        self.start_time + self.duration
    }

    // 获取总持续时间
    pub fn duration(&self) -> Duration {
        self.duration
    }

    // 偏移动画的起止点（用于跟随窗口位置变化）
    pub fn offset(&mut self, offset: f64) {
        self.from += offset;
        self.to += offset;

        // 同步更新内部弹簧数据（如果存在）
        if let Kind::Spring(spring) = &mut self.kind {
            spring.from += offset;
            spring.to += offset;
        }
    }
}

impl Curve {
    // 计算缓动曲线的Y值（进度比例→值比例）
    // 输入x: [0,1] 输出y: [0,1]
    pub fn y(self, x: f64) -> f64 {
        match self {
            Curve::Linear => x,  // 线性：y = x
            Curve::EaseOutQuad => EaseOutQuad.y(x),  // 二次缓出：y = 1 - (1-x)^2
            Curve::EaseOutCubic => EaseOutCubic.y(x),  // 三次缓出：y = 1 - (1-x)^3
            Curve::EaseOutExpo => 1. - 2f64.powf(-10. * x),  // 指数缓出：y = 1 - 2^(-10x)
        }
    }
}

// 实现从配置枚举到曲线枚举的转换
impl From<niri_config::AnimationCurve> for Curve {
    fn from(value: niri_config::AnimationCurve) -> Self {
        match value {
            niri_config::AnimationCurve::Linear => Curve::Linear,
            niri_config::AnimationCurve::EaseOutQuad => Curve::EaseOutQuad,
            niri_config::AnimationCurve::EaseOutCubic => Curve::EaseOutCubic,
            niri_config::AnimationCurve::EaseOutExpo => Curve::EaseOutExpo,
        }
    }
}

/* 动画系统流程图
1. 初始化
   [用户操作] → 创建Animation实例 → 根据配置选择类型（缓动/弹簧/减速）

2. 每帧更新
   +---------------------+
   | 合成器主循环         |
   | 检查动画状态         | → 若动画未完成 → 调用animation.value()
   | 更新窗口位置/透明度 |    ↓
   +---------------------+    [应用动画值到Wayland表面]
           ↑                  ↓
           +------------------+
   
3. 值计算过程
   +------------------------+
   | value_at()             |
   | 1. 检查时间边界         |
   | 2. 计算已用时间         |
   | 3. 根据动画类型计算:     |
   |    - 缓动: 曲线映射     |
   |    - 弹簧: 物理公式     |
   |    - 减速: 指数衰减     |
   +------------------------+
*/