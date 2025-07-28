// window/mapped.rs
// 此文件实现了已映射窗口的管理逻辑，包括渲染、状态跟踪和交互处理
// 在合成器中，已映射窗口代表用户可见并可交互的窗口实体

use std::cell::{Cell, Ref, RefCell};  // 内部可变性容器
use std::time::Duration;  // 时间间隔

use niri_config::{Color, CornerRadius, GradientInterpolation, WindowRule};  // 配置结构
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;  // 表面渲染
use smithay::backend::renderer::element::Kind;  // 渲染元素类型
use smithay::backend::renderer::gles::GlesRenderer;  // OpenGL渲染器
use smithay::desktop::space::SpaceElement as _;  // 空间元素特性
use smithay::desktop::{PopupManager, Window};  // 桌面窗口和弹出菜单管理
use smithay::output::{self, Output};  // 输出设备
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1;  // 顶层装饰协议
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;  // 顶层协议
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;  // Wayland表面
use smithay::reexports::wayland_server::Resource as _;  // Wayland资源
use smithay::utils::{Logical, Point, Rectangle, Scale, Serial, Size, Transform};  // 实用工具
use smithay::wayland::compositor::{remove_pre_commit_hook, with_states, HookId, SurfaceData};  // 提交钩子
use smithay::wayland::seat::WaylandFocus;  // 焦点管理
use smithay::wayland::shell::xdg::{SurfaceCachedState, ToplevelSurface};  // XDG shell
use wayland_backend::server::Credentials;  // 进程凭证

// 本地模块
use super::{ResolvedWindowRules, WindowRef};  // 窗口规则和引用
use crate::handlers::KdeDecorationsModeState;  // KDE装饰模式
use crate::layout::{  // 布局相关
    ConfigureIntent, InteractiveResizeData, LayoutElement, LayoutElementRenderElement,
    LayoutElementRenderSnapshot,
};
use crate::niri_render_elements;  // 渲染元素宏
use crate::render_helpers::border::BorderRenderElement;  // 边框渲染
use crate::render_helpers::offscreen::OffscreenData;  // 离屏渲染数据
use crate::render_helpers::renderer::NiriRenderer;  // 自定义渲染器
use crate::render_helpers::snapshot::RenderSnapshot;  // 渲染快照
use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};  // 纯色渲染
use crate::render_helpers::surface::render_snapshot_from_surface_tree;  // 表面快照
use crate::render_helpers::{BakedBuffer, RenderTarget, SplitElements};  // 渲染辅助
use crate::utils::id::IdCounter;  // ID生成器
use crate::utils::transaction::Transaction;  // 事务处理
use crate::utils::{  // 实用函数
    get_credentials_for_surface, send_scale_transform, update_tiled_state, with_toplevel_role,
    ResizeEdge,
};

/// 已映射窗口结构体
/// 包含窗口状态、渲染数据和交互逻辑
#[derive(Debug)]
pub struct Mapped {
    /// 底层的Smithay窗口对象
    pub window: Window,

    /// 窗口的唯一ID
    id: MappedId,

    /// 创建此窗口的进程凭证
    credentials: Option<Credentials>,

    /// 预提交钩子ID（用于拦截提交事件）
    pre_commit_hook: HookId,

    /// 当前应用的窗口规则
    rules: ResolvedWindowRules,

    /// 标记是否需要重新计算规则
    need_to_recompute_rules: bool,

    /// 标记是否需要发送配置事件
    needs_configure: bool,

    /// 标记是否需要帧回调
    needs_frame_callback: bool,

    /// 离屏渲染数据（当窗口被移出屏幕时使用）
    offscreen_data: RefCell<Option<OffscreenData>>,

    /// 窗口是否处于紧急状态（需要用户注意）
    is_urgent: bool,

    /// 窗口是否拥有键盘焦点
    is_focused: bool,

    /// 在所在列中是否激活
    is_active_in_column: bool,

    /// 是否为浮动窗口
    is_floating: bool,

    /// 是否为窗口投射目标
    is_window_cast_target: bool,

    /// 是否忽略不透明度规则
    ignore_opacity_window_rule: bool,

    /// 屏蔽渲染时的纯色缓冲区
    block_out_buffer: RefCell<SolidColorBuffer>,

    /// 下次配置是否启用动画
    animate_next_configure: bool,

    /// 需要动画的提交序列号列表
    animate_serials: Vec<Serial>,

    /// 动画前的渲染快照（不含弹出菜单）
    animation_snapshot: Option<LayoutElementRenderSnapshot>,

    /// 一次性尺寸请求状态（用于浮动窗口）
    request_size_once: Option<RequestSizeOnce>,

    /// 下次配置应参与的事务
    transaction_for_next_configure: Option<Transaction>,

    /// 待处理的事务列表
    pending_transactions: Vec<(Serial, Transaction)>,

    /// 交互式调整大小状态
    interactive_resize: Option<InteractiveResize>,

    /// 上次交互式调整开始时间（用于双击检测）
    last_interactive_resize_start: Cell<Option<(Duration, ResizeEdge)>>,

    /// 是否处于窗口化全屏模式（伪全屏）
    is_windowed_fullscreen: bool,

    /// 是否等待进入窗口化全屏
    is_pending_windowed_fullscreen: bool,

    /// 待提交的窗口化全屏状态列表
    uncommited_windowed_fullscreen: Vec<(Serial, bool)>,
}

// 定义渲染元素类型（用于窗口投射）
niri_render_elements! {
    WindowCastRenderElements<R> => {
        Layout = LayoutElementRenderElement<R>,  // 布局元素
        Border = BorderRenderElement,            // 带圆角的屏蔽窗口
    }
}

