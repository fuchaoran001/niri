//! 实用工具模块，包含各种辅助函数和数据结构。
//!
//! 本模块提供合成器核心功能所需的各类工具函数，包括：
//! - 几何计算（中心点计算、坐标转换）
//! - 输出设备管理（分辨率获取、逻辑输出转换）
//! - 表面状态处理（映射检测、缩放变换）
//! - 路径处理（主目录展开、截图路径生成）
//! - 窗口装饰管理（CSD/SSD切换、平铺状态更新）
//! - 错误处理工具（确保数值范围）
//! - 调试功能（触发panic）
//!
//! 这些工具函数贯穿整个合成器的生命周期，支撑着核心功能的实现。

use std::cmp::{max, min}; // 导入最大值/最小值比较函数
use std::f64; // 64位浮点数支持
use std::io::Write; // IO写操作trait
use std::path::{Path, PathBuf}; // 路径处理
use std::sync::atomic::AtomicBool; // 原子布尔类型
use std::time::Duration; // 时间间隔类型

use anyhow::{Context}; // 错误处理工具
use bitflags::bitflags; // 位标志宏
use directories::UserDirs; // 用户目录获取
use git_version::git_version; // Git版本信息获取
use niri_config::{OutputName}; // 配置结构体
use smithay::backend::renderer::utils::with_renderer_surface_state; // 渲染器表面状态访问
use smithay::input::pointer::CursorIcon; // 鼠标指针图标
use smithay::output::{self, Output}; // 输出设备管理
use smithay::reexports::rustix::time::{clock_gettime, ClockId}; // 系统时间获取
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1; // XDG装饰协议
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel; // XDG顶层协议
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface; // Wayland表面
use smithay::reexports::wayland_server::{DisplayHandle, Resource as _}; // Wayland服务器核心
use smithay::utils::{Coordinate, Logical, Point, Rectangle, Size, Transform}; // 几何工具
use smithay::wayland::compositor::{send_surface_state, with_states, SurfaceData}; // 合成器表面状态
use smithay::wayland::fractional_scale::with_fractional_scale; // 分数缩放支持
use smithay::wayland::shell::xdg::{
    ToplevelSurface, XdgToplevelSurfaceData, XdgToplevelSurfaceRoleAttributes, // XDG顶层表面
};
use wayland_backend::server::Credentials; // Wayland凭证

use crate::handlers::KdeDecorationsModeState; // KDE装饰状态
use crate::niri::ClientState; // 客户端状态

// 子模块声明
pub mod id; // ID管理
pub mod scale; // 缩放处理
pub mod spawning; // 进程生成
pub mod transaction; // 事务处理
pub mod watcher; // 文件监视

// 原子布尔值，标识当前是否作为systemd服务运行
pub static IS_SYSTEMD_SERVICE: AtomicBool = AtomicBool::new(false);

// 使用bitflags宏定义调整边缘的位标志
bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct ResizeEdge: u32 {
        const TOP          = 0b0001; // 上边缘
        const BOTTOM       = 0b0010; // 下边缘
        const LEFT         = 0b0100; // 左边缘
        const RIGHT        = 0b1000; // 右边缘

        const TOP_LEFT     = Self::TOP.bits() | Self::LEFT.bits(); // 左上角
        const BOTTOM_LEFT  = Self::BOTTOM.bits() | Self::LEFT.bits(); // 左下角

        const TOP_RIGHT    = Self::TOP.bits() | Self::RIGHT.bits(); // 右上角
        const BOTTOM_RIGHT = Self::BOTTOM.bits() | Self::RIGHT.bits(); // 右下角

        const LEFT_RIGHT   = Self::LEFT.bits() | Self::RIGHT.bits(); // 左右边缘
        const TOP_BOTTOM   = Self::TOP.bits() | Self::BOTTOM.bits(); // 上下边缘
    }
}

// 从XDG协议边缘类型转换到ResizeEdge
impl From<xdg_toplevel::ResizeEdge> for ResizeEdge {
    #[inline]
    fn from(x: xdg_toplevel::ResizeEdge) -> Self {
        // 通过bits转换并解包（保证安全）
        Self::from_bits(x as u32).unwrap()
    }
}

