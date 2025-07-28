// 文件: layer/mapped.rs
// 作用: 实现已映射层表面的渲染和管理逻辑
// Wayland概念: LayerSurface - 分层表面协议，允许客户端在桌面不同层级显示内容
// Rust概念: 泛型 - <R: NiriRenderer> 表示接受任何实现NiriRenderer的类型

use niri_config::layer_rule::LayerRule;
use niri_config::Config;
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::desktop::{LayerSurface, PopupManager};
use smithay::utils::{Logical, Point, Scale, Size};
use smithay::wayland::shell::wlr_layer::{ExclusiveZone, Layer};

// 导入父模块的ResolvedLayerRules
use super::ResolvedLayerRules;
// 导入本地工具函数和类型
use crate::animation::Clock;
use crate::layout::shadow::Shadow;
use crate::niri_render_elements;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::shadow::ShadowRenderElement;
use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::render_helpers::{RenderTarget, SplitElements};
use crate::utils::{baba_is_float_offset, round_logical_in_physical};

// 结构体: MappedLayer
// 作用: 表示已配置并准备好渲染的层表面，包含所有渲染所需状态
#[derive(Debug)]
pub struct MappedLayer {
    /// The surface itself.
    // 字段: surface
    // 类型: LayerSurface
    // 作用: 实际层表面实例
    surface: LayerSurface,

    /// Up-to-date rules.
    // 字段: rules
    // 类型: ResolvedLayerRules
    // 作用: 当前应用的渲染规则
    rules: ResolvedLayerRules,

    /// Buffer to draw instead of the surface when it should be blocked out.
    // 字段: block_out_buffer
    // 类型: SolidColorBuffer
    // 作用: 当需要阻止渲染时使用的纯色替代缓冲区
    block_out_buffer: SolidColorBuffer,

    /// The shadow around the surface.
    // 字段: shadow
    // 类型: Shadow
    // 作用: 表面阴影渲染器
    shadow: Shadow,

    /// The view size for the layer surface's output.
    // 字段: view_size
    // 类型: Size<f64, Logical>
    // 作用: 输出视图的逻辑尺寸
    view_size: Size<f64, Logical>,

    /// Scale of the output the layer surface is on (and rounds its sizes to).
    // 字段: scale
    // 类型: f64
    // 作用: 输出设备的缩放因子
    scale: f64,

    /// Clock for driving animations.
    // 字段: clock
    // 类型: Clock
    // 作用: 动画时钟驱动
    clock: Clock,
}

// 宏: niri_render_elements!
// 作用: 定义层表面渲染元素的枚举类型
// Rust概念: 声明宏 - 在编译时生成代码
niri_render_elements! {
    LayerSurfaceRenderElement<R> => {
        Wayland = WaylandSurfaceRenderElement<R>,
        SolidColor = SolidColorRenderElement,
        Shadow = ShadowRenderElement,
    }
}

// MappedLayer的实现块
impl MappedLayer {
    // 函数: new
    // 作用: 创建新的已映射层表面
    // 参数:
    //   surface - 层表面实例
    //   rules - 解析后的渲染规则
    //   view_size - 输出视图尺寸
    //   scale - 输出缩放因子
    //   clock - 动画时钟
    //   config - 全局配置
    pub fn new(
        surface: LayerSurface,
        rules: ResolvedLayerRules,
        view_size: Size<f64, Logical>,
        scale: f64,
        clock: Clock,
        config: &Config,
    ) -> Self {
        // 初始化阴影配置
        let mut shadow_config = config.layout.shadow;
        // 层表面阴影需要显式启用
        shadow_config.on = false;
        // 合并规则中的阴影覆盖
        let shadow_config = rules.shadow.resolve_against(shadow_config);

        // 创建MappedLayer实例
        Self {
            surface,
            rules,
            // 创建黑色纯色缓冲区
            block_out_buffer: SolidColorBuffer::new((0., 0.), [0., 0., 0., 1.]),
            view_size,
            scale,
            // 使用配置创建阴影渲染器
            shadow: Shadow::new(shadow_config),
            clock,
        }
    }

    // 函数: update_config
    // 作用: 更新全局配置
    pub fn update_config(&mut self, config: &Config) {
        // 更新阴影配置
        let mut shadow_config = config.layout.shadow;
        shadow_config.on = false;
        let shadow_config = self.rules.shadow.resolve_against(shadow_config);
        self.shadow.update_config(shadow_config);
    }

    // 函数: update_shaders
    // 作用: 更新着色器（例如分辨率变化时）
    pub fn update_shaders(&mut self) {
        self.shadow.update_shaders();
    }

    // 函数: update_sizes
    // 作用: 更新视图尺寸和缩放因子
    pub fn update_sizes(&mut self, view_size: Size<f64, Logical>, scale: f64) {
        self.view_size = view_size;
        self.scale = scale;
    }