// 全局ID计数器
static MAPPED_ID_COUNTER: IdCounter = IdCounter::new();

/// 窗口唯一ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MappedId(u64);

impl MappedId {
    /// 生成下一个ID
    pub fn next() -> MappedId {
        MappedId(MAPPED_ID_COUNTER.next())
    }

    /// 获取原始ID值
    pub fn get(self) -> u64 {
        self.0
    }
}

/// 交互式调整大小状态枚举
#[derive(Debug)]
enum InteractiveResize {
    /// 调整进行中
    Ongoing(InteractiveResizeData),
    /// 等待发送最终配置
    WaitingForLastConfigure(InteractiveResizeData),
    /// 等待最终提交
    WaitingForLastCommit {
        data: InteractiveResizeData,
        serial: Serial,  // 配置序列号
    },
}

impl InteractiveResize {
    /// 获取调整数据
    fn data(&self) -> InteractiveResizeData {
        match self {
            InteractiveResize::Ongoing(data) => *data,
            InteractiveResize::WaitingForLastConfigure(data) => *data,
            InteractiveResize::WaitingForLastCommit { data, .. } => *data,
        }
    }
}

/// 一次性尺寸请求状态
#[derive(Debug, Clone, Copy)]
enum RequestSizeOnce {
    /// 等待发送配置
    WaitingForConfigure,
    /// 等待窗口提交
    WaitingForCommit(Serial),  // 序列号
    /// 使用窗口当前尺寸
    UseWindowSize,
}

impl Mapped {
    // 创建新的已映射窗口
    pub fn new(window: Window, rules: ResolvedWindowRules, hook: HookId) -> Self {
        // 获取窗口的Wayland表面
        let surface = window.wl_surface().expect("no X11 support");
        // 获取创建此表面的进程凭证
        let credentials = get_credentials_for_surface(&surface);

        // 初始化并返回Mapped实例
        Self {
            window,
            id: MappedId::next(),  // 生成唯一ID
            credentials,
            pre_commit_hook: hook,  // 保存预提交钩子
            rules,  // 初始规则
            need_to_recompute_rules: false,
            needs_configure: false,
            needs_frame_callback: false,
            offscreen_data: RefCell::new(None),  // 无离屏数据
            is_urgent: false,
            is_focused: false,
            is_active_in_column: true,  // 默认在列中激活
            is_floating: false,
            is_window_cast_target: false,
            ignore_opacity_window_rule: false,
            // 创建黑色屏蔽缓冲区
            block_out_buffer: RefCell::new(SolidColorBuffer::new((0., 0.), [0., 0., 0., 1.])),
            animate_next_configure: false,
            animate_serials: Vec::new(),
            animation_snapshot: None,
            request_size_once: None,
            transaction_for_next_configure: None,
            pending_transactions: Vec::new(),
            interactive_resize: None,
            last_interactive_resize_start: Cell::new(None),
            is_windowed_fullscreen: false,
            is_pending_windowed_fullscreen: false,
            uncommited_windowed_fullscreen: Vec::new(),
        }
    }

    // 获取底层ToplevelSurface
    pub fn toplevel(&self) -> &ToplevelSurface {
        self.window.toplevel().expect("no X11 support")
    }

    /// 重新计算窗口规则并返回是否更改
    pub fn recompute_window_rules(&mut self, rules: &[WindowRule], is_at_startup: bool) -> bool {
        self.need_to_recompute_rules = false;  // 重置标志

        // 计算新规则
        let new_rules = ResolvedWindowRules::compute(rules, WindowRef::Mapped(self), is_at_startup);
        if new_rules == self.rules {
            return false;  // 无变化
        }

        // 如果新规则不再设置半透明，重置忽略标志
        if !new_rules.opacity.is_some_and(|o| o < 1.) {
            self.ignore_opacity_window_rule = false;
        }

        self.rules = new_rules;  // 更新规则
        true  // 规则已更改
    }

    // 如果需要则重新计算规则
    pub fn recompute_window_rules_if_needed(
        &mut self,
        rules: &[WindowRule],
        is_at_startup: bool,
    ) -> bool {
        if !self.need_to_recompute_rules {
            return false;
        }

        self.recompute_window_rules(rules, is_at_startup)
    }

    // 标记需要配置事件
    pub fn set_needs_configure(&mut self) {
        self.needs_configure = true;
    }

    // 获取窗口ID
    pub fn id(&self) -> MappedId {
        self.id
    }

    // 获取进程凭证
    pub fn credentials(&self) -> Option<&Credentials> {
        self.credentials.as_ref()
    }

    // 获取离屏数据引用
    pub fn offscreen_data(&self) -> Ref<Option<OffscreenData>> {
        self.offscreen_data.borrow()
    }

    // 检查是否聚焦
    pub fn is_focused(&self) -> bool {
        self.is_focused
    }

    // 检查在列中是否激活
    pub fn is_active_in_column(&self) -> bool {
        self.is_active_in_column
    }

    // 检查是否浮动
    pub fn is_floating(&self) -> bool {
        self.is_floating
    }

    // 检查是否为投屏目标
    pub fn is_window_cast_target(&self) -> bool {
        self.is_window_cast_target
    }

    // 切换忽略不透明度规则
    pub fn toggle_ignore_opacity_window_rule(&mut self) {
        self.ignore_opacity_window_rule = !self.ignore_opacity_window_rule;
    }