impl ResizeEdge {
    /// 获取当前边缘对应的鼠标指针图标
    pub fn cursor_icon(self) -> CursorIcon {
        match self {
            Self::LEFT => CursorIcon::WResize, // 西向调整
            Self::RIGHT => CursorIcon::EResize, // 东向调整
            Self::TOP => CursorIcon::NResize, // 北向调整
            Self::BOTTOM => CursorIcon::SResize, // 南向调整
            Self::TOP_LEFT => CursorIcon::NwResize, // 西北向调整
            Self::TOP_RIGHT => CursorIcon::NeResize, // 东北向调整
            Self::BOTTOM_RIGHT => CursorIcon::SeResize, // 东南向调整
            Self::BOTTOM_LEFT => CursorIcon::SwResize, // 西南向调整
            _ => CursorIcon::Default, // 默认指针
        }
    }
}

/// 获取当前niri版本信息字符串
pub fn version() -> String {
    // 优先使用构建时注入的版本字符串
    if let Some(v) = option_env!("NIRI_BUILD_VERSION_STRING") {
        return String::from(v);
    }

    // 从Cargo.toml获取版本组件
    const MAJOR: &str = env!("CARGO_PKG_VERSION_MAJOR");
    const MINOR: &str = env!("CARGO_PKG_VERSION_MINOR");
    const PATCH: &str = env!("CARGO_PKG_VERSION_PATCH");

    // 获取Git提交哈希（或备用字符串）
    let commit =
        option_env!("NIRI_BUILD_COMMIT").unwrap_or(git_version!(fallback = "unknown commit"));

    // 格式化为版本字符串
    if PATCH == "0" {
        // 忽略补丁版本为0的情况
        format!("{MAJOR}.{MINOR:0>2} ({commit})")
    } else {
        format!("{MAJOR}.{MINOR:0>2}.{PATCH} ({commit})")
    }
}

/// 获取单调递增时钟时间（不受系统时间调整影响）
pub fn get_monotonic_time() -> Duration {
    let ts = clock_gettime(ClockId::Monotonic); // 获取单调时钟时间
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32) // 转换为Duration
}

/// 计算矩形中心点（整数坐标）
pub fn center(rect: Rectangle<i32, Logical>) -> Point<i32, Logical> {
    rect.loc + rect.size.downscale(2).to_point()
}

/// 计算矩形中心点（浮点坐标）
pub fn center_f64(rect: Rectangle<f64, Logical>) -> Point<f64, Logical> {
    rect.loc + rect.size.downscale(2.0).to_point()
}

/// 逻辑坐标转物理坐标（四舍五入）
///
/// # Rust泛型说明
/// 使用泛型N和Coordinate trait实现类型安全的坐标转换，
/// 支持任何实现Coordinate trait的数字类型
pub fn to_physical_precise_round<N: Coordinate>(scale: f64, logical: impl Coordinate) -> N {
    N::from_f64((logical.to_f64() * scale).round())
}

/// 在物理像素网格中对齐逻辑坐标（四舍五入）
pub fn round_logical_in_physical(scale: f64, logical: f64) -> f64 {
    (logical * scale).round() / scale
}

/// 在物理像素网格中对齐逻辑坐标（至少1物理像素）
pub fn round_logical_in_physical_max1(scale: f64, logical: f64) -> f64 {
    if logical == 0. {
        return 0.;
    }

    (logical * scale).max(1.).round() / scale
}

/// 在物理像素网格中对齐逻辑坐标（向下取整，至少1物理像素）
pub fn floor_logical_in_physical_max1(scale: f64, logical: f64) -> f64 {
    if logical == 0. {
        return 0.;
    }

    (logical * scale).max(1.).floor() / scale
}

/// 获取输出设备的逻辑尺寸
///
/// 在合成器中的作用：
/// 计算输出设备在考虑缩放和旋转后的实际可视尺寸，
/// 用于窗口布局和定位
pub fn output_size(output: &Output) -> Size<f64, Logical> {
    let output_scale = output.current_scale().fractional_scale(); // 当前缩放系数
    let output_transform = output.current_transform(); // 当前变换（旋转/翻转）
    let output_mode = output.current_mode().unwrap(); // 当前显示模式
    let logical_size = output_mode.size.to_f64().to_logical(output_scale); // 转换为逻辑尺寸
    output_transform.transform_size(logical_size) // 应用变换
}

