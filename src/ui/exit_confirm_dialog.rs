
// 文件: ui/exit_confirm_dialog.rs
// 作用: 退出确认对话框UI组件，在用户尝试退出时显示确认提示
// 关键概念:
//   - 模态对话框: 覆盖其他UI元素，需要用户交互
//   - 多比例渲染: 为不同缩放比例缓存渲染结果
//   - 中心定位: 在屏幕中央显示对话框

use std::cell::RefCell;  // Rust概念: 内部可变性容器(单线程)
use std::collections::HashMap;  // Rust标准库: 键值对集合

use ordered_float::NotNan;  // 保证非NaN的浮点数，用于HashMap键
use pangocairo::cairo::{self, ImageSurface};  // Cairo图形库
use pangocairo::pango::{Alignment, FontDescription};  // Pango字体描述和对齐
use smithay::backend::renderer::element::Kind;  // Smithay渲染元素类型
use smithay::output::Output;  // Wayland输出(显示器)
use smithay::reexports::gbm::Format as Fourcc;  // 图形缓冲区格式
use smithay::utils::Transform;  // 几何变换

use crate::render_helpers::memory::MemoryBuffer;  // 内存缓冲区(存储像素数据)
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;  // 主GPU纹理元素
use crate::render_helpers::renderer::NiriRenderer;  // niri渲染器trait
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};  // 纹理渲染元素
use crate::utils::{output_size, to_physical_precise_round};  // 工具函数


// 对话框文本内容(HTML标记)
const TEXT: &str = "Are you sure you want to exit niri?\n\n\
                    Press <span face='mono' bgcolor='#2C2C2C'> Enter </span> to confirm.";
const PADDING: i32 = 16;     // 内边距
const FONT: &str = "sans 14px";  // 默认字体
const BORDER: i32 = 8;       // 边框宽度

/// 退出确认对话框组件
pub struct ExitConfirmDialog {
    // 对话框是否打开
    is_open: bool,
    
    // 按缩放比例缓存的渲染结果
    // 使用MemoryBuffer存储像素数据，避免重复渲染
    // 合成器概念: 缓存渲染结果提升性能
    buffers: RefCell<HashMap<NotNan<f64>, Option<MemoryBuffer>>>,
}

impl ExitConfirmDialog {
    /// 创建新的对话框实例
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            is_open: false, // 初始状态为关闭
            // 预渲染缩放比例1.0的对话框
            buffers: RefCell::new(HashMap::from([(
                NotNan::new(1.).unwrap(),  // 缩放比例1.0
                Some(render(1.)?),         // 渲染结果
            )])),
        })
    }


    /// 打开对话框
    /// 返回true表示状态改变(从关闭到打开)
    pub fn show(&mut self) -> bool {
        if !self.is_open {
            self.is_open = true;
            true     // 状态改变
        } else {
            false    // 已是打开状态
        }
    }

    pub fn hide(&mut self) -> bool {
        if self.is_open {
            self.is_open = false;
            true
        } else {
            false
        }
    }

    /// 检查对话框是否打开
    pub fn is_open(&self) -> bool {
        self.is_open
    }

    /// 渲染对话框到指定输出
    pub fn render<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        output: &Output,
    ) -> Option<PrimaryGpuTextureRenderElement> {
        // 关闭状态不渲染
        if !self.is_open {
            return None;
        }

        // 获取输出缩放比例和尺寸
        let scale = output.current_scale().fractional_scale();
        let output_size = output_size(output);

        // 获取或创建对应缩放的渲染缓存
        let mut buffers = self.buffers.borrow_mut();
        // 获取缩放比例1.0的缓存作为后备(确保总有内容显示)
        let fallback = buffers[&NotNan::new(1.).unwrap()].clone().unwrap();
        let buffer = buffers
            .entry(NotNan::new(scale).unwrap())
            .or_insert_with(|| render(scale).ok());  // 渲染失败时保留None
        let buffer = buffer.as_ref().unwrap_or(&fallback);  // 使用后备缓存
        
        // 计算对话框位置(屏幕中央)
        let size = buffer.logical_size();
        // 公式: (屏幕尺寸 - 对话框尺寸) / 2
        let buffer = TextureBuffer::from_memory_buffer(renderer.as_gles_renderer(), buffer).ok()?;

        let location = (output_size.to_f64().to_point() - size.to_point()).downscale(2.);
       
        // 确保位置不小于0(防止部分对话框在屏幕外)
        let mut location = location.to_physical_precise_round(scale).to_logical(scale);
        location.x = f64::max(0., location.x);
        location.y = f64::max(0., location.y);

        // 创建渲染元素
        let elem = TextureRenderElement::from_texture_buffer(
            buffer,
            location,
            1.,
            None,
            None,
            Kind::Unspecified,
        );
        Some(PrimaryGpuTextureRenderElement(elem))
    }
}

/// 渲染对话框内容到内存缓冲区
fn render(scale: f64) -> anyhow::Result<MemoryBuffer> {
    // 性能分析: 跟踪渲染耗时
    let _span = tracy_client::span!("exit_confirm_dialog::render");

    // 根据缩放比例调整内边距
    let padding: i32 = to_physical_precise_round(scale, PADDING);

    // 设置字体(根据缩放调整大小)
    let mut font = FontDescription::from_string(FONT);
    font.set_absolute_size(to_physical_precise_round(scale, font.size()));

    // 步骤1: 创建临时surface测量文本尺寸
    let surface = ImageSurface::create(cairo::Format::ARgb32, 0, 0)?;
    let cr = cairo::Context::new(&surface)?;
    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);  // 精确像素定位
    layout.set_font_description(Some(&font));
    layout.set_alignment(Alignment::Center);  // 文本居中对齐
    layout.set_markup(TEXT);  // 解析HTML标记

    // 计算带内边距的最终尺寸
    let (mut width, mut height) = layout.pixel_size();
    width += padding * 2;
    height += padding * 2;

    // 步骤2: 创建实际渲染surface
    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;
    
    // 绘制背景
    cr.set_source_rgb(0.1, 0.1, 0.1);  // 深灰色背景
    cr.paint()?;

    // 绘制文本
    cr.move_to(padding.into(), padding.into());
    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);
    layout.set_font_description(Some(&font));
    layout.set_alignment(Alignment::Center);  // 居中对齐
    layout.set_markup(TEXT);
    cr.set_source_rgb(1., 1., 1.);  // 白色文本
    pangocairo::functions::show_layout(&cr, &layout);

    // 绘制红色边框
    cr.move_to(0., 0.);
    cr.line_to(width.into(), 0.);
    cr.line_to(width.into(), height.into());
    cr.line_to(0., height.into());
    cr.line_to(0., 0.);
    cr.set_source_rgb(1., 0.3, 0.3);  // 红色边框
    // 根据缩放调整边框宽度(保持锐利)
    cr.set_line_width((f64::from(BORDER) / 2. * scale).round() * 2.);
    cr.stroke()?;
    drop(cr);  // 显式释放cairo上下文

    // 步骤3: 创建内存缓冲区
    let data = surface.take_data().unwrap();
    let buffer = MemoryBuffer::new(
        data.to_vec(),      // 像素数据
        Fourcc::Argb8888,   // ARGB格式
        (width, height),    // 尺寸
        scale,              // 缩放比例
        Transform::Normal,  // 无旋转
    );

    Ok(buffer)
}