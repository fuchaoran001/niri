/// lib.rs - niri 合成器的主库文件，作为整个项目的根模块
/// 该文件定义了库的公共接口和模块结构

/// 启用 tracing 宏的全局引入，用于日志和性能追踪
/// #[macro_use] 属性表示外部 crate 的宏将在本 crate 中可见
/// 概念：Rust 的宏系统允许在编译时进行代码生成
#[macro_use]
extern crate tracing;

/// 声明公共模块 animation - 负责窗口动画和转场效果
/// 在合成器中处理动画逻辑如窗口移动/缩放时的插值计算
pub mod animation;

/// 声明公共模块 backend - 抽象图形后端接口
/// 关键设计：提供统一的渲染接口，支持不同后端(如Wayland/X11)
pub mod backend;

/// 声明公共模块 cli - 命令行参数解析
/// 处理启动参数如--verbose、--config等
pub mod cli;

/// 声明公共模块 cursor - 光标管理
/// 职责：跟踪光标位置、形状变化和主题设置
pub mod cursor;

/// 条件编译：仅当启用 "dbus" 功能时包含 dbus 模块
/// 概念：#[cfg] 属性实现条件编译，常见于特性开关
#[cfg(feature = "dbus")]
pub mod dbus;  // D-Bus IPC 接口实现

/// 声明公共模块 frame_clock - 帧同步时钟
/// 合成器核心：管理VSync信号，协调渲染循环
pub mod frame_clock;

/// 声明公共模块 handlers - 事件处理器
/// 关键作用：将输入事件(键盘/鼠标)路由到对应窗口
pub mod handlers;

/// 声明公共模块 input - 输入设备管理
/// 数据结构：维护键盘、鼠标、触摸板等设备的抽象状态
pub mod input;

/// 声明公共模块 ipc - 进程间通信
/// 支持合成器与外部工具通信(如状态查询)
pub mod ipc;

/// 声明公共模块 layer - 图层管理
/// 核心概念：Wayland 的图层式窗口管理基础
pub mod layer;

/// 声明公共模块 layout - 布局引擎
/// 职责：计算窗口位置/尺寸，实现平铺/浮动布局
pub mod layout;

/// 声明公共模块 niri - 合成器主逻辑
/// 包含 Compositor 结构体，是整个合成器的状态机
pub mod niri;

/// 声明公共模块 protocols - Wayland 协议实现
/// 关键作用：实现各类Wayland接口(如xdg_shell)
pub mod protocols;

/// 声明公共模块 render_helpers - 渲染辅助工具
/// 提供共享的渲染函数如纹理处理、着色器管理
pub mod render_helpers;

/// 声明公共模块 rubber_band - 弹性滚动效果
/// 模拟物理滚动效果（如惯性滚动、边界回弹）
pub mod rubber_band;

/// 声明公共模块 ui - 用户界面组件
/// 包含状态栏、菜单等合成器自有界面元素
pub mod ui;

/// 声明公共模块 utils - 工具函数集
/// 提供跨模块使用的辅助函数(如几何计算)
pub mod utils;

/// 声明公共模块 window - 窗口对象
/// 核心数据结构：表示单个窗口及其状态
pub mod window;

/// 条件编译：当未启用 xdp-gnome-screencast 功能时
/// 提供虚拟的 PipeWire 工具实现（空操作）
#[cfg(not(feature = "xdp-gnome-screencast"))]
pub mod dummy_pw_utils;

/// 条件编译：当启用 xdp-gnome-screencast 功能时
/// 提供真正的 PipeWire 屏幕捕获实现
#[cfg(feature = "xdp-gnome-screencast")]
pub mod pw_utils;

/// 统一导出：无论是否启用 xdp-gnome-screencast
/// 都使用 pw_utils 名称导出对应模块
/// 设计意图：简化调用方代码，避免条件判断
#[cfg(not(feature = "xdp-gnome-screencast"))]
pub use dummy_pw_utils as pw_utils;  // 重定向到虚拟实现

/// 条件编译：测试专用模块
/// 仅在运行 cargo test 时包含
#[cfg(test)]
mod tests;  // 单元测试和集成测试