/// 将输出设备转换为IPC逻辑输出描述
///
/// 关键数据结构设计：
/// 定义标准化输出描述，用于进程间通信(IPC)，
/// 包含位置、尺寸、缩放和变换信息
pub fn logical_output(output: &Output) -> niri_ipc::LogicalOutput {
    let loc = output.current_location(); // 屏幕位置
    let size = output_size(output); // 逻辑尺寸
    // 匹配变换类型到IPC枚举
    let transform = match output.current_transform() {
        Transform::Normal => niri_ipc::Transform::Normal,
        Transform::_90 => niri_ipc::Transform::_90,
        Transform::_180 => niri_ipc::Transform::_180,
        Transform::_270 => niri_ipc::Transform::_270,
        Transform::Flipped => niri_ipc::Transform::Flipped,
        Transform::Flipped90 => niri_ipc::Transform::Flipped90,
        Transform::Flipped180 => niri_ipc::Transform::Flipped180,
        Transform::Flipped270 => niri_ipc::Transform::Flipped270,
    };
    // 构造IPC输出描述
    niri_ipc::LogicalOutput {
        x: loc.x,
        y: loc.y,
        width: size.w as u32,
        height: size.h as u32,
        scale: output.current_scale().fractional_scale(),
        transform,
    }
}

/// IPC变换枚举转Smithay变换枚举
pub fn ipc_transform_to_smithay(transform: niri_ipc::Transform) -> Transform {
    match transform {
        niri_ipc::Transform::Normal => Transform::Normal,
        niri_ipc::Transform::_90 => Transform::_90,
        niri_ipc::Transform::_180 => Transform::_180,
        niri_ipc::Transform::_270 => Transform::_270,
        niri_ipc::Transform::Flipped => Transform::Flipped,
        niri_ipc::Transform::Flipped90 => Transform::Flipped90,
        niri_ipc::Transform::Flipped180 => Transform::Flipped180,
        niri_ipc::Transform::Flipped270 => Transform::Flipped270,
    }
}

/// 检查Wayland表面是否已映射（有缓冲区内容）
///
/// 在合成器中的作用：
/// 确定表面是否准备好进行渲染，
/// 避免渲染未提交的空表面
pub fn is_mapped(surface: &WlSurface) -> bool {
    // 通过渲染器状态检查是否存在有效缓冲区
    with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false)
}

/// 向表面发送缩放和变换信息
///
/// 在合成器中的作用：
/// 通知客户端表面所需的缩放比例和方向变换，
/// 确保客户端渲染内容正确适配显示
pub fn send_scale_transform(
    surface: &WlSurface,
    data: &SurfaceData,
    scale: output::Scale,
    transform: Transform,
) {
    // 发送整数缩放比例和变换
    send_surface_state(surface, data, scale.integer_scale(), transform);
    // 设置分数缩放比例
    with_fractional_scale(data, |fractional| {
        fractional.set_preferred_scale(scale.fractional_scale());
    });
}

/// 展开包含"~"的路径为主目录路径
///
/// # Rust错误处理说明
/// 使用anyhow::Context提供错误上下文信息，
/// 便于追踪错误来源
pub fn expand_home(path: &Path) -> anyhow::Result<Option<PathBuf>> {
    // 尝试剥离"~"前缀
    if let Ok(rest) = path.strip_prefix("~") {
        let dirs = UserDirs::new().context("error retrieving home directory")?; // 获取用户目录
        Ok(Some([dirs.home_dir(), rest].iter().collect())) // 拼接路径
    } else {
        Ok(None) // 不包含"~"
    }
}

/// 将RGBA8像素数据写入PNG格式
///
/// 在合成器中的作用：
/// 保存渲染缓冲区的截图到文件
pub fn write_png_rgba8(
    w: impl Write, // 写入目标（文件/内存等）
    width: u32,    // 图像宽度
    height: u32,   // 图像高度
    pixels: &[u8], // RGBA像素数据
) -> Result<(), png::EncodingError> {
    // 创建PNG编码器
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgba); // 32位RGBA
    encoder.set_depth(png::BitDepth::Eight); // 每通道8位

    // 写入图像数据
    let mut writer = encoder.write_header()?;
    writer.write_image_data(pixels)
}

