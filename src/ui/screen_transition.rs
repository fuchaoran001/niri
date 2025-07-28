// 文件: screen_transition.rs
// 作用: 屏幕过渡动画模块，实现合成器在不同状态间切换时的淡入淡出效果
// 关键概念:
//   - 跨渲染目标: 同时支持输出显示、屏幕投射和屏幕捕获
//   - 交叉淡入淡出: 平滑过渡两个屏幕状态
//   - 时间驱动: 基于单调时钟控制动画进度

use std::time::Duration;  // 时间间隔表示

use smithay::backend::renderer::element::Kind;  // Smithay渲染元素类型
use smithay::backend::renderer::gles::GlesTexture;  // OpenGL ES纹理
use smithay::utils::{Scale, Transform};  // 缩放和变换工具

use crate::animation::Clock;  // 动画时钟
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;  // 主GPU纹理元素
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};  // 纹理渲染元素
use crate::render_helpers::RenderTarget;  // 渲染目标枚举

// 动画参数常量
pub const DELAY: Duration = Duration::from_millis(250);  // 动画开始前的延迟
pub const DURATION: Duration = Duration::from_millis(500);  // 动画持续时间

/// 屏幕过渡动画组件
#[derive(Debug)]  // 自动派生Debug trait便于调试
pub struct ScreenTransition {
    /// 每个渲染目标的源纹理(用于交叉淡入淡出)
    /// 索引对应:
    ///   [0] = RenderTarget::Output (主输出)
    ///   [1] = RenderTarget::Screencast (屏幕投射)
    ///   [2] = RenderTarget::ScreenCapture (屏幕捕获)
    from_texture: [TextureBuffer<GlesTexture>; 3],
    
    /// 单调时间: 动画开始的时间点
    start_at: Duration,
    
    /// 动画时钟
    clock: Clock,
}

impl ScreenTransition {
    /// 创建新的屏幕过渡动画
    /// 参数:
    ///   from_texture - 三个渲染目标的源纹理
    ///   delay - 动画开始前的延迟
    ///   clock - 共享时钟
    pub fn new(
        from_texture: [TextureBuffer<GlesTexture>; 3],
        delay: Duration,
        clock: Clock,
    ) -> Self {
        Self {
            from_texture,
            // 计算动画开始时间: 当前时间 + 延迟
            start_at: clock.now_unadjusted() + delay,
            clock,
        }
    }

    /// 检查动画是否完成
    pub fn is_done(&self) -> bool {
        // 当前时间 >= 开始时间 + 持续时间
        self.start_at + DURATION <= self.clock.now_unadjusted()
    }

    /// 更新纹理的缩放和变换(当输出配置改变时调用)
    /// 作用: 确保过渡纹理始终覆盖整个屏幕
    pub fn update_render_elements(&mut self, scale: Scale<f64>, transform: Transform) {
        // 更新所有三个纹理的缩放和变换
        for buffer in &mut self.from_texture {
            buffer.set_texture_scale(scale);
            buffer.set_texture_transform(transform);
        }
    }

    /// 渲染当前动画状态到指定目标
    /// 流程图:
    ///   [获取当前时间] -> 
    ///   [计算透明度alpha] -> 
    ///   [选择对应目标的纹理] -> 
    ///   [创建带透明度的渲染元素]
    pub fn render(&self, target: RenderTarget) -> PrimaryGpuTextureRenderElement {
        // 使用未调整的时间(忽略动画减速)
        let now = self.clock.now_unadjusted();

        // 计算当前透明度(0.0=完全透明, 1.0=完全不透明)
        let alpha = if self.start_at + DURATION <= now {
            // 动画已完成: 完全透明
            0.
        } else if self.start_at <= now {
            // 动画进行中: 线性递减(1.0 -> 0.0)
            let elapsed = (now - self.start_at).as_secs_f32();
            1. - elapsed / DURATION.as_secs_f32()
        } else {
            // 动画尚未开始: 完全不透明
            1.
        };

        // 根据渲染目标选择对应纹理
        let idx = match target {
            RenderTarget::Output => 0,         // 主显示输出
            RenderTarget::Screencast => 1,     // 屏幕投射(如录屏)
            RenderTarget::ScreenCapture => 2,   // 屏幕捕获(如截图)
        };

        // 创建渲染元素
        PrimaryGpuTextureRenderElement(TextureRenderElement::from_texture_buffer(
            self.from_texture[idx].clone(),  // 克隆纹理引用
            (0., 0.),                        // 覆盖整个屏幕
            alpha,                            // 当前透明度
            None,                             // 无裁剪区域
            None,                             // 无额外变换
            Kind::Unspecified,                // 元素类型
        ))
    }
}

/* 工作原理说明:
   1. 创建过渡动画时捕获当前屏幕纹理
   2. 动画开始前: 显示源纹理(alpha=1.0)
   3. 动画进行中: 源纹理逐渐透明(alpha 1.0->0.0)
   4. 动画完成后: 源纹理完全透明(alpha=0.0)

   同时在新图层上渲染新内容，形成交叉淡入淡出效果:
     [源纹理(逐渐透明)] 
     +
     [新内容(完全不透明)]
     =
     平滑过渡效果
*/