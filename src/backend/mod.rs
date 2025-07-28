//! 作用：后端抽象层
//! 说明：定义统一的后端接口，支持多种渲染后端（TTY/Winit/Headless）
//! 功能：
//!   - 初始化不同后端
//!   - 提供统一渲染接口
//!   - 管理输出映射
//!   - 处理输入配置

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use niri_config::{Config, ModKey}; // 配置管理
use smithay::backend::allocator::dmabuf::Dmabuf; // DMA缓冲区抽象
use smithay::backend::renderer::gles::GlesRenderer; // OpenGL ES渲染器
use smithay::output::Output; // 输出设备抽象
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface; // Wayland表面

use crate::niri::Niri; // 主合成器结构
use crate::utils::id::IdCounter; // ID生成器

// 子模块声明
pub mod tty;    // Linux TTY后端（DRM/KMS）
pub use tty::Tty; // 导出TTY后端

pub mod winit;  // 跨平台Winit后端
pub use winit::Winit; // 导出Winit后端

pub mod headless; // 无头渲染后端
pub use headless::Headless; // 导出Headless后端

// 枚举：后端类型
// 作用：表示可用的不同渲染后端
// 说明：
//   - Tty: 原生Linux DRM/KMS后端
//   - Winit: 跨平台窗口系统后端
//   - Headless: 无显示输出后端（用于测试）
#[allow(clippy::large_enum_variant)] // 允许大型变体（因Tty可能较大）
pub enum Backend {
    Tty(Tty),     // Linux原生DRM后端
    Winit(Winit), // 跨平台Winit后端
    Headless(Headless), // 无头测试后端
}

// 枚举：渲染结果
// 作用：描述渲染操作的执行结果
// 变体：
//   - Submitted: 帧已提交到后端等待显示
//   - NoDamage: 渲染成功但无可见变化（跳过提交）
//   - Skipped: 渲染被跳过（通常因错误）
#[derive(PartialEq, Eq)]
pub enum RenderResult {
    Submitted,
    NoDamage,
    Skipped,
}

// 类型别名：IPC输出映射
// 作用：存储输出ID到IPC输出描述的映射
// 说明：用于DBus/IPC通信中识别输出设备
pub type IpcOutputMap = HashMap<OutputId, niri_ipc::Output>;

// 静态ID计数器
// 作用：全局唯一的输出ID生成器
static OUTPUT_ID_COUNTER: IdCounter = IdCounter::new();

// 结构：输出标识符
// 作用：唯一标识物理或虚拟输出设备
// 特性：
//   - 实现Copy/Clone（轻量复制）
//   - 哈希支持（用于HashMap键）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutputId(u64);

impl OutputId {
    // 函数：生成下一个输出ID
    // 作用：使用全局计数器创建唯一ID
    fn next() -> OutputId {
        OutputId(OUTPUT_ID_COUNTER.next())
    }

    // 函数：获取原始ID值
    pub fn get(self) -> u64 {
        self.0
    }
}

// Backend枚举的方法实现
impl Backend {
    // 函数：初始化后端
    // 作用：执行后端特定的初始化逻辑
    // 参数：
    //   - niri: 主合成器实例（用于状态访问）
    pub fn init(&mut self, niri: &mut Niri) {
        // Rust模式匹配：根据枚举变体调用对应方法
        match self {
            Backend::Tty(tty) => tty.init(niri),
            Backend::Winit(winit) => winit.init(niri),
            Backend::Headless(headless) => headless.init(niri),
        }
    }

    // 函数：获取座位名称
    // 作用：返回后端关联的输入设备名称
    // 说明：Wayland中"座位"代表一组输入设备
    pub fn seat_name(&self) -> String {
        match self {
            Backend::Tty(tty) => tty.seat_name(),
            Backend::Winit(winit) => winit.seat_name(),
            Backend::Headless(headless) => headless.seat_name(),
        }
    }

    // 函数：访问主渲染器
    // 作用：在闭包中安全访问主OpenGL ES渲染器
    // 参数：
    //   - f: 接受渲染器引用的闭包
    // 返回：闭包执行结果（Option包装）
    // Rust机制：高阶函数（闭包作为参数）
    pub fn with_primary_renderer<T>(
        &mut self,
        f: impl FnOnce(&mut GlesRenderer) -> T,
    ) -> Option<T> {
        match self {
            Backend::Tty(tty) => tty.with_primary_renderer(f),
            Backend::Winit(winit) => winit.with_primary_renderer(f),
            Backend::Headless(headless) => headless.with_primary_renderer(f),
        }
    }

    // 函数：渲染输出
    // 作用：将合成结果渲染到指定输出设备
    // 参数：
    //   - niri: 主合成器状态
    //   - output: 目标输出设备
    //   - target_presentation_time: 目标呈现时间（用于帧同步）
    // 返回：渲染结果状态
    pub fn render(
        &mut self,
        niri: &mut Niri,
        output: &Output,
        target_presentation_time: Duration,
    ) -> RenderResult {
        match self {
            // TTY后端使用精确的呈现时间控制
            Backend::Tty(tty) => tty.render(niri, output, target_presentation_time),
            // Winit/Headless后端忽略呈现时间参数
            Backend::Winit(winit) => winit.render(niri, output),
            Backend::Headless(headless) => headless.render(niri, output),
        }
    }