    // 设置聚焦状态
    pub fn set_is_focused(&mut self, is_focused: bool) {
        if self.is_focused == is_focused {
            return;
        }

        self.is_focused = is_focused;
        self.is_urgent = false;  // 聚焦时清除紧急状态
        self.need_to_recompute_rules = true;  // 标记需要重新计算规则
    }

    // 设置投屏目标状态
    pub fn set_is_window_cast_target(&mut self, value: bool) {
        if self.is_window_cast_target == value {
            return;
        }

        self.is_window_cast_target = value;
        self.need_to_recompute_rules = true;
    }

    /// 渲染不含弹出菜单的窗口快照
    fn render_snapshot(&self, renderer: &mut GlesRenderer) -> LayoutElementRenderSnapshot {
        let _span = tracy_client::span!("Mapped::render_snapshot");

        // 获取窗口尺寸
        let size = self.size().to_f64();

        // 准备屏蔽缓冲区
        let mut buffer = self.block_out_buffer.borrow_mut();
        buffer.resize(size);
        let blocked_out_contents = vec![BakedBuffer {
            buffer: buffer.clone(),
            location: Point::from((0., 0.)),
            src: None,
            dst: None,
        }];

        // 计算缓冲区位置（考虑缩放）
        let buf_pos = self.window.geometry().loc.upscale(-1).to_f64();

        // 渲染表面内容
        let mut contents = vec![];
        let surface = self.toplevel().wl_surface();
        render_snapshot_from_surface_tree(renderer, surface, buf_pos, &mut contents);

        // 返回快照结构
        RenderSnapshot {
            contents,
            blocked_out_contents,
            block_out_from: self.rules().block_out_from,
            size,
            texture: Default::default(),
            blocked_out_texture: Default::default(),
        }
    }

    // 检查提交是否需要动画
    pub fn should_animate_commit(&mut self, commit_serial: Serial) -> bool {
        let mut should_animate = false;
        // 保留需要动画的序列号
        self.animate_serials.retain_mut(|serial| {
            if commit_serial.is_no_older_than(serial) {
                should_animate = true;
                false  // 移除已处理的序列号
            } else {
                true   // 保留未处理的序列号
            }
        });
        should_animate
    }

    // 存储动画快照
    pub fn store_animation_snapshot(&mut self, renderer: &mut GlesRenderer) {
        self.animation_snapshot = Some(self.render_snapshot(renderer));
    }

    // 获取待处理事务
    pub fn take_pending_transaction(&mut self, commit_serial: Serial) -> Option<Transaction> {
        let mut rv = None;

        // 按序列号顺序处理待处理事务
        while let Some((serial, _)) = self.pending_transactions.first() {
            // 处理当前及更早的提交
            if commit_serial.is_no_older_than(serial) {
                let (_, transaction) = self.pending_transactions.remove(0);
                rv = Some(transaction);
            } else {
                break;
            }
        }

        rv
    }

    // 获取上次交互调整开始时间
    pub fn last_interactive_resize_start(&self) -> &Cell<Option<(Duration, ResizeEdge)>> {
        &self.last_interactive_resize_start
    }

    /// 为屏幕投射渲染窗口
    pub fn render_for_screen_cast<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        scale: Scale<f64>,
    ) -> impl DoubleEndedIterator<Item = WindowCastRenderElements<R>> {
        // 计算边界框（含弹出菜单）
        let bbox = self.window.bbox_with_popups().to_physical_precise_up(scale);

        // 检查是否支持边框着色器
        let has_border_shader = BorderRenderElement::has_shader(renderer);
        let rules = self.rules();
        // 获取圆角半径（应用规则或默认）
        let radius = rules.geometry_corner_radius.unwrap_or_default();
        let window_size = self
            .size()
            .to_f64()
            .to_physical_precise_round(scale)
            .to_logical(scale);
        let radius = radius.fit_to(window_size.w as f32, window_size.h as f32);

        // 计算渲染位置
        let location = self.window.geometry().loc.to_f64() - bbox.loc.to_logical(scale);
        // 渲染元素
        let elements = self.render(renderer, location, scale, 1., RenderTarget::Screencast);

        // 转换元素为投屏格式
        elements.into_iter().map(move |elem| {
            if let LayoutElementRenderElement::SolidColor(elem) = &elem {
                // 处理屏蔽渲染的圆角
                if radius != CornerRadius::default() && has_border_shader {
                    let geo = elem.geo();
                    return BorderRenderElement::new(
                        geo.size,
                        Rectangle::from_size(geo.size),
                        GradientInterpolation::default(),
                        Color::from_color32f(elem.color()),
                        Color::from_color32f(elem.color()),
                        0.,
                        Rectangle::from_size(geo.size),
                        0.,
                        radius,
                        scale.x as f32,
                        1.,
                    )
                    .with_location(geo.loc)
                    .into();
                }
            }

            // 默认转换
            WindowCastRenderElements::from(elem)
        })
    }

    /// 发送帧回调
    pub fn send_frame<T, F>(
        &mut self,
        output: &Output,
        time: T,
        throttle: Option<Duration>,
        mut primary_scan_out_output: F,
    ) where
        T: Into<Duration>,
        F: FnMut(&WlSurface, &SurfaceData) -> Option<Output> + Copy,
    {
        let needs_frame_callback = self.needs_frame_callback;
        self.needs_frame_callback = false;

        // 决定是否发送帧回调
        let should_send = move |surface: &WlSurface, states: &SurfaceData| {
            // 检查主扫描输出
            if let Some(output) = primary_scan_out_output(surface, states) {
                return Some(output);
            }

            // 如果需要则发送给所有表面
            needs_frame_callback.then(|| output.clone())
        };
        self.window.send_frame(output, time, throttle, should_send);
    }

    // 更新平铺状态
    pub fn update_tiled_state(&self, prefer_no_csd: bool) {
        update_tiled_state(self.toplevel(), prefer_no_csd, self.rules.tiled_state);
    }

    // 检查是否窗口化全屏
    pub fn is_windowed_fullscreen(&self) -> bool {
        self.is_windowed_fullscreen
    }

    // 设置紧急状态
    pub fn set_urgent(&mut self, urgent: bool) {
        // 已聚焦窗口不能设为紧急
        if self.is_focused && urgent {
            return;
        }

        let changed = self.is_urgent != urgent;
        self.is_urgent = urgent;
        self.need_to_recompute_rules |= changed;
    }

    // 检查是否紧急
    pub fn is_urgent(&self) -> bool {
        self.is_urgent
    }
}