/// 检查输出设备名称是否匹配目标名称
pub fn output_matches_name(output: &Output, target: &str) -> bool {
    // 从用户数据获取输出名称
    let name = output.user_data().get::<OutputName>().unwrap();
    name.matches(target) // 进行匹配
}

/// 检查连接器名称是否属于笔记本面板
///
/// 笔记本面板特征：
/// 通常以"eDP-"、"LVDS"或"DSI-"开头
pub fn is_laptop_panel(connector: &str) -> bool {
    matches!(connector.get(..4), Some("eDP-" | "LVDS" | "DSI-"))
}

/// 在XDG顶层表面角色上下文中执行操作
///
/// # Rust锁机制说明
/// 通过互斥锁安全访问多线程共享的角色属性，
/// 确保状态一致性
pub fn with_toplevel_role<T>(
    toplevel: &ToplevelSurface, // 目标顶层表面
    f: impl FnOnce(&mut XdgToplevelSurfaceRoleAttributes) -> T, // 操作函数
) -> T {
    // 访问表面状态
    with_states(toplevel.wl_surface(), |states| {
        // 获取并锁定角色数据
        let mut role = states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .unwrap()
            .lock()
            .unwrap();

        // 执行操作
        f(&mut role)
    })
}

/// 更新窗口平铺状态
///
/// 决策逻辑：
/// 1. 若客户端支持XDG装饰协议 → 使用协商的服务端装饰模式
/// 2. 若客户端支持KDE装饰协议 → 使用服务端装饰或按配置偏好
/// 3. 否则 → 使用配置的prefer_no_csd值
///
/// 在合成器中的作用：
/// 控制窗口是否应处于平铺状态（无边框），
/// 影响窗口装饰渲染方式
pub fn update_tiled_state(
    toplevel: &ToplevelSurface, // 目标顶层表面
    prefer_no_csd: bool,        // 配置偏好（是否禁用CSD）
    force_tiled: Option<bool>,  // 强制平铺选项（覆盖自动决策）
) {
    // 自动决策函数
    let should_tile = || {
        // 检查XDG装饰模式
        if let Some(mode) = toplevel.with_pending_state(|state| state.decoration_mode) {
            // 若为服务端装饰则平铺
            mode == zxdg_toplevel_decoration_v1::Mode::ServerSide
        } 
        // 检查KDE装饰状态
        else if let Some(mode) = with_states(toplevel.wl_surface(), |states| {
            states.data_map.get::<KdeDecorationsModeState>().cloned()
        }) {
            // KDE服务端装饰或按配置偏好
            mode.is_server() || prefer_no_csd
        } 
        // 默认使用配置偏好
        else {
            prefer_no_csd
        }
    };

    // 确定最终平铺状态（强制参数优先）
    let should_tile = force_tiled.unwrap_or_else(should_tile);

    // 更新表面状态
    toplevel.with_pending_state(|state| {
        if should_tile {
            // 设置四个方向的平铺状态
            state.states.set(xdg_toplevel::State::TiledLeft);
            state.states.set(xdg_toplevel::State::TiledRight);
            state.states.set(xdg_toplevel::State::TiledTop);
            state.states.set(xdg_toplevel::State::TiledBottom);
        } else {
            // 清除平铺状态
            state.states.unset(xdg_toplevel::State::TiledLeft);
            state.states.unset(xdg_toplevel::State::TiledRight);
            state.states.unset(xdg_toplevel::State::TiledTop);
            state.states.unset(xdg_toplevel::State::TiledBottom);
        }
    });
}

/// 获取与表面关联的客户端凭证
///
/// 在合成器中的作用：
/// 实现安全策略，如仅允许特定用户截图
pub fn get_credentials_for_surface(surface: &WlSurface) -> Option<Credentials> {
    // 升级弱引用获取显示句柄
    let handle = surface.handle().upgrade()?;
    let dh = DisplayHandle::from(handle);

    // 获取关联客户端
    let client = dh.get_client(surface.id()).ok()?;
    // 检查凭证状态
    let data = client.get_data::<ClientState>().unwrap();
    if data.credentials_unknown {
        return None; // 凭证未知
    }

    // 获取客户端凭证
    client.get_credentials(&dh).ok()
}

