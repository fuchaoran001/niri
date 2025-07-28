// 文件: ui/config_error_notification.rs
// 作用: 配置错误通知UI组件，在配置解析失败时显示通知消息
// 关键概念:
//   - Cairo/Pango: 2D图形和文本渲染库
//   - 动画系统: 平滑的显示/隐藏过渡效果
//   - 多输出支持: 为不同缩放比例输出缓存渲染结果

use std::cell::RefCell;  // Rust概念: 内部可变性容器(单线程)
use std::collections::HashMap;  // Rust标准库: 键值对集合
use std::path::{Path, PathBuf};  // Rust路径处理
use std::rc::Rc;  // Rust概念: 引用计数智能指针
use std::time::Duration;  // 时间间隔表示

use niri_config::Config;  // niri配置解析
use ordered_float::NotNan;  // 保证非NaN的浮点数，用于HashMap键
use pangocairo::cairo::{self, ImageSurface};  // Cairo图形库
use pangocairo::pango::FontDescription;  // Pango字体描述
use smithay::backend::renderer::element::Kind;  // Smithay渲染元素类型
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};  // OpenGL ES渲染器
use smithay::output::Output;  // Wayland输出(显示器)
use smithay::reexports::gbm::Format as Fourcc;  // 图形缓冲区格式
use smithay::utils::{Point, Transform};  // 几何工具

use crate::animation::{Animation, Clock};  // 动画系统
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;  // 主GPU纹理元素
use crate::render_helpers::renderer::NiriRenderer;  // niri渲染器trait
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};  // 纹理渲染元素
use crate::utils::{output_size, to_physical_precise_round};  // 工具函数

// 通知文本内容(HTML标记)
const TEXT: &str = "Failed to parse the config file. \
                    Please run <span face='monospace' bgcolor='#000000'>niri validate</span> \
                    to see the errors.";
const PADDING: i32 = 8;      // 内边距
const FONT: &str = "sans 14px";  // 默认字体
const BORDER: i32 = 4;       // 边框宽度

/// 配置错误通知组件
pub struct ConfigErrorNotification {
    // 通知当前状态(隐藏/显示中/显示/隐藏中)
    state: State,
    
    // 按缩放比例缓存的纹理
    // 使用RefCell实现内部可变性，HashMap键为缩放比例(保证非NaN)
    // 合成器概念: 缓存纹理避免重复渲染相同内容
    buffers: RefCell<HashMap<NotNan<f64>, Option<TextureBuffer<GlesTexture>>>>,

    // 如果设置，表示"已在{path}创建配置"的通知
    // 如果未设置，表示配置错误通知
    created_path: Option<PathBuf>,

    clock: Clock,        // 动画时钟
    config: Rc<RefCell<Config>>,  // 共享配置(使用Rc+RefCell实现共享可变)
}

// 通知状态枚举
enum State {
    Hidden,  // 完全隐藏
    Showing(Animation),  // 显示动画中
    Shown(Duration),     // 完全显示(带自动隐藏倒计时)
    Hiding(Animation),   // 隐藏动画中
}

impl ConfigErrorNotification {
    /// 创建新的通知实例
    pub fn new(clock: Clock, config: Rc<RefCell<Config>>) -> Self {
        Self {
            state: State::Hidden,
            buffers: RefCell::new(HashMap::new()),
            created_path: None,
            clock,
            config,
        }
    }

    // 创建动画实例
    fn animation(&self, from: f64, to: f64) -> Animation {
        let c = self.config.borrow();  // 借用配置
        Animation::new(
            self.clock.clone(),
            from,
            to,
            0.,
            c.animations.config_notification_open_close.0,  // 从配置获取动画时长
        )
    }

    /// 显示"配置已创建"通知
    pub fn show_created(&mut self, created_path: PathBuf) {
        let created_path = Some(created_path);
        // 路径变化时清除缓存
        if self.created_path != created_path {
            self.created_path = created_path;
            self.buffers.borrow_mut().clear();
        }

        // 启动显示动画
        self.state = State::Showing(self.animation(0., 1.));
    }

    /// 显示配置错误通知
    pub fn show(&mut self) {
        // 从"已创建"切换到错误通知时清除缓存
        if self.created_path.is_some() {
            self.created_path = None;
            self.buffers.borrow_mut().clear();
        }

        // 启动显示动画(即使正在显示也重新开始以引起注意)
        self.state = State::Showing(self.animation(0., 1.));
    }

    /// 隐藏通知
    pub fn hide(&mut self) {
        // 已隐藏则直接返回
        if matches!(self.state, State::Hidden) {
            return;
        }

        // 启动隐藏动画
        self.state = State::Hiding(self.animation(1., 0.));
    }