    // 函数: update_render_elements
    // 作用: 更新渲染元素（尺寸变化时调用）
    // 参数: size - 新的表面尺寸
    pub fn update_render_elements(&mut self, size: Size<f64, Logical>) {
        // 将逻辑尺寸四舍五入到物理像素
        // Rust概念: 方法链 - 连续调用多个方法
        let size = size
            .to_physical_precise_round(self.scale)
            .to_logical(self.scale);

        // 调整纯色缓冲区大小
        self.block_out_buffer.resize(size);

        // 获取圆角半径配置
        let radius = self.rules.geometry_corner_radius.unwrap_or_default();
        // 更新阴影渲染元素
        // FIXME: 基于键盘焦点设置is_active?
        self.shadow
            .update_render_elements(size, true, radius, self.scale, 1.);
    }

    // 函数: are_animations_ongoing
    // 作用: 检查是否有动画正在进行
    pub fn are_animations_ongoing(&self) -> bool {
        self.rules.baba_is_float // "baba is float"动画状态
    }

    // 函数: surface
    // 作用: 获取层表面引用
    pub fn surface(&self) -> &LayerSurface {
        &self.surface
    }

    // 函数: rules
    // 作用: 获取渲染规则引用
    pub fn rules(&self) -> &ResolvedLayerRules {
        &self.rules
    }

    // 函数: recompute_layer_rules
    // 作用: 重新计算层规则并返回是否改变
    // 参数:
    //   rules - 配置规则列表
    //   is_at_startup - 是否在启动阶段
    pub fn recompute_layer_rules(&mut self, rules: &[LayerRule], is_at_startup: bool) -> bool {
        // 计算新规则
        let new_rules = ResolvedLayerRules::compute(rules, &self.surface, is_at_startup);
        // 检查规则是否改变
        if new_rules == self.rules {
            return false;
        }

        // 更新规则
        self.rules = new_rules;
        true
    }

    // 函数: place_within_backdrop
    // 作用: 判断是否应放置在概览背景中
    pub fn place_within_backdrop(&self) -> bool {
        // 检查规则是否启用
        if !self.rules.place_within_backdrop {
            return false;
        }

        // 只允许背景层
        if self.surface.layer() != Layer::Background {
            return false;
        }

        // 检查独占区域设置
        let state = self.surface.cached_state();
        if state.exclusive_zone != ExclusiveZone::DontCare {
            return false;
        }

        true
    }

    // 函数: bob_offset
    // 作用: 计算浮动动画偏移量
    pub fn bob_offset(&self) -> Point<f64, Logical> {
        // 检查是否启用浮动动画
        if !self.rules.baba_is_float {
            return Point::from((0., 0.));
        }

        // 计算Y轴偏移量
        let y = baba_is_float_offset(self.clock.now(), self.view_size.h);
        // 四舍五入到物理像素
        let y = round_logical_in_physical(self.scale, y);
        Point::from((0., y))
    }

    // 函数: render
    // 作用: 渲染层表面及其所有元素
    // 参数:
    //   renderer - 渲染器实例
    //   location - 渲染位置
    //   target - 渲染目标类型
    // 返回: SplitElements - 分类的渲染元素集合
    // 流程图:
    //   [开始]
    //   -> 计算浮动偏移
    //   -> 检查是否需要阻止渲染:
    //        |-> 是: 渲染纯色块
    //        |-> 否: 渲染实际表面和弹出窗口
    //   -> 添加阴影
    //   -> 返回渲染元素集合
    pub fn render<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        location: Point<f64, Logical>,
        target: RenderTarget,
    ) -> SplitElements<LayerSurfaceRenderElement<R>> {
        // 创建空的渲染元素集合
        let mut rv = SplitElements::default();

        // 创建缩放对象
        let scale = Scale::from(self.scale);
        // 获取不透明度（限制在0-1范围内）
        let alpha = self.rules.opacity.unwrap_or(1.).clamp(0., 1.);
        // 应用浮动偏移
        let location = location + self.bob_offset();

        // 检查是否需要阻止渲染
        if target.should_block_out(self.rules.block_out_from) {
            // 四舍五入位置到物理像素
            let location = location.to_physical_precise_round(scale).to_logical(scale);

            // 创建纯色渲染元素
            // FIXME: 考虑geometry-corner-radius
            let elem = SolidColorRenderElement::from_buffer(
                &self.block_out_buffer,
                location,
                alpha,
                Kind::Unspecified,
            );
            // 添加到普通元素列表
            rv.normal.push(elem.into());
        } else {
            // 计算表面位置
            let buf_pos = location;

            // 获取主表面
            let surface = self.surface.wl_surface();
            // 处理所有弹出窗口
            // Wayland概念: Popup - 临时弹出窗口
            for (popup, popup_offset) in PopupManager::popups_for_surface(surface) {
                // 计算弹出窗口偏移
                let offset = popup_offset - popup.geometry().loc;

                // 渲染弹出窗口表面树
                rv.popups.extend(render_elements_from_surface_tree(
                    renderer,
                    popup.wl_surface(),
                    (buf_pos + offset.to_f64()).to_physical_precise_round(scale),
                    scale,
                    alpha,
                    Kind::Unspecified,
                ));
            }

            // 渲染主表面树
            rv.normal = render_elements_from_surface_tree(
                renderer,
                surface,
                buf_pos.to_physical_precise_round(scale),
                scale,
                alpha,
                Kind::Unspecified,
            );
        }

        // 渲染阴影
        let location = location.to_physical_precise_round(scale).to_logical(scale);
        rv.normal
            .extend(self.shadow.render(renderer, location).map(Into::into));

        rv
    }
}