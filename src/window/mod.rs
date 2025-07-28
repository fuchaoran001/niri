// window/mod.rs
// 此文件定义了窗口管理系统的核心结构，包括窗口引用、规则解析和应用逻辑
// 在合成器中，窗口规则系统允许用户自定义窗口行为（如大小、位置、外观等）

use std::cmp::{max, min};  // 比较函数

use niri_config::{  // 配置结构体
    BlockOutFrom, BorderRule, CornerRadius, FloatingPosition, Match, PresetSize, ShadowRule,
    TabIndicatorRule, WindowRule,
};
use niri_ipc::ColumnDisplay;  // IPC通信定义
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;  // Wayland顶层协议
use smithay::utils::{Logical, Size};  // 逻辑坐标和尺寸
use smithay::wayland::compositor::with_states;  // Wayland状态访问
use smithay::wayland::shell::xdg::{  // XDG shell实现
    SurfaceCachedState, ToplevelSurface, XdgToplevelSurfaceRoleAttributes,
};

use crate::utils::with_toplevel_role;  // 辅助函数

// 子模块：已映射窗口管理
pub mod mapped;
pub use mapped::Mapped;  // 导出已映射窗口结构体

// 子模块：未映射窗口管理
pub mod unmapped;
pub use unmapped::{InitialConfigureState, Unmapped};  // 导出未映射窗口结构体

/// 窗口引用枚举（已映射或未映射）
#[derive(Debug, Clone, Copy)]
pub enum WindowRef<'a> {
    Unmapped(&'a Unmapped),  // 未映射窗口引用
    Mapped(&'a Mapped),      // 已映射窗口引用
}

/// 已解析的窗口规则集合
/// 包含所有应用到窗口的规则计算结果
#[derive(Debug, PartialEq)]
pub struct ResolvedWindowRules {
    /// 默认宽度（None表示未设置，Some(None)表示由窗口决定）
    pub default_width: Option<Option<PresetSize>>,
    
    /// 默认高度
    pub default_height: Option<Option<PresetSize>>,
    
    /// 默认列显示方式
    pub default_column_display: Option<ColumnDisplay>,
    
    /// 默认浮动位置
    pub default_floating_position: Option<FloatingPosition>,
    
    /// 指定打开窗口的输出设备
    pub open_on_output: Option<String>,
    
    /// 指定打开窗口的工作区
    pub open_on_workspace: Option<String>,
    
    /// 是否最大化打开
    pub open_maximized: Option<bool>,
    
    /// 是否全屏打开
    pub open_fullscreen: Option<bool>,
    
    /// 是否浮动打开
    pub open_floating: Option<bool>,
    
    /// 是否聚焦打开
    pub open_focused: Option<bool>,
    
    // 尺寸约束
    pub min_width: Option<u16>,
    pub min_height: Option<u16>,
    pub max_width: Option<u16>,
    pub max_height: Option<u16>,
    
    // 外观规则
    pub focus_ring: BorderRule,           // 聚焦边框规则
    pub border: BorderRule,                // 窗口边框规则
    pub shadow: ShadowRule,                // 阴影规则
    pub tab_indicator: TabIndicatorRule,   // 标签指示器规则
    
    /// 是否用实心背景绘制边框
    pub draw_border_with_background: Option<bool>,
    
    /// 窗口不透明度
    pub opacity: Option<f32>,
    
    /// 窗口圆角半径
    pub geometry_corner_radius: Option<CornerRadius>,
    
    /// 是否裁剪到几何形状（含圆角）
    pub clip_to_geometry: Option<bool>,
    
    /// 是否启用浮动动画（上下浮动）
    pub baba_is_float: Option<bool>,
    
    /// 渲染屏蔽设置
    pub block_out_from: Option<BlockOutFrom>,
    
    /// 是否启用可变刷新率
    pub variable_refresh_rate: Option<bool>,
    
    /// 滚动事件乘数
    pub scroll_factor: Option<f64>,
    
    /// 是否设置平铺状态
    pub tiled_state: Option<bool>,
}

// 窗口引用方法实现
impl<'a> WindowRef<'a> {
    /// 获取底层ToplevelSurface
    pub fn toplevel(self) -> &'a ToplevelSurface {
        match self {
            WindowRef::Unmapped(unmapped) => unmapped.toplevel(),
            WindowRef::Mapped(mapped) => mapped.toplevel(),
        }
    }
    
    /// 检查窗口是否聚焦
    pub fn is_focused(self) -> bool {
        match self {
            WindowRef::Unmapped(_) => false,
            WindowRef::Mapped(mapped) => mapped.is_focused(),
        }
    }
    
    /// 检查窗口是否紧急（需要用户注意）
    pub fn is_urgent(self) -> bool {
        match self {
            WindowRef::Unmapped(_) => false,
            WindowRef::Mapped(mapped) => mapped.is_urgent(),
        }
    }
    
    /// 检查窗口在列中是否激活
    pub fn is_active_in_column(self) -> bool {
        match self {
            // 未映射窗口视为激活（需要处理）
            WindowRef::Unmapped(_) => true,
            WindowRef::Mapped(mapped) => mapped.is_active_in_column(),
        }
    }
    
    /// 检查窗口是否浮动
    pub fn is_floating(self) -> bool {
        match self {
            // 注意：未映射窗口无法确定浮动状态（规则应用限制）
            WindowRef::Unmapped(_) => false,
            WindowRef::Mapped(mapped) => mapped.is_floating(),
        }
    }
    
    /// 检查窗口是否作为投屏目标
    pub fn is_window_cast_target(self) -> bool {
        match self {
            WindowRef::Unmapped(_) => false,
            WindowRef::Mapped(mapped) => mapped.is_window_cast_target(),
        }
    }
}