// 析构函数实现
impl Drop for Mapped {
    fn drop(&mut self) {
        // 移除预提交钩子
        remove_pre_commit_hook(self.toplevel().wl_surface(), self.pre_commit_hook.clone());
    }
}

/* 关键功能详解
1. 生命周期管理:
   - new(): 创建窗口时初始化状态
   - drop(): 销毁时移除Wayland钩子
   - set_is_focused(): 更新焦点状态并重置紧急状态

2. 规则系统:
   - recompute_window_rules(): 动态更新规则
   - toggle_ignore_opacity_window_rule(): 用户覆盖不透明度规则

3. 渲染系统:
   - render_snapshot(): 创建不含弹出菜单的快照
   - render_for_screen_cast(): 为投屏优化渲染
   - store_animation_snapshot(): 存储动画起始状态

4. 事务管理:
   - take_pending_transaction(): 处理异步事务
   - send_frame(): 协调帧回调

5. 状态同步:
   - update_tiled_state(): 同步Wayland平铺状态
   - set_urgent(): 管理紧急状态逻辑

6. 窗口系统集成:
   - toplevel(): 访问底层Wayland对象
   - credentials(): 获取创建者进程信息

设计模式:
   - 状态机: interactive_resize管理调整大小流程
   - 观察者模式: need_to_recompute_rules标记规则更新
   - 快照模式: animation_snapshot保存动画起始状态
   - 事务模式: pending_transactions处理异步操作

性能考虑:
   - 离屏渲染: offscreen_data避免不可见窗口的渲染
   - 动画优化: animate_serials管理动画序列
   - 资源复用: block_out_buffer重用屏蔽缓冲区
*/

impl LayoutElement for Mapped {
    // 作用：为 Mapped 类型实现 LayoutElement trait，使其可作为布局元素参与合成器的布局和渲染流程
    // 说明：LayoutElement 是合成器核心抽象，代表可布局渲染的窗口/元素
    // Rust机制：trait 实现 - 为特定类型定义接口方法集合
    type Id = Window;
    // 作用：定义布局元素的唯一标识类型为 Window
    // 说明：用于在布局系统中唯一识别窗口实例

    fn id(&self) -> &Self::Id {
        // 作用：返回窗口的标识符
        &self.window
    }

    fn size(&self) -> Size<i32, Logical> {
        // 作用：获取窗口的当前逻辑尺寸
        // 合成器逻辑：从窗口几何信息中提取尺寸用于布局计算
        // 说明：Logical 表示逻辑坐标（独立于缩放）
        self.window.geometry().size
    }

    fn buf_loc(&self) -> Point<i32, Logical> {
        // 作用：计算缓冲区原点相对于窗口位置的偏移
        // 说明：用于将窗口坐标系转换为缓冲区坐标系
        // Wayland协议：surface 有自己的坐标系系统
        Point::from((0, 0)) - self.window.geometry().loc
    }

    fn is_in_input_region(&self, point: Point<f64, Logical>) -> bool {
        // 作用：检测输入事件坐标是否在窗口的输入区域内
        // 合成器逻辑：将全局坐标转换为窗口本地坐标后检测
        let surface_local = point + self.window.geometry().loc.to_f64();
        self.window.is_in_input_region(&surface_local)
    }

