// spring.rs
// 此文件实现了基于物理的弹簧动画模型，用于模拟自然弹性运动
// 在合成器中，弹簧动画用于窗口移动、工作区切换等需要自然过渡效果的场景

use std::time::Duration;  // 时间间隔类型

// 弹簧物理参数
#[derive(Debug, Clone, Copy)]  // 自动实现Debug、Clone和Copy trait
pub struct SpringParams {
    pub damping: f64,      // 阻尼系数（牛顿·秒/米）
    pub mass: f64,         // 质量（千克）
    pub stiffness: f64,    // 刚度系数（牛顿/米）
    pub epsilon: f64,      // 精度阈值（当位移小于此值时视为静止）
}

// 弹簧动画实例
#[derive(Debug, Clone, Copy)]
pub struct Spring {
    pub from: f64,               // 起始位置
    pub to: f64,                 // 目标位置
    pub initial_velocity: f64,   // 初始速度
    pub params: SpringParams,    // 物理参数
}

impl SpringParams {
    // 创建新的弹簧参数
    // 参数说明:
    //   damping_ratio: 阻尼比（临界阻尼=1.0，<1欠阻尼，>1过阻尼）
    //   stiffness: 刚度系数
    //   epsilon: 停止阈值
    pub fn new(damping_ratio: f64, stiffness: f64, epsilon: f64) -> Self {
        // 确保参数有效（非负）
        let damping_ratio = damping_ratio.max(0.);
        let stiffness = stiffness.max(0.);
        let epsilon = epsilon.max(0.);

        // 固定质量为1（简化计算）
        let mass = 1.;
        // 计算临界阻尼（2*sqrt(mass*stiffness)）
        let critical_damping = 2. * (mass * stiffness).sqrt();
        // 实际阻尼 = 阻尼比 * 临界阻尼
        let damping = damping_ratio * critical_damping;

        Self {
            damping,
            mass,
            stiffness,
            epsilon,
        }
    }
}

impl Spring {
    // 计算给定时间点的弹簧位置
    pub fn value_at(&self, t: Duration) -> f64 {
        self.oscillate(t.as_secs_f64())  // 将时间转换为秒并计算
    }

    // 基于libadwaita (LGPL-2.1-or-later)和RBBAnimation (MIT)实现
    // 计算弹簧完全静止所需的时间
    pub fn duration(&self) -> Duration {
        const DELTA: f64 = 0.001;  // 微小变化量（用于数值计算）

        // 计算阻尼系数β = damping/(2*mass)
        let beta = self.params.damping / (2. * self.params.mass);

        // 处理无效阻尼情况（无阻尼或负阻尼）
        if beta.abs() <= f64::EPSILON || beta < 0. {
            return Duration::MAX;  // 永不停止
        }

        // 起始点和目标点相同：立即完成
        if (self.to - self.from).abs() <= f64::EPSILON {
            return Duration::ZERO;
        }

        // 计算固有频率ω₀ = sqrt(stiffness/mass)
        let omega0 = (self.params.stiffness / self.params.mass).sqrt();

        // 初始估计：当包络函数衰减到epsilon时的时间
        // 公式：t = -ln(epsilon)/β
        let mut x0 = -self.params.epsilon.ln() / beta;

        // 临界阻尼或欠阻尼情况：直接使用包络时间估计
        // 使用f32::EPSILON作为比较阈值（数值稳定性考虑）
        if (beta - omega0).abs() <= f64::from(f32::EPSILON) || beta < omega0 {
            return Duration::from_secs_f64(x0);
        }

        /* 过阻尼情况下的牛顿迭代法流程图：
          +-----------------------------------+
          | 初始化:                            |
          |   x0 = -ln(epsilon)/β (包络时间)  |
          |   y0 = spring(x0)                |
          +-----------------+-----------------+
                            |
          +-----------------v-----------------+
          | 计算斜率:                        |
          |   m = [spring(x0+Δ) - y0]/Δ      |
          +-----------------+-----------------+
                            |
          +-----------------v-----------------+
          | 计算新估计值:                     |
          |   x1 = (target - y0 + m*x0)/m    |
          |   y1 = spring(x1)               |
          +-----------------+-----------------+
                            |
          +-----------------v-----------------+
          | 检查收敛: |y1 - target| < epsilon?|
          | 是 → 返回x1                       |
          | 否 → 设x0=x1, y0=y1 并重复       |
          +----------------------------------+
        */

        // 过阻尼情况：使用牛顿法迭代求解
        let mut y0 = self.oscillate(x0);  // 计算初始位置
        let m = (self.oscillate(x0 + DELTA) - y0) / DELTA;  // 数值计算斜率

        // 牛顿法公式: x1 = x0 - f(x0)/f'(x0)
        // 但这里我们求解 spring(t) = to 的根
        let mut x1 = (self.to - y0 + m * x0) / m;
        let mut y1 = self.oscillate(x1);

        // 迭代计数器（防止无限循环）
        let mut i = 0;
        while (self.to - y1).abs() > self.params.epsilon {
            // 安全限制：最多1000次迭代
            if i > 1000 {
                return Duration::ZERO;
            }

            // 准备下一轮迭代
            x0 = x1;
            y0 = y1;

            // 重新计算斜率
            let m = (self.oscillate(x0 + DELTA) - y0) / DELTA;

            // 牛顿法更新
            x1 = (self.to - y0 + m * x0) / m;
            y1 = self.oscillate(x1);

            // 数值不稳定检查
            if !y1.is_finite() {
                return Duration::from_secs_f64(x0);
            }

            i += 1;
        }

        // 返回收敛的时间
        Duration::from_secs_f64(x1)
    }