// 已解析规则方法实现
impl ResolvedWindowRules {
    /// 创建空规则集合
    pub const fn empty() -> Self {
        Self {
            // 初始化所有字段为None或默认值
            default_width: None,
            default_height: None,
            default_column_display: None,
            default_floating_position: None,
            open_on_output: None,
            open_on_workspace: None,
            open_maximized: None,
            open_fullscreen: None,
            open_floating: None,
            open_focused: None,
            min_width: None,
            min_height: None,
            max_width: None,
            max_height: None,
            // 边框规则默认值
            focus_ring: BorderRule {
                off: false,
                on: false,
                width: None,
                active_color: None,
                inactive_color: None,
                urgent_color: None,
                active_gradient: None,
                inactive_gradient: None,
                urgent_gradient: None,
            },
            border: BorderRule {
                off: false,
                on: false,
                width: None,
                active_color: None,
                inactive_color: None,
                urgent_color: None,
                active_gradient: None,
                inactive_gradient: None,
                urgent_gradient: None,
            },
            shadow: ShadowRule {
                off: false,
                on: false,
                offset: None,
                softness: None,
                spread: None,
                draw_behind_window: None,
                color: None,
                inactive_color: None,
            },
            tab_indicator: TabIndicatorRule {
                active_color: None,
                inactive_color: None,
                urgent_color: None,
                active_gradient: None,
                inactive_gradient: None,
                urgent_gradient: None,
            },
            draw_border_with_background: None,
            opacity: None,
            geometry_corner_radius: None,
            clip_to_geometry: None,
            baba_is_float: None,
            block_out_from: None,
            variable_refresh_rate: None,
            scroll_factor: None,
            tiled_state: None,
        }
    }
    
