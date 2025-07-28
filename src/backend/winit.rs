//! 作用：Winit后端实现
//! 说明：使用winit库创建窗口环境，支持跨平台运行
//! 特性：
//!   - 窗口化渲染环境
//!   - 支持DPI缩放
//!   - 响应窗口事件（调整大小/输入等）
//!   - 集成到合成器主循环

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use niri_config::{Config, OutputName}; // 配置管理
use smithay::backend::allocator::dmabuf::Dmabuf; // DMA缓冲区支持
use smithay::backend::renderer::damage::OutputDamageTracker; // 损伤区域跟踪
use smithay::backend::renderer::gles::GlesRenderer; // OpenGL ES渲染器
use smithay::backend::renderer::{DebugFlags, ImportDma, ImportEgl, Renderer}; // 渲染器特性
use smithay::backend::winit::{self, WinitEvent, WinitGraphicsBackend}; // winit后端集成
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel}; // 输出设备抽象
use smithay::reexports::calloop::LoopHandle; // 事件循环句柄
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback; // 呈现时间协议
use smithay::reexports::winit::dpi::LogicalSize; // 逻辑尺寸
use smithay::reexports::winit::window::Window; // winit窗口对象
use smithay::wayland::presentation::Refresh; // 呈现刷新类型

use super::{IpcOutputMap, OutputId, RenderResult}; // 父模块类型
use crate::niri::{Niri, RedrawState, State}; // 主合成器状态
use crate::render_helpers::debug::draw_damage; // 调试损伤可视化
use crate::render_helpers::{resources, shaders, RenderTarget}; // 渲染辅助工具
use crate::utils::{get_monotonic_time, logical_output}; // 实用函数

// 结构：Winit后端
// 作用：管理winit窗口环境及其与合成器的集成
// 成员：
//   - config: 共享配置引用
//   - output: 虚拟输出设备（对应窗口）
//   - backend: winit图形后端（包含渲染器和窗口）
//   - damage_tracker: 输出损伤跟踪器
//   - ipc_outputs: IPC输出描述映射
pub struct Winit {
    config: Rc<RefCell<Config>>,
    output: Output,
    backend: WinitGraphicsBackend<GlesRenderer>,
    damage_tracker: OutputDamageTracker,
    ipc_outputs: Arc<Mutex<IpcOutputMap>>,
}

