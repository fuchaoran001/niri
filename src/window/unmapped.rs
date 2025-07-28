/// window/unmapped.rs - 未映射窗口实现
/// 职责：管理尚未显示在屏幕上的窗口状态
/// 关键概念：处理窗口初始配置和激活令牌

use niri_config::PresetSize;  // 预设尺寸类型
use smithay::desktop::Window;  // Smithay 窗口抽象
use smithay::output::Output;  // 显示输出
use smithay::wayland::shell::xdg::ToplevelSurface;  // Wayland toplevel 表面
use smithay::wayland::xdg_activation::XdgActivationTokenData;  // XDG 激活令牌数据

use super::ResolvedWindowRules;  // 已解析的窗口规则

/// 未映射窗口结构
/// 设计：封装窗口在映射前的所有状态
#[derive(Debug)]
pub struct Unmapped {
    /// 底层窗口对象
    pub window: Window,
    
    /// 初始配置状态
    pub state: InitialConfigureState,
    
    /// 激活令牌数据（如果有）
    /// 作用：用于窗口首次显示时的焦点管理
    pub activation_token_data: Option<XdgActivationTokenData>,
}

/// 初始配置状态枚举
/// 设计：区分未配置和已配置两种状态
#[allow(clippy::large_enum_variant)]  // 允许大枚举变体（规则结构较大）
#[derive(Debug)]
pub enum InitialConfigureState {
    /// 窗口尚未进行初始配置
    NotConfigured {
        /// 窗口请求的全屏状态及目标输出
        /// - None: 未请求全屏
        /// - Some(None): 请求全屏但未指定输出
        /// - Some(Some(output)): 请求全屏到特定输出
        wants_fullscreen: Option<Option<Output>>,
    },
    
    /// 窗口已完成初始配置
    Configured {
        /// 已解析的窗口规则
        /// 注意：初始配置后才开始跟踪规则
        rules: ResolvedWindowRules,
        
        /// 滚动布局默认宽度
        /// None 表示窗口自行决定宽度
        width: Option<PresetSize>,
        
        /// 滚动布局默认高度
        /// None 表示窗口自行决定高度
        height: Option<PresetSize>,
        
        /// 浮动布局默认宽度
        /// None 表示窗口自行决定宽度
        floating_width: Option<PresetSize>,
        
        /// 浮动布局默认高度
        /// None 表示窗口自行决定高度
        floating_height: Option<PresetSize>,
        
        /// 是否以全宽度打开（类似最大化）
        is_full_width: bool,
        
        /// 目标输出（打开窗口的位置）
        /// None 情况：
        ///   - 无可用输出
        ///   - 对话框需从父窗口获取输出
        output: Option<Output>,
        
        /// 目标工作区名称
        workspace_name: Option<String>,
    },
}

impl Unmapped {
    /// 创建新的未映射窗口
    /// 参数：window - 基础窗口对象
    /// 返回：初始状态为 NotConfigured 的 Unmapped 实例
    pub fn new(window: Window) -> Self {
        Self {
            window,
            state: InitialConfigureState::NotConfigured {
                wants_fullscreen: None,  // 初始无全屏请求
            },
            activation_token_data: None,  // 无激活令牌
        }
    }
    
    /// 检查是否需要初始配置
    /// 返回：true 表示处于 NotConfigured 状态
    pub fn needs_initial_configure(&self) -> bool {
        matches!(self.state, InitialConfigureState::NotConfigured { .. })
    }
    
    /// 获取窗口的 toplevel 表面
    /// 注意：仅支持 Wayland 窗口（不支持 X11）
    pub fn toplevel(&self) -> &ToplevelSurface {
        self.window.toplevel().expect("不支持 X11 窗口")
    }
}

/* 未映射窗口生命周期：

  创建
   │
   ├─ 状态: NotConfigured
   │   ├─ 接收配置请求 (如全屏)
   │   └─ 收集激活令牌
   │
   ├─ 初始配置
   │   ├─ 解析窗口规则
   │   ├─ 确定尺寸/位置
   │   └─ 转换为 Configured 状态
   │
   └─ 准备映射
       ├─ 应用配置
       └─ 转移到 Mapped 状态

关键点：
1. 初始配置是窗口显示前的必经阶段
2. 激活令牌用于控制窗口首次显示时的焦点行为
3. 配置状态分离确保规则处理逻辑清晰
*/