    fn render<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        location: Point<f64, Logical>,
        scale: Scale<f64>,
        alpha: f32,
        target: RenderTarget,
    ) -> SplitElements<LayoutElementRenderElement<R>> {
        // 作用：渲染窗口及其所有弹出层
        // 合成器逻辑：
        //   [检测遮挡] → [渲染主窗口] → [收集所有弹出层]
        //   ↓___________________________↑
        let mut rv = SplitElements::default();

        // 检查是否需要遮挡渲染（如最小化状态）
        if target.should_block_out(self.rules.block_out_from) {
            // 创建纯色遮挡缓冲区
            let mut buffer = self.block_out_buffer.borrow_mut();
            buffer.resize(self.window.geometry().size.to_f64());
            let elem =
                SolidColorRenderElement::from_buffer(&buffer, location, alpha, Kind::Unspecified);
            rv.normal.push(elem.into());
        } else {
            // 计算缓冲区渲染位置
            let buf_pos = location - self.window.geometry().loc.to_f64();

            // 获取主 surface
            let surface = self.toplevel().wl_surface();
            // 渲染所有关联的弹出层
            for (popup, popup_offset) in PopupManager::popups_for_surface(surface) {
                 // 计算弹出层位置偏移
                let offset = self.window.geometry().loc + popup_offset - popup.geometry().loc;
                // 递归渲染弹出层树
                rv.popups.extend(render_elements_from_surface_tree(
                    renderer,
                    popup.wl_surface(),
                    (buf_pos + offset.to_f64()).to_physical_precise_round(scale),
                    scale,
                    alpha,
                    Kind::Unspecified,
                ));
            }
            // 渲染主窗口 surface 树
            rv.normal = render_elements_from_surface_tree(
                renderer,
                surface,
                buf_pos.to_physical_precise_round(scale),
                scale,
                alpha,
                Kind::Unspecified,
            );
        }

        rv
    }

    fn render_normal<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        location: Point<f64, Logical>,
        scale: Scale<f64>,
        alpha: f32,
        target: RenderTarget,
    ) -> Vec<LayoutElementRenderElement<R>> {
        if target.should_block_out(self.rules.block_out_from) {
            let mut buffer = self.block_out_buffer.borrow_mut();
            buffer.resize(self.window.geometry().size.to_f64());
            let elem =
                SolidColorRenderElement::from_buffer(&buffer, location, alpha, Kind::Unspecified);
            vec![elem.into()]
        } else {
            let buf_pos = location - self.window.geometry().loc.to_f64();
            let surface = self.toplevel().wl_surface();
            render_elements_from_surface_tree(
                renderer,
                surface,
                buf_pos.to_physical_precise_round(scale),
                scale,
                alpha,
                Kind::Unspecified,
            )
        }
    }

    fn render_popups<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        location: Point<f64, Logical>,
        scale: Scale<f64>,
        alpha: f32,
        target: RenderTarget,
    ) -> Vec<LayoutElementRenderElement<R>> {
        if target.should_block_out(self.rules.block_out_from) {
            vec![]
        } else {
            let mut rv = vec![];

            let buf_pos = location - self.window.geometry().loc.to_f64();
            let surface = self.toplevel().wl_surface();
            for (popup, popup_offset) in PopupManager::popups_for_surface(surface) {
                let offset = self.window.geometry().loc + popup_offset - popup.geometry().loc;

                rv.extend(render_elements_from_surface_tree(
                    renderer,
                    popup.wl_surface(),
                    (buf_pos + offset.to_f64()).to_physical_precise_round(scale),
                    scale,
                    alpha,
                    Kind::Unspecified,
                ));
            }

            rv
        }
    }

    fn request_size(
        &mut self,
        size: Size<i32, Logical>,
        is_fullscreen: bool,
        animate: bool,
        transaction: Option<Transaction>,
    ) {

        // 作用：请求窗口调整到指定尺寸
        // 合成器逻辑：处理全屏状态转换 → 更新待处理状态 → 记录动画需求
        // 流程图：
        //   [全屏状态处理] → [更新配置状态] → [记录动画标记] → [保存事务]
        //        ↓_____________________________↑

        // 处理全屏状态转换
        if is_fullscreen {
            self.is_pending_windowed_fullscreen = false;

            if self.is_windowed_fullscreen {
                // Make sure we receive a commit to update self.is_windowed_fullscreen to false
                // later on.
                self.needs_configure = true;
            }
        }

        // 更新 toplevel 的待处理状态
        let changed = self.toplevel().with_pending_state(|state| {
            let changed = state.size != Some(size);
            state.size = Some(size);
            if is_fullscreen || self.is_pending_windowed_fullscreen {
                state.states.set(xdg_toplevel::State::Fullscreen);
            } else {
                state.states.unset(xdg_toplevel::State::Fullscreen);
            }
            changed
        });


        // 记录是否需要动画
        if changed && animate {
            self.animate_next_configure = true;
        }

        self.request_size_once = None;

        // Store the transaction regardless of whether the size changed. This is because with 3+
        // windows in a column, the size may change among windows 1 and 2 and then right away among
        // windows 2 and 3, and we want all windows 1, 2 and 3 to use the last transaction, rather
        // than window 1 getting stuck with the previous transaction that is immediately released
        // by 2.
        // 保存事务（用于多窗口协同调整）
        if let Some(transaction) = transaction {
            self.transaction_for_next_configure = Some(transaction);
        }
    }

    fn request_size_once(&mut self, size: Size<i32, Logical>, animate: bool) {
        // Assume that when calling this function, the window is going floating, so it can no
        // longer participate in any transactions with other windows.
        self.transaction_for_next_configure = None;

        // If our last requested size already matches the size we want to request-once, clear the
        // size request right away. However, we must also check if we're unfullscreening, because
        // in that case the window itself will restore its previous size upon receiving a (0, 0)
        // configure, whereas what we potentially want is to unfullscreen the window into its
        // fullscreen size.
        let already_sent = with_toplevel_role(self.toplevel(), |role| {
            let (last_sent, last_serial) = if let Some(configure) = role.pending_configures().last()
            {
                // FIXME: it would be more optimal to find the *oldest* pending configure that
                // has the same size and fullscreen state to the last pending configure.
                (&configure.state, configure.serial)
            } else {
                (
                    role.last_acked.as_ref().unwrap(),
                    role.configure_serial.unwrap(),
                )
            };

            let same_size = last_sent.size.unwrap_or_default() == size;
            let has_fullscreen = last_sent.states.contains(xdg_toplevel::State::Fullscreen);
            let same_fullscreen = has_fullscreen == self.is_pending_windowed_fullscreen;
            (same_size && same_fullscreen).then_some(last_serial)
        });

        if let Some(serial) = already_sent {
            if let Some(current_serial) =
                with_toplevel_role(self.toplevel(), |role| role.current_serial)
            {
                // God this triple negative...
                if !current_serial.is_no_older_than(&serial) {
                    // We have already sent a request for the new size, but the surface has not
                    // committed in response yet, so we will wait for that commit.
                    self.request_size_once = Some(RequestSizeOnce::WaitingForCommit(serial));
                } else {
                    // We have already sent a request for the new size, and the surface has
                    // committed in response, so we will start using the current size right away.
                    self.request_size_once = Some(RequestSizeOnce::UseWindowSize);
                }
            } else {
                warn!("no current serial; did the surface not ack the initial configure?");
                self.request_size_once = Some(RequestSizeOnce::UseWindowSize);
            };
            return;
        }

        let changed = self.toplevel().with_pending_state(|state| {
            let changed = state.size != Some(size);
            state.size = Some(size);
            if !self.is_pending_windowed_fullscreen {
                state.states.unset(xdg_toplevel::State::Fullscreen);
            }
            changed
        });

        if changed && animate {
            self.animate_next_configure = true;
        }

        self.request_size_once = Some(RequestSizeOnce::WaitingForConfigure);
    }

    fn min_size(&self) -> Size<i32, Logical> {
        let min_size = with_states(self.toplevel().wl_surface(), |state| {
            let mut guard = state.cached_state.get::<SurfaceCachedState>();
            guard.current().min_size
        });

        self.rules.apply_min_size(min_size)
    }

    fn max_size(&self) -> Size<i32, Logical> {
        let max_size = with_states(self.toplevel().wl_surface(), |state| {
            let mut guard = state.cached_state.get::<SurfaceCachedState>();
            guard.current().max_size
        });

        self.rules.apply_max_size(max_size)
    }

    fn is_wl_surface(&self, wl_surface: &WlSurface) -> bool {
        self.toplevel().wl_surface() == wl_surface
    }

    fn set_preferred_scale_transform(&self, scale: output::Scale, transform: Transform) {
        self.window.with_surfaces(|surface, data| {
            send_scale_transform(surface, data, scale, transform);
        });
    }

    fn has_ssd(&self) -> bool {
        let toplevel = self.toplevel();
        let mode = with_toplevel_role(self.toplevel(), |role| role.current.decoration_mode);

        match mode {
            Some(zxdg_toplevel_decoration_v1::Mode::ServerSide) => true,
            // Check KDE decorations when XDG are not in use.
            None => with_states(toplevel.wl_surface(), |states| {
                states
                    .data_map
                    .get::<KdeDecorationsModeState>()
                    .map(KdeDecorationsModeState::is_server)
                    == Some(true)
            }),
            _ => false,
        }
    }

    fn output_enter(&self, output: &Output) {
        let overlap = Rectangle::from_size(Size::from((i32::MAX, i32::MAX)));
        self.window.output_enter(output, overlap)
    }

    fn output_leave(&self, output: &Output) {
        self.window.output_leave(output)
    }

    fn set_offscreen_data(&self, data: Option<OffscreenData>) {
        let Some(data) = data else {
            self.offscreen_data.replace(None);
            return;
        };

        let mut offscreen_data = self.offscreen_data.borrow_mut();
        match &mut *offscreen_data {
            None => {
                *offscreen_data = Some(data);
            }
            Some(existing) => {
                // Replace the id, amend existing element states. This is necessary to handle
                // multiple layers of offscreen (e.g. resize animation + alpha animation).
                existing.id = data.id;
                existing.states.states.extend(data.states.states);
            }
        }
    }

    fn is_urgent(&self) -> bool {
        self.is_urgent
    }

    fn set_activated(&mut self, active: bool) {
        let changed = self.toplevel().with_pending_state(|state| {
            if active {
                state.states.set(xdg_toplevel::State::Activated)
            } else {
                state.states.unset(xdg_toplevel::State::Activated)
            }
        });
        self.need_to_recompute_rules |= changed;
    }

    fn set_active_in_column(&mut self, active: bool) {
        let changed = self.is_active_in_column != active;
        self.is_active_in_column = active;
        self.need_to_recompute_rules |= changed;
    }

    fn set_floating(&mut self, floating: bool) {
        let changed = self.is_floating != floating;
        self.is_floating = floating;
        self.need_to_recompute_rules |= changed;
    }

    fn set_bounds(&self, bounds: Size<i32, Logical>) {
        self.toplevel().with_pending_state(|state| {
            state.bounds = Some(bounds);
        });
    }

    fn configure_intent(&self) -> ConfigureIntent {
        let _span =
            trace_span!("configure_intent", surface = ?self.toplevel().wl_surface().id()).entered();

        if self.needs_configure {
            trace!("the window needs_configure");
            return ConfigureIntent::ShouldSend;
        }

        with_toplevel_role(self.toplevel(), |attributes| {
            if let Some(server_pending) = &attributes.server_pending {
                let current_server = attributes.current_server_state();
                if server_pending != current_server {
                    // Something changed. Check if the only difference is the size, and if the
                    // current server size matches the current committed size.
                    let mut current_server_same_size = current_server.clone();
                    current_server_same_size.size = server_pending.size;
                    if current_server_same_size == *server_pending {
                        // Only the size changed. Check if the window committed our previous size
                        // request.
                        if attributes.current.size == current_server.size {
                            // The window had committed for our previous size change, so we can
                            // change the size again.
                            trace!(
                                "current size matches server size: {:?}",
                                attributes.current.size
                            );
                            ConfigureIntent::CanSend
                        } else {
                            // The window had not committed for our previous size change yet. Since
                            // nothing else changed, do not send the new size request yet. This
                            // throttling is done because some clients do not batch size requests,
                            // leading to bad behavior with very fast input devices (i.e. a 1000 Hz
                            // mouse). This throttling also helps interactive resize transactions
                            // preserve visual consistency.
                            trace!("throttling resize");
                            ConfigureIntent::Throttled
                        }
                    } else {
                        // Something else changed other than the size; send it.
                        trace!("something changed other than the size");
                        ConfigureIntent::ShouldSend
                    }
                } else {
                    // Nothing changed since the last configure.
                    ConfigureIntent::NotNeeded
                }
            } else {
                // Nothing changed since the last configure.
                ConfigureIntent::NotNeeded
            }
        })
    }

    fn send_pending_configure(&mut self) {
        let toplevel = self.toplevel();
        let _span =
            trace_span!("send_pending_configure", surface = ?toplevel.wl_surface().id()).entered();

        // If the window needs a configure, send it regardless.
        let has_pending_changes = self.needs_configure
            || with_toplevel_role(self.toplevel(), |role| {
                // Check for pending changes manually to account for RequestSizeOnce::UseWindowSize.
                if role.server_pending.is_none() {
                    return false;
                }

                let current_server_size = role.current_server_state().size;
                let server_pending = role.server_pending.as_mut().unwrap();

                // With UseWindowSize, we do not consider size-only changes, because we will
                // request the current window size and do not expect it to actually change.
                if let Some(RequestSizeOnce::UseWindowSize) = self.request_size_once {
                    server_pending.size = current_server_size;
                }

                let server_pending = role.server_pending.as_ref().unwrap();
                server_pending != role.current_server_state()
            });

        if has_pending_changes {
            // If needed, replace the pending size with the current window size.
            if let Some(RequestSizeOnce::UseWindowSize) = self.request_size_once {
                let size = self.window.geometry().size;
                toplevel.with_pending_state(|state| {
                    state.size = Some(size);
                });
            }

            let serial = toplevel.send_configure();
            trace!(?serial, "sending configure");

            self.needs_configure = false;

            // Send the window a frame callback unconditionally to let it respond to size changes
            // and such immediately, even when it's hidden. This especially matters for cases like
            // tabbed columns which compute their width based on all windows in the column, even
            // hidden ones.
            self.needs_frame_callback = true;

            if self.animate_next_configure {
                self.animate_serials.push(serial);
            }

            if let Some(transaction) = self.transaction_for_next_configure.take() {
                self.pending_transactions.push((serial, transaction));
            }

            self.interactive_resize = match self.interactive_resize.take() {
                Some(InteractiveResize::WaitingForLastConfigure(data)) => {
                    Some(InteractiveResize::WaitingForLastCommit { data, serial })
                }
                x => x,
            };

            if let Some(RequestSizeOnce::WaitingForConfigure) = self.request_size_once {
                self.request_size_once = Some(RequestSizeOnce::WaitingForCommit(serial));
            }

            // If is_pending_windowed_fullscreen changed compared to the last value that we "sent"
            // to the window, store the configure serial.
            let last_sent_windowed_fullscreen = self
                .uncommited_windowed_fullscreen
                .last()
                .map(|(_, value)| *value)
                .unwrap_or(self.is_windowed_fullscreen);
            if last_sent_windowed_fullscreen != self.is_pending_windowed_fullscreen {
                self.uncommited_windowed_fullscreen
                    .push((serial, self.is_pending_windowed_fullscreen));
            }
        } else {
            self.interactive_resize = match self.interactive_resize.take() {
                // We probably started and stopped resizing in the same loop cycle without anything
                // changing.
                Some(InteractiveResize::WaitingForLastConfigure { .. }) => None,
                x => x,
            };
        }

        self.animate_next_configure = false;
        self.transaction_for_next_configure = None;
    }

    fn is_fullscreen(&self) -> bool {
        if self.is_windowed_fullscreen {
            return false;
        }

        with_toplevel_role(self.toplevel(), |role| {
            role.current
                .states
                .contains(xdg_toplevel::State::Fullscreen)
        })
    }

    fn is_pending_fullscreen(&self) -> bool {
        if self.is_pending_windowed_fullscreen {
            return false;
        }

        self.toplevel()
            .with_pending_state(|state| state.states.contains(xdg_toplevel::State::Fullscreen))
    }

    fn is_ignoring_opacity_window_rule(&self) -> bool {
        self.ignore_opacity_window_rule
    }

    fn requested_size(&self) -> Option<Size<i32, Logical>> {
        self.toplevel().with_pending_state(|state| state.size)
    }

    fn expected_size(&self) -> Option<Size<i32, Logical>> {
        // We can only use current size if it's not fullscreen.
        let current_size = (!self.is_fullscreen()).then(|| self.window.geometry().size);

        // Check if we should be using the current window size.
        //
        // This branch can be useful (give different result than the logic below) in this example
        // case:
        //
        // 1. We request_size_once a size change.
        // 2. We send a second configure requesting a state change.
        // 3. The window acks and commits-to the first configure but not the second, with a
        //    different size.
        //
        // In this case self.request_size_once will already flip to UseWindowSize and this branch
        // will return the window's own new size, but the logic below would see an uncommitted size
        // change and return our size.
        if let Some(RequestSizeOnce::UseWindowSize) = self.request_size_once {
            return current_size;
        }

        let pending = with_toplevel_role(self.toplevel(), |role| {
            // If we have a server-pending size change that we haven't sent yet, use that size.
            if let Some(server_pending) = &role.server_pending {
                let current_server = role.current_server_state();
                if server_pending.size != current_server.size {
                    return Some((
                        server_pending.size.unwrap_or_default(),
                        server_pending
                            .states
                            .contains(xdg_toplevel::State::Fullscreen),
                    ));
                }
            }

            // If we have a sent-but-not-committed-to size, use that.
            let (last_sent, last_serial) = if let Some(configure) = role.pending_configures().last()
            {
                (&configure.state, configure.serial)
            } else {
                (
                    role.last_acked.as_ref().unwrap(),
                    role.configure_serial.unwrap(),
                )
            };

            if let Some(current_serial) = role.current_serial {
                if !current_serial.is_no_older_than(&last_serial) {
                    return Some((
                        last_sent.size.unwrap_or_default(),
                        last_sent.states.contains(xdg_toplevel::State::Fullscreen),
                    ));
                }
            }

            None
        });

        if let Some((mut size, fullscreen)) = pending {
            // If the pending change is fullscreen, we can't use that size.
            if fullscreen && !self.is_pending_windowed_fullscreen {
                return None;
            }

            // If some component of the pending size is zero, substitute it with the current window
            // size. But only if the current size is not fullscreen.
            if size.w == 0 {
                size.w = current_size?.w;
            }
            if size.h == 0 {
                size.h = current_size?.h;
            }

            Some(size)
        } else {
            // No pending size, return the current size if it's non-fullscreen.
            current_size
        }
    }

    fn is_pending_windowed_fullscreen(&self) -> bool {
        self.is_pending_windowed_fullscreen
    }

    fn request_windowed_fullscreen(&mut self, value: bool) {
        if self.is_pending_windowed_fullscreen == value {
            return;
        }

        self.is_pending_windowed_fullscreen = value;

        // Set the fullscreen state to match.
        //
        // When going from windowed to real fullscreen, we'll use request_size() which will set the
        // fullscreen state back.
        self.toplevel().with_pending_state(|state| {
            if value {
                state.states.set(xdg_toplevel::State::Fullscreen);
            } else {
                state.states.unset(xdg_toplevel::State::Fullscreen);
            }
        });

        // Make sure we recieve a commit later to update self.is_windowed_fullscreen.
        self.needs_configure = true;
    }

    fn is_child_of(&self, parent: &Self) -> bool {
        self.toplevel().parent().as_ref() == Some(parent.toplevel().wl_surface())
    }

    fn refresh(&self) {
        self.window.refresh();
    }

    fn rules(&self) -> &ResolvedWindowRules {
        &self.rules
    }

    fn animation_snapshot(&self) -> Option<&LayoutElementRenderSnapshot> {
        self.animation_snapshot.as_ref()
    }

    fn take_animation_snapshot(&mut self) -> Option<LayoutElementRenderSnapshot> {
        self.animation_snapshot.take()
    }

    fn set_interactive_resize(&mut self, data: Option<InteractiveResizeData>) {
        self.toplevel().with_pending_state(|state| {
            if data.is_some() {
                state.states.set(xdg_toplevel::State::Resizing);
            } else {
                state.states.unset(xdg_toplevel::State::Resizing);
            }
        });

        if let Some(data) = data {
            self.interactive_resize = Some(InteractiveResize::Ongoing(data));
        } else {
            self.interactive_resize = match self.interactive_resize.take() {
                Some(InteractiveResize::Ongoing(data)) => {
                    Some(InteractiveResize::WaitingForLastConfigure(data))
                }
                x => x,
            }
        }
    }

    fn cancel_interactive_resize(&mut self) {
        self.set_interactive_resize(None);
        self.interactive_resize = None;
    }

    fn interactive_resize_data(&self) -> Option<InteractiveResizeData> {
        Some(self.interactive_resize.as_ref()?.data())
    }

    fn on_commit(&mut self, commit_serial: Serial) {
        if let Some(InteractiveResize::WaitingForLastCommit { serial, .. }) =
            &self.interactive_resize
        {
            if commit_serial.is_no_older_than(serial) {
                self.interactive_resize = None;
            }
        }

        if let Some(RequestSizeOnce::WaitingForCommit(serial)) = &self.request_size_once {
            if commit_serial.is_no_older_than(serial) {
                self.request_size_once = Some(RequestSizeOnce::UseWindowSize);
            }
        }

        // "Commit" our "acked" pending windowed fullscreen state.
        self.uncommited_windowed_fullscreen
            .retain_mut(|(serial, value)| {
                if commit_serial.is_no_older_than(serial) {
                    self.is_windowed_fullscreen = *value;
                    false
                } else {
                    true
                }
            });
    }
}