/// 确保数值在[min_size, max_size]范围内
///
/// 处理规则：
/// 1. 若max_size>0：限制上限
/// 2. 若min_size>0：限制下限
pub fn ensure_min_max_size(mut x: i32, min_size: i32, max_size: i32) -> i32 {
    if max_size > 0 {
        x = min(x, max_size);
    }
    if min_size > 0 {
        x = max(x, min_size);
    }
    x
}

/// 增强版范围确保（特殊处理0值）
///
/// 处理规则：
/// 1. 非零值：正常范围限制
/// 2. 零值且min_size=max_size>0：返回min_size
/// 3. 否则保持0
pub fn ensure_min_max_size_maybe_zero(x: i32, min_size: i32, max_size: i32) -> i32 {
    if x != 0 {
        ensure_min_max_size(x, min_size, max_size)
    } else if min_size > 0 && min_size == max_size {
        min_size
    } else {
        0
    }
}

/// 将矩形限制在区域内（优先保持左上角位置）
///
/// 算法步骤：
/// 1. 限制右下角不超过区域右下角
/// 2. 限制左上角不低于区域左上角
///
/// 在合成器中的作用：
/// 确保窗口不会移出屏幕可见区域
pub fn clamp_preferring_top_left_in_area(
    area: Rectangle<f64, Logical>, // 限制区域
    rect: &mut Rectangle<f64, Logical>, // 待调整矩形
) {
    // 调整X坐标（不超过区域右边界）
    rect.loc.x = f64::min(rect.loc.x, area.loc.x + area.size.w - rect.size.w);
    // 调整Y坐标（不超过区域下边界）
    rect.loc.y = f64::min(rect.loc.y, area.loc.y + area.size.h - rect.size.h);

    // 二次调整X坐标（不低于区域左边界）
    rect.loc.x = f64::max(rect.loc.x, area.loc.x);
    // 二次调整Y坐标（不低于区域上边界）
    rect.loc.y = f64::max(rect.loc.y, area.loc.y);
}

/// 在区域内居中矩形（优先左上角位置）
///
/// 当矩形大于区域时，返回左上角对齐的位置
pub fn center_preferring_top_left_in_area(
    area: Rectangle<f64, Logical>, // 目标区域
    size: Size<f64, Logical>,      // 要定位的尺寸
) -> Point<f64, Logical> {
    let area_size = area.size.to_point(); // 区域尺寸转点
    let size = size.to_point(); // 目标尺寸转点
    let mut offset = (area_size - size).downscale(2.); // 计算居中偏移
    offset.x = f64::max(offset.x, 0.); // 确保X偏移非负
    offset.y = f64::max(offset.y, 0.); // 确保Y偏移非负
    area.loc + offset // 返回定位点
}

/// 计算浮动窗口的Y轴偏移（呼吸动画效果）
///
/// 公式：
///   amplitude * (sin(2π * now / 3.6) - 1)
/// 其中：
///   amplitude = view_height / 96
///
/// 在合成器中的作用：
/// 为最小化窗口添加呼吸动画效果
pub fn baba_is_float_offset(now: Duration, view_height: f64) -> f64 {
    let now = now.as_secs_f64(); // 当前时间（秒）
    let amplitude = view_height / 96.; // 振幅（与视图高度相关）
    amplitude * ((f64::consts::TAU * now / 3.6).sin() - 1.) // 正弦波动
}

