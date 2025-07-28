/// dummy_pw_utils.rs - PipeWire 虚拟实现模块
/// 作用：当未启用 `xdp-gnome-screencast` 功能时提供空实现
/// 设计意图：保持代码结构统一，避免条件编译污染主逻辑

use anyhow::bail;  // 错误处理工具宏，快速返回错误
use smithay::reexports::calloop::LoopHandle;  // 事件循环句柄类型

use crate::niri::State;  // 合成器状态类型

/// 虚拟 PipeWire 结构体
/// 设计模式：空对象模式（Null Object Pattern）
/// 作用：提供与真实 PipeWire 相同的接口，但所有操作无效果
pub struct PipeWire;

/// 虚拟屏幕投射结构体
/// 保留此结构体以保持类型一致性
pub struct Cast;

impl PipeWire {
    /// 虚拟构造函数
    /// 参数：_event_loop - 忽略事件循环句柄
    /// 返回值：总是返回错误
    /// 
    /// 关键行为：
    /// 1. 当用户尝试初始化 PipeWire 时，明确提示功能未启用
    /// 2. 避免编译错误，保持类型系统完整性
    pub fn new(_event_loop: &LoopHandle<'static, State>) -> anyhow::Result<Self> {
        /// 使用 anyhow::bail! 宏返回格式化的错误
        bail!(
            "PipeWire 支持已禁用（请启用 \"xdp-gnome-screencast\" 编译特性）"
        );
        
        /// 说明：此错误会传播到调用方，通常会导致以下结果之一：
        ///   - 合成器启动失败（如果 PipeWire 是必需组件）
        ///   - 降级使用其他屏幕共享方案
        ///   - 忽略错误继续运行（如果功能可选）
    }
}

/* 设计模式解析：

   +---------------------+      +---------------------+
   |  真实 PipeWire 实现  |      |  虚拟 PipeWire 实现  |
   +---------------------+      +---------------------+
   | + new() -> Ok(Self) |      | + new() -> Error    |
   | + 功能方法...       |      | + 无方法实现         |
   +---------------------+      +---------------------+
               ^                           ^
               |                           |
   +---------------------+      +---------------------+
   |   条件编译启用        |      |   条件编译未启用     |
   |   #[cfg(feature=...]|      |   #[cfg(not(...))] |
   +---------------------+      +---------------------+

优点：
1. 主代码无需关心是否启用 PipeWire
2. 编译时自动选择实现，零运行时开销
3. 清晰的错误提示帮助用户解决问题
*/