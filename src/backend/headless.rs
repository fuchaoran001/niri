//! 作用：无头测试后端实现
//! 说明：提供虚拟输出环境用于自动化测试，不进行实际渲染
//! 特性：
//!   - 模拟输出设备创建
//!   - 记录呈现反馈
//!   - 支持输出状态跟踪
//! 限制：不执行真实渲染（仅流程验证）

use std::mem;
use std::sync::{Arc, Mutex};

use niri_config::OutputName; // 输出命名配置
use smithay::backend::allocator::dmabuf::Dmabuf; // DMA缓冲区（未实现）
use smithay::backend::renderer::element::RenderElementStates; // 渲染元素状态占位符
use smithay::backend::renderer::gles::GlesRenderer; // OpenGL ES渲染器（未使用）
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel}; // 输出设备抽象
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback; // 呈现时间协议
use smithay::utils::Size; // 尺寸工具
use smithay::wayland::presentation::Refresh; // 呈现刷新类型

use super::{IpcOutputMap, OutputId, RenderResult}; // 从父模块导入类型
use crate::niri::{Niri, RedrawState}; // 主合成器状态
use crate::utils::{get_monotonic_time, logical_output}; // 实用函数

// 结构：无头后端
// 作用：模拟显示设备行为的虚拟后端
// 成员：
//   - ipc_outputs: 线程安全的IPC输出描述映射
pub struct Headless {
    ipc_outputs: Arc<Mutex<IpcOutputMap>>,
}

impl Headless {
    // 函数：创建新实例
    pub fn new() -> Self {
        Self {
            ipc_outputs: Default::default(), // 初始化为空映射
        }
    }

    // 函数：初始化后端
    // 说明：空实现（测试环境无需特殊初始化）
    pub fn init(&mut self, _niri: &mut Niri) {}

    // 函数：添加虚拟输出
    // 作用：创建并配置模拟输出设备
    // 参数：
    //   - n: 输出序号（用于生成唯一标识）
    //   - size: 虚拟分辨率（宽, 高）
    // 流程：
    //   1. 构造输出标识信息
    //   2. 创建Output对象
    //   3. 设置显示模式
    //   4. 记录IPC输出信息
    //   5. 添加到合成器
    pub fn add_output(&mut self, niri: &mut Niri, n: u8, size: (u16, u16)) {
        // 生成唯一连接器名称
        let connector = format!("headless-{n}");
        let make = "niri".to_string();
        let model = "headless".to_string();
        let serial = n.to_string();

        // 创建虚拟输出（物理尺寸为0）
        let output = Output::new(
            connector.clone(),
            PhysicalProperties {
                size: (0, 0).into(), // 无物理尺寸
                subpixel: Subpixel::Unknown, // 子像素布局未知
                make: make.clone(),
                model: model.clone(),
            },
        );

        // 配置显示模式（固定60Hz）
        let mode = Mode {
            size: Size::from((i32::from(size.0), i32::from(size.1))),
            refresh: 60_000, // 毫赫兹单位（60Hz）
        };

  
        output.change_current_state(Some(mode), None, None, None); // 应用当前模式
        output.set_preferred(mode); // 设为首选模式

        // 存储输出标识信息
        output.user_data().insert_if_missing(|| OutputName {
            connector,
            make: Some(make),
            model: Some(model),
            serial: Some(serial),
        });

        // 生成IPC输出描述
        let physical_properties = output.physical_properties();
        self.ipc_outputs.lock().unwrap().insert(
            OutputId::next(), // 分配唯一ID
            niri_ipc::Output {
                name: output.name(),
                make: physical_properties.make,
                model: physical_properties.model,
                serial: None, // 无物理序列号
                physical_size: None, // 无物理尺寸
                modes: vec![niri_ipc::Mode {
                    width: size.0,
                    height: size.1,
                    refresh_rate: 60_000,
                    is_preferred: true,
                }],
                current_mode: Some(0), // 当前使用第一个模式
                vrr_supported: false, // 不支持VRR
                vrr_enabled: false,
                logical: Some(logical_output(&output)), // 逻辑位置信息
            },
        );

        // 添加到合成器（不指定位置）
        niri.add_output(output, None, false);
    }

    // 函数：获取座位名称
    // 返回：固定字符串"headless"
    pub fn seat_name(&self) -> String {
        "headless".to_owned()
    }

    // 函数：访问主渲染器
    // 说明：始终返回None（无真实渲染器）
    pub fn with_primary_renderer<T>(
        &mut self,
        _f: impl FnOnce(&mut GlesRenderer) -> T,
    ) -> Option<T> {
        None
    }

    // 函数：模拟渲染过程
    // 作用：处理呈现反馈并更新输出状态
    // 流程：
    //   1. 创建空渲染状态
    //   2. 处理所有待呈现反馈（标记为已呈现）
    //   3. 更新输出重绘状态
    //   4. 递增帧回调序号
    // 返回：总是Submitted（模拟提交成功）
    pub fn render(&mut self, niri: &mut Niri, output: &Output) -> RenderResult {
        // 创建空渲染状态（测试环境无实际渲染）
        let states = RenderElementStates::default();
        
        // 获取并处理呈现反馈
        let mut presentation_feedbacks = niri.take_presentation_feedbacks(output, &states);
        presentation_feedbacks.presented::<_, smithay::utils::Monotonic>(
            get_monotonic_time(), // 使用当前时间作为呈现时间
            Refresh::Unknown,     // 刷新类型未知
            0,                    // 序列号（未使用）
            wp_presentation_feedback::Kind::empty(), // 无特殊标志
        );

        // 更新输出状态机
        let output_state = niri.output_state.get_mut(output).unwrap();
        match mem::replace(&mut output_state.redraw_state, RedrawState::Idle) {
            RedrawState::Idle => unreachable!(), // 理论上不应发生
            RedrawState::Queued => (),           // 正常状态转移
            RedrawState::WaitingForVBlank { .. } => unreachable!(), // 无真实垂直同步
            RedrawState::WaitingForEstimatedVBlank(_) => unreachable!(),
            RedrawState::WaitingForEstimatedVBlankAndQueued(_) => unreachable!(),
        }

        // 递增帧回调序号（模拟真实设备行为）
        output_state.frame_callback_sequence = output_state.frame_callback_sequence.wrapping_add(1);

        // FIXME: 此处应处理未完成动画的重绘请求（测试环境暂未实现）

        // 返回提交成功
        RenderResult::Submitted
    }

    // 函数：导入DMA缓冲区
    // 说明：未实现（测试环境无需真实缓冲区）
    pub fn import_dmabuf(&mut self, _dmabuf: &Dmabuf) -> bool {
        unimplemented!()
    }

    // 函数：获取IPC输出映射
    pub fn ipc_outputs(&self) -> Arc<Mutex<IpcOutputMap>> {
        self.ipc_outputs.clone()
    }
}

// 为Headless实现Default特征
impl Default for Headless {
    fn default() -> Self {
        Self::new()
    }
}