// 条件编译：仅当启用dbus特性时包含
#[cfg(feature = "dbus")]
/// 显示截图完成通知（通过DBus）
pub fn show_screenshot_notification(image_path: Option<PathBuf>) -> anyhow::Result<()> {
    use std::collections::HashMap; // 哈希表

    use zbus::zvariant; // DBus变体类型

    // 建立DBus会话连接
    let conn = zbus::blocking::Connection::session()?;

    // 尝试添加截图作为通知图标
    let mut image_url = None;
    if let Some(path) = image_path {
        match path.canonicalize() {
            Ok(path) => match url::Url::from_file_path(path) {
                Ok(url) => {
                    image_url = Some(url); // 转换成功
                }
                Err(err) => {
                    warn!("error converting screenshot path to file url: {err:?}");
                }
            },
            Err(err) => {
                warn!("error canonicalizing screenshot path: {err:?}");
            }
        }
    }

    // 通知操作列表（空）
    let actions: &[&str] = &[];

    // 发送DBus通知
    conn.call_method(
        Some("org.freedesktop.Notifications"), // 目标服务
        "/org/freedesktop/Notifications",      // 对象路径
        Some("org.freedesktop.Notifications"), // 接口名
        "Notify",                              // 方法名
        &( // 参数：
            "niri",                            // 应用名称
            0u32,                              // 通知ID（0表示新通知）
            image_url.as_ref().map(|url| url.as_str()).unwrap_or(""), // 图标URL
            "Screenshot captured",              // 通知标题
            "You can paste the image from the clipboard.", // 通知内容
            actions,                           // 操作列表
            HashMap::from([                    // 附加提示
                ("transient", zvariant::Value::Bool(true)), // 临时通知
                ("urgency", zvariant::Value::U8(1)), // 中等紧急度
            ]),
            -1,                                // 超时（-1为默认）
        ),
    )?;

    Ok(())
}

/// 故意触发panic的调试函数
///
/// 用于测试崩溃恢复机制
#[inline(never)] // 禁止内联确保函数可见
pub fn cause_panic() {
    let a = Duration::from_secs(1);
    let b = Duration::from_secs(2);
    let _ = a - b; // 故意计算负时长触发panic
}

// 单元测试模块
#[cfg(test)]
mod tests {
    use super::*; // 导入父模块所有内容

    // 测试clamp_preferring_top_left函数
    #[test]
    fn test_clamp_preferring_top_left() {
        // 测试辅助函数（定义区域和矩形，验证结果）
        fn check(
            (ax, ay, aw, ah): (i32, i32, i32, i32), // 区域参数
            (rx, ry, rw, rh): (i32, i32, i32, i32), // 矩形参数
            (ex, ey): (i32, i32),                   // 期望位置
        ) {
            // 构造区域和矩形（转换为f64）
            let area = Rectangle::new(Point::from((ax, ay)), Size::from((aw, ah))).to_f64();
            let mut rect = Rectangle::new(Point::from((rx, ry)), Size::from((rw, rh))).to_f64();
            // 执行定位调整
            clamp_preferring_top_left_in_area(area, &mut rect);
            // 验证结果位置
            assert_eq!(rect.loc, Point::from((ex, ey)).to_f64());
        }

        // 测试用例集：
        // 基础定位
        check((0, 0, 10, 20), (2, 3, 4, 5), (2, 3));
        // 超出左边界
        check((0, 0, 10, 20), (-2, 3, 4, 5), (0, 3));
        // 超出上边界
        check((0, 0, 10, 20), (2, -3, 4, 5), (2, 0));
        // 超出左上角
        check((0, 0, 10, 20), (-2, -3, 4, 5), (0, 0));

        // 带偏移的区域
        check((1, 1, 10, 20), (2, 3, 4, 5), (2, 3));
        check((1, 1, 10, 20), (-2, 3, 4, 5), (1, 3));
        check((1, 1, 10, 20), (2, -3, 4, 5), (2, 1));
        check((1, 1, 10, 20), (-2, -3, 4, 5), (1, 1));

        // 超出右边界
        check((0, 0, 10, 20), (20, 3, 4, 5), (6, 3));
        // 超出下边界
        check((0, 0, 10, 20), (2, 30, 4, 5), (2, 15));
        // 超出右下角
        check((0, 0, 10, 20), (20, 30, 4, 5), (6, 15));

        // 尺寸过大
        check((0, 0, 10, 20), (20, 30, 40, 5), (0, 15)); // 宽度过大
        check((0, 0, 10, 20), (20, 30, 4, 50), (6, 0));  // 高度过大
        check((0, 0, 10, 20), (20, 30, 40, 50), (0, 0)); // 宽高均过大
    }
}