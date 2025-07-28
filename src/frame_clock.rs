// 文件: frame_clock.rs
// 作用: 帧时钟管理模块，负责计算和预测合成器下一帧的呈现时间
// 关键概念:
//   - 帧时钟: 合成器中协调帧渲染和显示的核心计时机制
//   - VRR (可变刷新率): 允许显示器动态调整刷新率的技术，减少画面撕裂
//   - 呈现时间: 帧实际显示在屏幕上的时间点

use std::num::NonZeroU64;  // Rust特性: 非零整数类型，优化内存布局并保证安全
use std::time::Duration;   // Rust标准库: 表示时间跨度

use crate::utils::get_monotonic_time;  // 引入工具函数: 获取单调递增的系统时间

#[derive(Debug)]  // Rust特性: 自动派生Debug trait，便于打印调试信息
pub struct FrameClock {
    // 上一次帧实际呈现的时间点
    last_presentation_time: Option<Duration>,  // Rust概念: Option<T> 表示可能有值(T)或空(None)
    
    // 刷新间隔(纳秒)，使用NonZeroU64优化内存
    refresh_interval_ns: Option<NonZeroU64>,  // 合成器概念: 显示器刷新周期，如60Hz对应16.67ms
    
    // 是否启用可变刷新率(VRR)
    vrr: bool,  // Wayland概念: VRR允许动态调整刷新率匹配渲染速度
}

impl FrameClock {
    // 构造函数: 创建新的帧时钟实例
    // 参数:
    //   refresh_interval - 显示器的刷新间隔
    //   vrr - 是否启用可变刷新率
    pub fn new(refresh_interval: Option<Duration>, vrr: bool) -> Self {
        // 将刷新间隔转换为纳秒存储(非零值优化)
        let refresh_interval_ns = if let Some(interval) = &refresh_interval {
            // 断言确保秒数为0(只处理毫秒/纳秒级间隔)
            assert_eq!(interval.as_secs(), 0);
            // NonZeroU64::new()创建非零值，unwrap()因为纳秒值必定非零
            Some(NonZeroU64::new(interval.subsec_nanos().into()).unwrap())
        } else {
            None  // 无固定刷新率模式(如VRR全动态)
        };

        Self {
            last_presentation_time: None,  // 初始无历史呈现时间
            refresh_interval_ns,
            vrr,
        }
    }

    // 获取当前刷新间隔
    pub fn refresh_interval(&self) -> Option<Duration> {
        // 将纳秒值转回Duration类型
        self.refresh_interval_ns
            .map(|r| Duration::from_nanos(r.get()))  // Rust概念: map处理Option内部值
    }

    // 设置VRR模式状态
    pub fn set_vrr(&mut self, vrr: bool) {
        // 状态无变化时直接返回
        if self.vrr == vrr {
            return;
        }

        self.vrr = vrr;
        // 重置历史记录(刷新模式改变需重新校准)
        self.last_presentation_time = None;
    }

    // 查询当前VRR状态
    pub fn vrr(&self) -> bool {
        self.vrr
    }

    // 记录新帧的呈现时间
    pub fn presented(&mut self, presentation_time: Duration) {
        // 忽略零值时间(无效时间戳)
        if presentation_time.is_zero() {
            return;
        }

        // 更新最近呈现时间
        self.last_presentation_time = Some(presentation_time);
    }

    // 计算并返回下一帧的理想呈现时间
    pub fn next_presentation_time(&self) -> Duration {
        // 获取当前单调时间(不受系统时钟调整影响)
        let mut now = get_monotonic_time();

        /* 处理无刷新间隔或无历史记录的情况 */
        // 情况1: 无固定刷新率 -> 立即返回当前时间
        let Some(refresh_interval_ns) = self.refresh_interval_ns else {
            return now;
        };
        // 情况2: 无历史呈现时间 -> 返回当前时间
        let Some(last_presentation_time) = self.last_presentation_time else {
            return now;
        };

        // 提取刷新间隔值
        let refresh_interval_ns = refresh_interval_ns.get();

        // 处理VBlank提前到达的情况(当前时间早于上次呈现时间)
        // 流程图:
        //   [当前时间 <= 上次呈现时间?] 
        //     -> 是: 调整当前时间 = now + 刷新间隔
        //     -> 否: 保持当前时间
        if now <= last_presentation_time {
            let orig_now = now;  // 保存原始时间用于日志
            now += Duration::from_nanos(refresh_interval_ns);  // 向后偏移一个刷新周期

            // 双重检查: 偏移后仍早于上次呈现时间(极端情况)
            if now < last_presentation_time {
                // 记录异常情况(连续多个提前的VBlank)
                error!(
                    now = ?orig_now,
                    ?last_presentation_time,
                    "got a 2+ early VBlank, {:?} until presentation",
                    last_presentation_time - now,
                );
                // 手动校准到合理时间
                now = last_presentation_time + Duration::from_nanos(refresh_interval_ns);
            }
        }

        // 计算自上次呈现后经过的时间
        let since_last = now - last_presentation_time;
        // 转换为纳秒精度
        let since_last_ns =
            since_last.as_secs() * 1_000_000_000 + u64::from(since_last.subsec_nanos());
        
        // 计算到下一帧的纳秒数(向上取整到刷新间隔倍数)
        // 示例: 
        //   刷新间隔 = 16.67ms (60Hz)
        //   经过时间 = 10ms -> to_next_ns = 16.67ms (1个周期)
        //   经过时间 = 20ms -> to_next_ns = 33.34ms (2个周期)
        let to_next_ns = (since_last_ns / refresh_interval_ns + 1) * refresh_interval_ns;

        /* VRR特殊处理 */
        // 当启用VRR且预测间隔超过一帧时，允许立即呈现
        // 原理: VRR可动态适配帧率，避免强制等待固定间隔
        if self.vrr && to_next_ns > refresh_interval_ns {
            now  // 返回当前时间(尽快呈现)
        } else {
            // 标准模式: 按固定间隔返回下一呈现时间点
            last_presentation_time + Duration::from_nanos(to_next_ns)
        }
    }
}