    /// 推进动画状态
    pub fn advance_animations(&mut self) {
        match &mut self.state {
            State::Hidden => (),
            State::Showing(anim) => {
                // 动画完成时切换到完全显示状态
                if anim.is_done() {
                    let duration = if self.created_path.is_some() {
                        // "配置已创建"通知显示更长时间
                        Duration::from_secs(8)
                    } else {
                        // 错误通知显示4秒
                        Duration::from_secs(4)
                    };
                    self.state = State::Shown(self.clock.now() + duration);
                }
            }
            State::Shown(deadline) => {
                // 倒计时结束启动隐藏
                if self.clock.now() >= *deadline {
                    self.hide();
                }
            }
            State::Hiding(anim) => {
                // 隐藏动画完成时切换到完全隐藏
                if anim.is_clamped_done() {
                    self.state = State::Hidden;
                }
            }
        }
    }

    /// 检查是否有动画在进行中
    pub fn are_animations_ongoing(&self) -> bool {
        !matches!(self.state, State::Hidden)
    }

    /// 渲染通知到指定输出
    pub fn render<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        output: &Output,
    ) -> Option<PrimaryGpuTextureRenderElement> {
        // 隐藏状态不渲染
        if matches!(self.state, State::Hidden) {
            return None;
        }

        // 获取输出缩放比例和尺寸
        let scale = output.current_scale().fractional_scale();
        let output_size = output_size(output);
        let path = self.created_path.as_deref();

        // 获取或创建对应缩放的纹理缓存
        let mut buffers = self.buffers.borrow_mut();
        let buffer = buffers
            .entry(NotNan::new(scale).unwrap())
            .or_insert_with(move || render(renderer.as_gles_renderer(), scale, path).ok());
        let buffer = buffer.clone()?;  // 克隆Arc引用

        // 计算通知位置(居中显示)
        let size = buffer.logical_size();
        let y_range = size.h + f64::from(PADDING) * 2.;

        let x = (output_size.w - size.w).max(0.) / 2.;
        let y = match &self.state {
            State::Hidden => unreachable!(),
            // 动画状态: 从屏幕顶部滑入
            State::Showing(anim) | State::Hiding(anim) => -size.h + anim.value() * y_range,
            // 完全显示状态: 固定在顶部
            State::Shown(_) => f64::from(PADDING) * 2.,
        };

        // 转换为逻辑坐标
        let location = Point::from((x, y));
        let location = location.to_physical_precise_round(scale).to_logical(scale);

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

/// 渲染通知纹理
fn render(
    renderer: &mut GlesRenderer,
    scale: f64,
    created_path: Option<&Path>,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    // 性能分析: 跟踪渲染耗时
    let _span = tracy_client::span!("config_error_notification::render");

    // 根据缩放比例调整内边距
    let padding: i32 = to_physical_precise_round(scale, PADDING);

    // 根据通知类型设置文本和边框颜色
    let mut text = String::from(TEXT);
    let mut border_color = (1., 0.3, 0.3);  // 错误通知边框(红色)
    if let Some(path) = created_path {
        text = format!(
            "Created a default config file at \
             <span face='monospace' bgcolor='#000000'>{:?}</span>",
            path
        );
        border_color = (0.5, 1., 0.5);  // 创建通知边框(绿色)
    };

    // 设置字体(根据缩放调整大小)
    let mut font = FontDescription::from_string(FONT);
    font.set_absolute_size(to_physical_precise_round(scale, font.size()));

    // 步骤1: 创建临时surface测量文本尺寸
    let surface = ImageSurface::create(cairo::Format::ARgb32, 0, 0)?;
    let cr = cairo::Context::new(&surface)?;
    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);  // 精确像素定位
    layout.set_font_description(Some(&font));
    layout.set_markup(&text);  // 解析HTML标记

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
    layout.set_markup(&text);
    cr.set_source_rgb(1., 1., 1.);  // 白色文本
    pangocairo::functions::show_layout(&cr, &layout);

    // 绘制边框
    cr.move_to(0., 0.);
    cr.line_to(width.into(), 0.);
    cr.line_to(width.into(), height.into());
    cr.line_to(0., height.into());
    cr.line_to(0., 0.);
    cr.set_source_rgb(border_color.0, border_color.1, border_color.2);
    // 根据缩放调整边框宽度(保持锐利)
    cr.set_line_width((f64::from(BORDER) / 2. * scale).round() * 2.);
    cr.stroke()?;
    drop(cr);  // 显式释放cairo上下文

    // 步骤3: 将Cairo surface转为OpenGL纹理
    let data = surface.take_data().unwrap();
    let buffer = TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Argb8888,  // ARGB格式
        (width, height),
        false,  // 不透明
        scale,
        Transform::Normal,  // 无旋转
        Vec::new(),  // 无额外损伤区域
    )?;

    Ok(buffer)
}