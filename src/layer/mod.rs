// 文件: layer/mod.rs
// 作用: 定义层表面规则解析逻辑，用于配置层表面渲染行为
// Wayland概念: LayerSurface - 分层表面协议，允许客户端创建在桌面不同层级显示的窗口
// Rust概念: 模块系统 - 通过mod声明子模块，use导入其他模块的公开项

use niri_config::layer_rule::{LayerRule, Match};
use niri_config::{BlockOutFrom, CornerRadius, ShadowRule};
use smithay::desktop::LayerSurface;

// 子模块声明: mapped
// 作用: 包含已映射层表面的处理逻辑
pub mod mapped;
// 公开导出: MappedLayer
// 作用: 表示已配置并准备好渲染的层表面
pub use mapped::MappedLayer;

/// Rules fully resolved for a layer-shell surface.
// 中文翻译: 已为层表面完全解析的规则
#[derive(Debug, PartialEq)]
// 结构体: ResolvedLayerRules
// 作用: 存储层表面的最终渲染规则，包含所有配置覆盖项
// Rust概念: derive宏 - 自动生成Debug和PartialEq trait实现
pub struct ResolvedLayerRules {
    /// Extra opacity to draw this layer surface with.
    // 中文翻译: 绘制此层表面的额外不透明度
    pub opacity: Option<f32>,

    /// Whether to block out this layer surface from certain render targets.
    // 中文翻译: 是否将此层表面从特定渲染目标中排除
    pub block_out_from: Option<BlockOutFrom>,

    /// Shadow overrides.
    // 中文翻译: 阴影覆盖设置
    pub shadow: ShadowRule,

    /// Corner radius to assume this layer surface has.
    // 中文翻译: 假定此层表面具有的圆角半径
    pub geometry_corner_radius: Option<CornerRadius>,

    /// Whether to place this layer surface within the overview backdrop.
    // 中文翻译: 是否将此层表面放置在概览背景中
    pub place_within_backdrop: bool,

    /// Whether to bob this window up and down.
    // 中文翻译: 是否使此窗口上下浮动
    pub baba_is_float: bool,
}

// ResolvedLayerRules的实现块
impl ResolvedLayerRules {
    // 函数: empty
    // 作用: 创建空规则集（所有值设为默认状态）
    // Rust概念: const函数 - 编译时可求值的常量函数
    pub const fn empty() -> Self {
        Self {
            opacity: None,
            block_out_from: None,
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
            geometry_corner_radius: None,
            place_within_backdrop: false,
            baba_is_float: false,
        }
    }

    // 函数: compute
    // 作用: 根据配置规则集计算层表面的最终渲染规则
    // 参数:
    //   rules - 配置规则列表
    //   surface - 目标层表面
    //   is_at_startup - 是否在启动阶段
    // 流程图:
    //   [开始] 
    //   -> 创建空规则集
    //   -> 遍历每条规则:
    //        |-> 检查规则是否匹配 (包含匹配项且不排除)
    //            |-> 匹配成功: 应用规则覆盖
    //   -> 返回最终规则集
    pub fn compute(rules: &[LayerRule], surface: &LayerSurface, is_at_startup: bool) -> Self {
        // 性能分析: 创建跟踪span用于性能监控
        let _span = tracy_client::span!("ResolvedLayerRules::compute");

        // 初始化空规则集
        let mut resolved = ResolvedLayerRules::empty();

        // 遍历所有规则
        for rule in rules {
            // 闭包: 检查单个匹配条件
            // Rust概念: 闭包 - 捕获上下文的匿名函数
            let matches = |m: &Match| {
                // 检查启动条件
                if let Some(at_startup) = m.at_startup {
                    if at_startup != is_at_startup {
                        return false;
                    }
                }

                // 调用匹配函数
                surface_matches(surface, m)
            };

            // 规则匹配逻辑:
            // 1. 若无匹配条件则默认匹配
            // 2. 否则需要至少一个条件匹配
            if !(rule.matches.is_empty() || rule.matches.iter().any(matches)) {
                continue;
            }

            // 排除逻辑: 任一排除条件匹配则跳过此规则
            if rule.excludes.iter().any(matches) {
                continue;
            }

            // 应用规则覆盖 (Option::take模式)
            // Rust概念: if let - 模式匹配解构Option
            if let Some(x) = rule.opacity {
                resolved.opacity = Some(x);
            }
            if let Some(x) = rule.block_out_from {
                resolved.block_out_from = Some(x);
            }
            if let Some(x) = rule.geometry_corner_radius {
                resolved.geometry_corner_radius = Some(x);
            }
            if let Some(x) = rule.place_within_backdrop {
                resolved.place_within_backdrop = x;
            }
            if let Some(x) = rule.baba_is_float {
                resolved.baba_is_float = x;
            }

            // 合并阴影规则
            // Wayland概念: 阴影 - 控制窗口阴影的视觉表现
            resolved.shadow.merge_with(&rule.shadow);
        }

        resolved
    }
}

// 函数: surface_matches
// 作用: 检查层表面是否匹配给定条件
// 参数:
//   surface - 目标层表面
//   m - 匹配条件
// 返回: bool (是否匹配)
fn surface_matches(surface: &LayerSurface, m: &Match) -> bool {
    // 检查命名空间正则匹配
    if let Some(namespace_re) = &m.namespace {
        // Wayland概念: 命名空间 - 层表面的唯一标识符
        if !namespace_re.0.is_match(surface.namespace()) {
            return false;
        }
    }

    // 其他匹配条件可在此扩展
    true
}