    /// Computes and returns the duration until the spring reaches its target position.
    /// 计算弹簧首次到达目标位置的时间（近似值）
    pub fn clamped_duration(&self) -> Option<Duration> {
        // 计算阻尼系数β
        let beta = self.params.damping / (2. * self.params.mass);

        // 处理无效情况
        if beta.abs() <= f64::EPSILON || beta < 0. {
            return Some(Duration::MAX);
        }

        // 起始点目标点相同：立即完成
        if (self.to - self.from).abs() <= f64::EPSILON {
            return Some(Duration::ZERO);
        }

        /* 逐步逼近算法：
          +----------------------------------+
          | 初始化:                          |
          |   i = 1 (从1ms开始)              |
          |   y = spring(i/1000)            |
          +---------------+------------------+
                          |
          +---------------v------------------+
          | 循环直到满足条件:                 |
          |   if to > from: y > to - epsilon |
          |   else:        y < to + epsilon  |
          +---------------+------------------+
                          |
          +---------------v------------------+
          |   i += 1                        |
          |   y = spring(i/1000)           |
          +---------------+------------------+
                          |
          +---------------v------------------+
          | 超时检查: i > 3000? → 返回None   |
          +----------------------------------+
        */
        
        // 从1ms开始逐步检查（跳过t=0避免初始位置）
        let mut i = 1u16;
        let mut y = self.oscillate(f64::from(i) / 1000.);

        // 检查是否进入目标区域（考虑方向）
        while (self.to > self.from && y < self.to - self.params.epsilon)
            || (self.to < self.from && y > self.to + self.params.epsilon)
        {
            // 安全限制：最多3000ms
            if i > 3000 {
                return None;  // 无法在合理时间内收敛
            }

            i += 1;
            y = self.oscillate(f64::from(i) / 1000.);
        }

        // 返回找到的时间点
        Some(Duration::from_millis(u64::from(i)))
    }