    /// 计算窗口应用的规则
    /// 参数:
    ///   rules - 所有可用规则列表
    ///   window - 目标窗口引用
    ///   is_at_startup - 是否在启动阶段
    pub fn compute(rules: &[WindowRule], window: WindowRef, is_at_startup: bool) -> Self {
        let _span = tracy_client::span!("ResolvedWindowRules::compute");  // 性能分析
        
        // 创建空规则集合
        let mut resolved = ResolvedWindowRules::empty();
        
        // 访问窗口的Wayland角色属性
        with_toplevel_role(window.toplevel(), |role| {
            // 确保存在待处理状态（用于规则匹配）
            if role.server_pending.is_none() {
                role.server_pending = Some(role.current_server_state().clone());
            }
            
            // 临时存储输出和工作区名称（用于最后处理）
            let mut open_on_output = None;
            let mut open_on_workspace = None;
            
            // 遍历所有规则
            for rule in rules {
                // 定义匹配函数（闭包）
                let matches = |m: &Match| {
                    // 检查启动条件
                    if let Some(at_startup) = m.at_startup {
                        if at_startup != is_at_startup {
                            return false;
                        }
                    }
                    
                    // 检查窗口是否匹配当前规则条件
                    window_matches(window, role, m)
                };
                
                // 检查规则是否适用（匹配任意条件且不排除）
                if !(rule.matches.is_empty() || rule.matches.iter().any(matches)) {
                    continue;  // 跳过不匹配规则
                }
                
                if rule.excludes.iter().any(matches) {
                    continue;  // 排除条件匹配，跳过
                }
                
                // 应用规则属性（条件覆盖）
                // 尺寸规则
                if let Some(x) = rule.default_column_width {
                    resolved.default_width = Some(x.0);
                }
                if let Some(x) = rule.default_window_height {
                    resolved.default_height = Some(x.0);
                }
                
                // 布局规则
                if let Some(x) = rule.default_column_display {
                    resolved.default_column_display = Some(x);
                }
                if let Some(x) = rule.default_floating_position {
                    resolved.default_floating_position = Some(x);
                }
                
                // 打开位置规则（临时存储）
                if let Some(x) = rule.open_on_output.as_deref() {
                    open_on_output = Some(x);
                }
                if let Some(x) = rule.open_on_workspace.as_deref() {
                    open_on_workspace = Some(x);
                }
                
                // 打开状态规则
                if let Some(x) = rule.open_maximized {
                    resolved.open_maximized = Some(x);
                }
                if let Some(x) = rule.open_fullscreen {
                    resolved.open_fullscreen = Some(x);
                }
                if let Some(x) = rule.open_floating {
                    resolved.open_floating = Some(x);
                }
                if let Some(x) = rule.open_focused {
                    resolved.open_focused = Some(x);
                }
                
                // 尺寸约束规则
                if let Some(x) = rule.min_width {
                    resolved.min_width = Some(x);
                }
                if let Some(x) = rule.min_height {
                    resolved.min_height = Some(x);
                }
                if let Some(x) = rule.max_width {
                    resolved.max_width = Some(x);
                }
                if let Some(x) = rule.max_height {
                    resolved.max_height = Some(x);
                }
                
                // 外观规则（合并方式）
                resolved.focus_ring.merge_with(&rule.focus_ring);
                resolved.border.merge_with(&rule.border);
                resolved.shadow.merge_with(&rule.shadow);
                resolved.tab_indicator.merge_with(&rule.tab_indicator);
                
                // 其他规则
                if let Some(x) = rule.draw_border_with_background {
                    resolved.draw_border_with_background = Some(x);
                }
                if let Some(x) = rule.opacity {
                    resolved.opacity = Some(x);
                }
                if let Some(x) = rule.geometry_corner_radius {
                    resolved.geometry_corner_radius = Some(x);
                }
                if let Some(x) = rule.clip_to_geometry {
                    resolved.clip_to_geometry = Some(x);
                }
                if let Some(x) = rule.baba_is_float {
                    resolved.baba_is_float = Some(x);
                }
                if let Some(x) = rule.block_out_from {
                    resolved.block_out_from = Some(x);
                }
                if let Some(x) = rule.variable_refresh_rate {
                    resolved.variable_refresh_rate = Some(x);
                }
                if let Some(x) = rule.scroll_factor {
                    resolved.scroll_factor = Some(x.0);
                }
                if let Some(x) = rule.tiled_state {
                    resolved.tiled_state = Some(x);
                }
            }
            
            // 设置最终打开位置
            resolved.open_on_output = open_on_output.map(|x| x.to_owned());
            resolved.open_on_workspace = open_on_workspace.map(|x| x.to_owned());
        });
        
        resolved
    }
    
    /// 应用最小尺寸约束
    pub fn apply_min_size(&self, min_size: Size<i32, Logical>) -> Size<i32, Logical> {
        let mut size = min_size;
        
        // 宽度约束
        if let Some(x) = self.min_width {
            size.w = max(size.w, i32::from(x));
        }
        
        // 高度约束
        if let Some(x) = self.min_height {
            size.h = max(size.h, i32::from(x));
        }
        
        size
    }
    
    /// 应用最大尺寸约束
    pub fn apply_max_size(&self, max_size: Size<i32, Logical>) -> Size<i32, Logical> {
        let mut size = max_size;
        
        // 宽度约束（特殊处理0值）
        if let Some(x) = self.max_width {
            if size.w == 0 {
                size.w = i32::from(x);
            } else if x > 0 {
                size.w = min(size.w, i32::from(x));
            }
        }
        
        // 高度约束
        if let Some(x) = self.max_height {
            if size.h == 0 {
                size.h = i32::from(x);
            } else if x > 0 {
                size.h = min(size.h, i32::from(x));
            }
        }
        
        size
    }
    