impl Winit {
    // 函数：创建新实例
    // 作用：初始化winit窗口和渲染环境
    // 参数：
    //   - config: 共享配置
    //   - event_loop: 主事件循环句柄
    // 返回：Result包裹的Winit实例
    pub fn new(
        config: Rc<RefCell<Config>>,
        event_loop: LoopHandle<State>,
    ) -> Result<Self, winit::Error> {
        // 创建窗口属性
        let builder = Window::default_attributes()
            .with_inner_size(LogicalSize::new(1280.0, 800.0)) // 初始尺寸
            .with_title("niri"); // 窗口标题
        
        // 初始化winit图形后端
        let (backend, winit) = winit::init_from_attributes(builder)?;

        // 创建虚拟输出设备（对应窗口）
        let output = Output::new(
            "winit".to_string(),
            PhysicalProperties {
                size: (0, 0).into(), // 无物理尺寸
                subpixel: Subpixel::Unknown, // 子像素布局未知
                make: "Smithay".into(),
                model: "Winit".into(),
            },
        );

        // 设置输出显示模式
        let mode = Mode {
            size: backend.window_size(), // 使用窗口尺寸
            refresh: 60_000, // 60Hz刷新率
        };
        output.change_current_state(Some(mode), None, None, None);
        output.set_preferred(mode); // 设为首选模式

        // 存储输出标识信息
        output.user_data().insert_if_missing(|| OutputName {
            connector: "winit".to_string(),
            make: Some("Smithay".to_string()),
            model: Some("Winit".to_string()),
            serial: None,
        });

        // 准备IPC输出描述
        let physical_properties = output.physical_properties();
        let ipc_outputs = Arc::new(Mutex::new(HashMap::from([(
            OutputId::next(), // 分配唯一ID
            niri_ipc::Output {
                name: output.name(),
                make: physical_properties.make,
                model: physical_properties.model,
                serial: None,
                physical_size: None,
                modes: vec![niri_ipc::Mode {
                    // 转换尺寸为u16（限制在安全范围内）
                    width: backend.window_size().w.clamp(0, u16::MAX as i32) as u16,
                    height: backend.window_size().h.clamp(0, u16::MAX as i32) as u16,
                    refresh_rate: 60_000,
                    is_preferred: true,
                }],
                current_mode: Some(0),
                vrr_supported: false, // 不支持VRR
                vrr_enabled: false,
                logical: Some(logical_output(&output)), // 逻辑位置信息
            },
        )])));

        // 初始化损伤跟踪器
        let damage_tracker = OutputDamageTracker::from_output(&output);

        // 注册winit事件源
        event_loop
            .insert_source(winit, move |event, _, state| match event {
                // 窗口大小变化事件
                WinitEvent::Resized { size, .. } => {
                    let winit = state.backend.winit();
                    
                    // 更新输出模式
                    winit.output.change_current_state(
                        Some(Mode {
                            size,
                            refresh: 60_000,
                        }),
                        None,
                        None,
                        None,
                    );

                    // 更新IPC输出描述
                    {
                        let mut ipc_outputs = winit.ipc_outputs.lock().unwrap();
                        let output = ipc_outputs.values_mut().next().unwrap();
                        let mode = &mut output.modes[0];
                        mode.width = size.w.clamp(0, u16::MAX as i32) as u16;
                        mode.height = size.h.clamp(0, u16::MAX as i32) as u16;
                        if let Some(logical) = output.logical.as_mut() {
                            logical.width = size.w as u32;
                            logical.height = size.h as u32;
                        }
                        state.niri.ipc_outputs_changed = true; // 标记变更
                    }

                    // 通知合成器输出尺寸变化
                    state.niri.output_resized(&winit.output);
                }
                // 输入事件（转发给合成器）
                WinitEvent::Input(event) => state.process_input_event(event),
                // 窗口焦点事件（暂不处理）
                WinitEvent::Focus(_) => (),
                // 重绘请求（排队重绘）
                WinitEvent::Redraw => state.niri.queue_redraw(&state.backend.winit().output),
                // 窗口关闭请求（停止主循环）
                WinitEvent::CloseRequested => state.niri.stop_signal.stop(),
            })
            .unwrap();

        // 返回初始化完成的实例
        Ok(Self {
            config,
            output,
            backend,
            damage_tracker,
            ipc_outputs,
        })
    }

    // 函数：初始化后端
    // 作用：完成与合成器的集成
    // 流程：
    //   1. 绑定Wayland显示
    //   2. 初始化渲染资源
    //   3. 加载自定义着色器（如果配置）
    //   4. 添加虚拟输出到合成器
    pub fn init(&mut self, niri: &mut Niri) {
        let renderer = self.backend.renderer();
        
        // 绑定Wayland显示（用于客户端渲染）
        if let Err(err) = renderer.bind_wl_display(&niri.display_handle) {
            warn!("error binding renderer wl_display: {err}");
        }

        // 初始化渲染资源
        resources::init(renderer);
        shaders::init(renderer);

        // 应用自定义着色器配置
        let config = self.config.borrow();
        if let Some(src) = config.animations.window_resize.custom_shader.as_deref() {
            shaders::set_custom_resize_program(renderer, Some(src));
        }
        if let Some(src) = config.animations.window_close.custom_shader.as_deref() {
            shaders::set_custom_close_program(renderer, Some(src));
        }
        if let Some(src) = config.animations.window_open.custom_shader.as_deref() {
            shaders::set_custom_open_program(renderer, Some(src));
        }
        drop(config);

        // 更新着色器状态
        niri.update_shaders();

        // 添加输出到合成器
        niri.add_output(self.output.clone(), None, false);
    }

    // 函数：获取座位名称
    // 返回：固定字符串"winit"
    pub fn seat_name(&self) -> String {
        "winit".to_owned()
    }