/* 已映射窗口关键功能说明
1. 状态管理:
   - 焦点状态 (is_focused)
   - 浮动状态 (is_floating)
   - 紧急状态 (is_urgent)
   - 全屏模拟 (is_windowed_fullscreen)

2. 渲染控制:
   - 离屏渲染 (offscreen_data)
   - 动画快照 (animation_snapshot)
   - 屏蔽渲染 (block_out_buffer)

3. 交互处理:
   - 交互式调整大小 (interactive_resize)
   - 事务管理 (pending_transactions)
   - 尺寸请求 (request_size_once)

4. 规则应用:
   - 动态规则更新 (need_to_recompute_rules)
   - 规则覆盖 (ignore_opacity_window_rule)

5. 生命周期:
   - 预提交钩子 (pre_commit_hook)
   - 配置管理 (needs_configure)
   - 进程凭证 (credentials)

6. 窗口投射:
   - 特殊渲染路径 (WindowCastRenderElements)

工作流程:
  1. 窗口创建: 初始化状态，设置预提交钩子
  2. 规则应用: 根据匹配规则设置初始状态
  3. 用户交互: 处理调整大小、焦点变化等
  4. 渲染准备: 生成渲染元素，处理动画
  5. 提交处理: 通过钩子拦截提交，更新状态
  6. 事务协调: 管理异步操作，确保一致性
*/