    /// 同时应用最小和最大尺寸约束
    pub fn apply_min_max_size(
        &self,
        min_size: Size<i32, Logical>,
        max_size: Size<i32, Logical>,
    ) -> (Size<i32, Logical>, Size<i32, Logical>) {
        let min_size = self.apply_min_size(min_size);
        let max_size = self.apply_max_size(max_size);
        (min_size, max_size)
    }
    
    /// 计算窗口是否应浮动打开
    pub fn compute_open_floating(&self, toplevel: &ToplevelSurface) -> bool {
        // 规则优先
        if let Some(res) = self.open_floating {
            return res;
        }
        
        // 有父窗口的窗口（如对话框）默认浮动
        if toplevel.parent().is_some() {
            return true;
        }
        
        // 获取窗口尺寸约束
        let (min_size, max_size) = with_states(toplevel.wl_surface(), |state| {
            let mut guard = state.cached_state.get::<SurfaceCachedState>();
            let current = guard.current();
            (current.min_size, current.max_size)
        });
        
        // 应用规则约束
        let (min_size, max_size) = self.apply_min_max_size(min_size, max_size);
        
        // 固定高度的窗口默认浮动
        min_size.h > 0 && min_size.h == max_size.h
    }
}

/// 检查窗口是否匹配规则条件
fn window_matches(window: WindowRef, role: &XdgToplevelSurfaceRoleAttributes, m: &Match) -> bool {
    // 获取待处理状态（由调用者确保存在）
    let server_pending = role.server_pending.as_ref().unwrap();
    
    // 检查聚焦状态
    if let Some(is_focused) = m.is_focused {
        if window.is_focused() != is_focused {
            return false;
        }
    }
    
    // 检查紧急状态
    if let Some(is_urgent) = m.is_urgent {
        if window.is_urgent() != is_urgent {
            return false;
        }
    }
    
    // 检查激活状态（对应Wayland的Activated状态）
    if let Some(is_active) = m.is_active {
        let pending_activated = server_pending
            .states
            .contains(xdg_toplevel::State::Activated);
        if is_active != pending_activated {
            return false;
        }
    }
    
    // 检查应用ID正则匹配
    if let Some(app_id_re) = &m.app_id {
        let Some(app_id) = &role.app_id else {
            return false;  // 无应用ID则不匹配
        };
        if !app_id_re.0.is_match(app_id) {
            return false;
        }
    }
    
    // 检查标题正则匹配
    if let Some(title_re) = &m.title {
        let Some(title) = &role.title else {
            return false;  // 无标题则不匹配
        };
        if !title_re.0.is_match(title) {
            return false;
        }
    }
    
    // 检查列内激活状态
    if let Some(is_active_in_column) = m.is_active_in_column {
        if window.is_active_in_column() != is_active_in_column {
            return false;
        }
    }
    
    // 检查浮动状态
    if let Some(is_floating) = m.is_floating {
        if window.is_floating() != is_floating {
            return false;
        }
    }
    
    // 检查投屏目标状态
    if let Some(is_window_cast_target) = m.is_window_cast_target {
        if window.is_window_cast_target() != is_window_cast_target {
            return false;
        }
    }
    
    // 所有条件通过
    true
}

/* 窗口规则系统详解

1. 规则匹配流程
   +-----------------------+
   | 遍历所有规则            |
   | 对每个规则:            |
   |   a. 检查是否匹配条件    |
   |   b. 检查是否排除条件    |
   |   c. 应用规则属性       |
   +-----------------------+

2. 条件类型
   - 布尔状态: 聚焦/紧急/激活等
   - 字符串匹配: 应用ID/标题（支持正则）
   - 布局状态: 浮动/列内激活等
   - 启动状态: 是否在启动阶段

3. 规则应用优先级
   - 规则按配置文件顺序应用
   - 后应用的规则覆盖先前的
   - 例外: 打开位置规则（最后生效）

4. 浮动窗口启发式规则
   a. 有父窗口 → 浮动
   b. 固定高度 → 浮动
   c. 用户规则优先

5. Wayland状态管理
   - server_pending: 待应用的状态
   - SurfaceCachedState: 存储尺寸约束
   - ToplevelSurface: 代表顶层窗口

6. 使用场景
   - 窗口打开时应用初始规则
   - 运行时动态更新规则
   - 用户配置自定义窗口行为
*/