    // 函数：访问主渲染器
    // 作用：在闭包中安全访问OpenGL ES渲染器
    pub fn with_primary_renderer<T>(
        &mut self,
        f: impl FnOnce(&mut GlesRenderer) -> T,
    ) -> Option<T> {
        Some(f(self.backend.renderer()))
    }

    // 函数：渲染输出
    // 作用：将合成结果渲染到winit窗口
    // 流程：
    //   1. 生成渲染元素列表
    //   2. 可选绘制损伤区域（调试）
    //   3. 绑定帧缓冲区
    //   4. 渲染到窗口
    //   5. 提交帧并处理呈现反馈
    //   6. 更新输出状态
    pub fn render(&mut self, niri: &mut Niri, output: &Output) -> RenderResult {
        let _span = tracy_client::span!("Winit::render");

        // 生成渲染元素
        let mut elements = niri.render::<GlesRenderer>(
            self.backend.renderer(),
            output,
            true,
            RenderTarget::Output,
        );

        // 调试：可视化损伤区域
        if niri.debug_draw_damage {
            let output_state = niri.output_state.get_mut(output).unwrap();
            draw_damage(&mut output_state.debug_damage_tracker, &mut elements);
        }

        // 绑定帧缓冲区并渲染
        let res = {
            let (renderer, mut framebuffer) = self.backend.bind().unwrap();
            // FIXME: 暂时无法获取缓冲区年龄
            let age = 0;
            self.damage_tracker
                .render_output(renderer, &mut framebuffer, age, &elements, [0.; 4])
                .unwrap()
        };

        // 更新主扫描输出
        niri.update_primary_scanout_output(output, &res.states);

        // 处理渲染结果
        let rv;
        if let Some(damage) = res.damage {
            // 可选的帧同步等待（根据配置）
            if self
                .config
                .borrow()
                .debug
                .wait_for_frame_completion_before_queueing
            {
                let _span = tracy_client::span!("wait for completion");
                if let Err(err) = res.sync.wait() {
                    warn!("error waiting for frame completion: {err:?}");
                }
            }

            // 提交帧到窗口
            self.backend.submit(Some(damage)).unwrap();

            // 处理呈现反馈
            let mut presentation_feedbacks = niri.take_presentation_feedbacks(output, &res.states);
            presentation_feedbacks.presented::<_, smithay::utils::Monotonic>(
                get_monotonic_time(),
                Refresh::Unknown,
                0,
                wp_presentation_feedback::Kind::empty(),
            );

            rv = RenderResult::Submitted;
        } else {
            rv = RenderResult::NoDamage;
        }

        // 更新输出状态机
        let output_state = niri.output_state.get_mut(output).unwrap();
        match mem::replace(&mut output_state.redraw_state, RedrawState::Idle) {
            RedrawState::Idle => unreachable!(),
            RedrawState::Queued => (),
            _ => unreachable!(), // Winit后端不使用VBlank状态
        }

        // 递增帧回调序号
        output_state.frame_callback_sequence = output_state.frame_callback_sequence.wrapping_add(1);

        // 处理未完成动画
        if output_state.unfinished_animations_remain {
            // 请求下一帧重绘
            self.backend.window().request_redraw();
        }

        rv
    }

    // 函数：切换调试着色
    // 作用：启用/禁用渲染调试色块
    pub fn toggle_debug_tint(&mut self) {
        let renderer = self.backend.renderer();
        // 切换TINT调试标志
        renderer.set_debug_flags(renderer.debug_flags() ^ DebugFlags::TINT);
    }

    // 函数：导入DMA缓冲区
    // 作用：将DMA缓冲区添加到渲染器资源池
    // 返回：是否导入成功
    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        match self.backend.renderer().import_dmabuf(dmabuf, None) {
            Ok(_texture) => true,
            Err(err) => {
                debug!("error importing dmabuf: {err:?}");
                false
            }
        }
    }

    // 函数：获取IPC输出映射
    pub fn ipc_outputs(&self) -> Arc<Mutex<IpcOutputMap>> {
        self.ipc_outputs.clone()
    }
}