    /// Returns the spring position at a given time in seconds.
    /// 核心函数：计算给定时间（秒）的弹簧位置
    fn oscillate(&self, t: f64) -> f64 {
        // 提取参数（简化公式）
        let b = self.params.damping;
        let m = self.params.mass;
        let k = self.params.stiffness;
        let v0 = self.initial_velocity;

        // 计算阻尼系数β和固有频率ω₀
        let beta = b / (2. * m);
        let omega0 = (k / m).sqrt();

        // 初始位移（相对于目标位置）
        let x0 = self.from - self.to;

        // 包络函数：e^(-βt)
        let envelope = (-beta * t).exp();

        /* 三种阻尼状态下的运动方程：
          ┌───────────────────────┬───────────────────────────────┐
          │ 阻尼类型              │ 方程                          │
          ├───────────────────────┼───────────────────────────────┤
          │ 临界阻尼 (β = ω₀)     │ y = to + e^(-βt)[x0 + (βx0+v0)t] │
          │ 欠阻尼 (β < ω₀)      │ y = to + e^(-βt)[x0·cos(ω₁t) + ((βx0+v0)/ω₁)·sin(ω₁t)] │
          │ 过阻尼 (β > ω₀)      │ y = to + e^(-βt)[x0·cosh(ω₂t) + ((βx0+v0)/ω₂)·sinh(ω₂t)] │
          └───────────────────────┴───────────────────────────────┘
          其中：
            ω₁ = sqrt(ω₀² - β²)  [欠阻尼振荡频率]
            ω₂ = sqrt(β² - ω₀²)  [过阻尼衰减参数]
        */
        
        // 使用f32::EPSILON作为比较阈值（数值稳定性）
        if (beta - omega0).abs() <= f64::from(f32::EPSILON) {
            // 临界阻尼：无振荡，最快回到平衡位置
            self.to + envelope * (x0 + (beta * x0 + v0) * t)
        } else if beta < omega0 {
            // 欠阻尼：振荡衰减
            let omega1 = ((omega0 * omega0) - (beta * beta)).sqrt();  // 振荡频率
            self.to
                + envelope
                    * (x0 * (omega1 * t).cos() + ((beta * x0 + v0) / omega1) * (omega1 * t).sin())
        } else {
            // 过阻尼：缓慢衰减无振荡
            let omega2 = ((beta * beta) - (omega0 * omega0)).sqrt();  // 衰减参数
            self.to
                + envelope
                    * (x0 * (omega2 * t).cosh() + ((beta * x0 + v0) / omega2) * (omega2 * t).sinh())
        }
    }
}

// 单元测试模块
#[cfg(test)]  // 条件编译：仅在测试时包含
mod tests {
    use super::*;  // 导入父模块所有内容

    // 测试起点终点相同时的情况（防止NaN）
    #[test]
    fn overdamped_spring_equal_from_to_nan() {
        let spring = Spring {
            from: 0.,
            to: 0.,
            initial_velocity: 0.,
            params: SpringParams::new(1.15, 850., 0.0001),
        };
        // 验证不会panic
        let _ = spring.duration();
        let _ = spring.clamped_duration();
        let _ = spring.value_at(Duration::ZERO);
    }

    // 测试特定参数下的过阻尼弹簧（防止计算错误）
    #[test]
    fn overdamped_spring_duration_panic() {
        let spring = Spring {
            from: 0.,
            to: 1.,
            initial_velocity: 0.,
            params: SpringParams::new(6., 1200., 0.0001),
        };
        // 验证不会panic
        let _ = spring.duration();
        let _ = spring.clamped_duration();
        let _ = spring.value_at(Duration::ZERO);
    }
}

/* 弹簧物理模型详解
1. 微分方程基础:
   m·d²x/dt² + b·dx/dt + k·x = 0
   其中:
     m = 质量
     b = 阻尼系数
     k = 刚度系数
     x = 位移（相对于平衡位置）

2. 特征方程的解:
   λ = [-b ± sqrt(b²-4mk)] / (2m)

3. 阻尼类型判定:
   - 欠阻尼: b² < 4mk  → 振荡衰减
   - 临界阻尼: b² = 4mk → 最快无振荡返回
   - 过阻尼: b² > 4mk  → 缓慢无振荡返回

4. 在合成器中的应用:
   - 窗口动画: 模拟窗口移动的惯性效果
   - 工作区切换: 平滑过渡效果
   - 菜单弹出: 自然弹性效果

5. 参数选择指南:
   - 刚度(stiffness): 值越大，动画越快（默认范围: 100-1000）
   - 阻尼比(damping_ratio):
        0 < ratio < 1: 欠阻尼（有回弹）
        ratio = 1: 临界阻尼（最快无振荡）
        ratio > 1: 过阻尼（缓慢无振荡）
   - 质量(mass): 固定为1（简化模型）
*/