    // 函数：获取修饰键配置
    // 作用：根据后端类型解析配置中的修饰键
    // 说明：
    //   - Winit后端可能需要特殊处理（嵌套合成器场景）
    //   - TTY/Headless使用标准配置
    pub fn mod_key(&self, config: &Config) -> ModKey {
        match self {
            Backend::Winit(_) => config.input.mod_key_nested.unwrap_or({
                // 嵌套模式下的回退逻辑
                if let Some(ModKey::Alt) = config.input.mod_key {
                    ModKey::Super // 避免与宿主冲突
                } else {
                    ModKey::Alt
                }
            }),
            // 直接使用配置的修饰键（默认Super）
            Backend::Tty(_) | Backend::Headless(_) => config.input.mod_key.unwrap_or(ModKey::Super),
        }
    }

    // 函数：切换虚拟终端
    // 作用：仅TTY后端支持（切换Linux VT）
    pub fn change_vt(&mut self, vt: i32) {
        if let Backend::Tty(tty) = self {
            tty.change_vt(vt);
        }
    }

    // 函数：挂起系统
    // 作用：仅TTY后端支持（系统休眠）
    pub fn suspend(&mut self) {
        if let Backend::Tty(tty) = self {
            tty.suspend();
        }
    }

    // 函数：切换调试着色
    // 作用：在渲染输出上叠加调试色块（开发用）
    pub fn toggle_debug_tint(&mut self) {
        match self {
            Backend::Tty(tty) => tty.toggle_debug_tint(),
            Backend::Winit(winit) => winit.toggle_debug_tint(),
            _ => (),
        }
    }

    // 函数：导入DMA缓冲区
    // 作用：将DMA缓冲区添加到渲染器资源池
    // 返回：是否导入成功
    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        match self {
            Backend::Tty(tty) => tty.import_dmabuf(dmabuf),
            Backend::Winit(winit) => winit.import_dmabuf(dmabuf),
            Backend::Headless(headless) => headless.import_dmabuf(dmabuf),
        }
    }

    // 函数：提前导入表面
    // 作用：优化Wayland表面的缓冲区导入（仅TTY需要）
    pub fn early_import(&mut self, surface: &WlSurface) {
        if let Backend::Tty(tty) = self {
            tty.early_import(surface);
        }
    }

    // 函数：获取IPC输出映射
    // 作用：返回线程安全的输出设备描述映射
    // 说明：用于DBus/IPC接口报告当前输出状态
    pub fn ipc_outputs(&self) -> Arc<Mutex<IpcOutputMap>> {
        match self {
            Backend::Tty(tty) => tty.ipc_outputs(),
            Backend::Winit(winit) => winit.ipc_outputs(),
            Backend::Headless(headless) => headless.ipc_outputs(),
        }
    }

    // 函数：获取GBM设备（条件编译）
    // 作用：仅TTY后端提供DMA缓冲区分配设备
    // 用途：屏幕共享功能需要
    #[cfg(feature = "xdp-gnome-screencast")]
    pub fn gbm_device(
        &self,
    ) -> Option<smithay::backend::allocator::gbm::GbmDevice<smithay::backend::drm::DrmDeviceFd>>
    {
        if let Backend::Tty(tty) = self {
            tty.primary_gbm_device()
        } else {
            None
        }
    }

    // 函数：设置显示器电源状态
    // 作用：仅TTY后端支持（控制DRM设备电源）
    pub fn set_monitors_active(&mut self, active: bool) {
        if let Backend::Tty(tty) = self {
            tty.set_monitors_active(active);
        }
    }

    // 函数：动态设置VRR
    // 作用：按需启用/禁用可变刷新率（仅TTY）
    pub fn set_output_on_demand_vrr(&mut self, niri: &mut Niri, output: &Output, enable_vrr: bool) {
        if let Backend::Tty(tty) = self {
            tty.set_output_on_demand_vrr(niri, output, enable_vrr);
        }
    }

    // 函数：处理输出配置变更
    // 作用：当输出配置改变时更新后端状态（仅TTY）
    pub fn on_output_config_changed(&mut self, niri: &mut Niri) {
        if let Backend::Tty(tty) = self {
            tty.on_output_config_changed(niri);
        }
    }

    // 以下为类型安全访问方法
    // 作用：避免直接匹配，提供类型化访问

    // 安全访问TTY后端（Option版）
    pub fn tty_checked(&mut self) -> Option<&mut Tty> {
        if let Self::Tty(v) = self {
            Some(v)
        } else {
            None
        }
    }

    // 安全访问TTY后端（直接访问）
    pub fn tty(&mut self) -> &mut Tty {
        match self {
            Self::Tty(v) => v,
            _ => panic!("backend is not Tty"), // 开发阶段检查
        }
    }

    // 安全访问Winit后端
    pub fn winit(&mut self) -> &mut Winit {
        match self {
            Self::Winit(v) => v,
            _ => panic!("backend is not Winit"),
        }
    }

    // 安全访问Headless后端
    pub fn headless(&mut self) -> &mut Headless {
        match self {
            Self::Headless(v) => v,
            _ => panic!("backend is not Headless"),
        }
    }
}