//! 默认显示器缩放比例计算模块
//!
//! 本模块参考Mutter（GNOME窗口管理器）的实现逻辑和测试：
//! <https://gitlab.gnome.org/GNOME/mutter/-/blob/gnome-46/src/backends/meta-monitor.c>

use smithay::utils::{Physical, Raw, Size}; // 导入尺寸类型（物理/原始坐标系）

// 缩放比例范围限制
const MIN_SCALE: i32 = 1;   // 最小缩放比例
const MAX_SCALE: i32 = 4;   // 最大缩放比例
const STEPS: i32 = 4;       // 缩放步进单位（1/4=0.25）
const MIN_LOGICAL_AREA: i32 = 800 * 480; // 最小逻辑区域（800×480像素）

// 目标DPI值
const MOBILE_TARGET_DPI: f64 = 135.;      // 移动设备目标DPI
const LARGE_TARGET_DPI: f64 = 110.;       // 大型设备目标DPI
const LARGE_MIN_SIZE_INCHES: f64 = 20.;   // 区分移动/大型设备的对角线尺寸阈值

/// 计算显示器的理想缩放比例
///
/// 算法流程：
/// 1. 计算屏幕对角线尺寸（英寸）
/// 2. 根据尺寸选择目标DPI（移动设备/大型设备）
/// 3. 计算物理DPI（基于分辨率和物理尺寸）
/// 4. 计算完美缩放比例 = 物理DPI / 目标DPI
/// 5. 从支持的比例中选取最接近完美比例的值
///
/// 在合成器中的作用：
/// 自动为不同尺寸/分辨率的显示器设置合适的默认缩放，
/// 确保界面元素大小符合人体工学
pub fn guess_monitor_scale(
    size_mm: Size<i32, Raw>,      // 物理尺寸（毫米）
    resolution: Size<i32, Physical> // 物理分辨率（像素）
) -> f64 {
    // 无效尺寸检查（避免除零错误）
    if size_mm.w == 0 || size_mm.h == 0 {
        return 1.; // 默认缩放
    }

    // 计算对角线尺寸（英寸）：
    // 1. 毫米转英寸：/25.4
    // 2. 勾股定理：sqrt(w² + h²)
    let diag_inches = f64::from(size_mm.w * size_mm.w + size_mm.h * size_mm.h).sqrt() / 25.4;

    // 根据尺寸选择目标DPI
    let target_dpi = if diag_inches < LARGE_MIN_SIZE_INCHES {
        MOBILE_TARGET_DPI  // 小尺寸设备使用更高DPI
    } else {
        LARGE_TARGET_DPI   // 大尺寸设备使用稍低DPI
    };

    // 计算物理DPI：
    // 公式：sqrt(水平像素² + 垂直像素²) / 对角线英寸
    let physical_dpi =
        f64::from(resolution.w * resolution.w + resolution.h * resolution.h).sqrt() / diag_inches;
    
    // 计算完美缩放比例（物理DPI / 目标DPI）
    let perfect_scale = physical_dpi / target_dpi;

    // 从支持的比例中查找最接近完美比例的值
    supported_scales(resolution)
        // 计算每个比例与完美比例的绝对差值
        .map(|scale| (scale, (scale - perfect_scale).abs()))
        // 按差值排序（最小差值优先）
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        // 提取比例值（找不到时使用1.0）
        .map_or(1., |(scale, _)| scale)
}

/// 生成给定分辨率支持的缩放比例迭代器
///
/// 支持条件：
/// 1. 比例在MIN_SCALE到MAX_SCALE之间（步进STEPS）
/// 2. 应用缩放后逻辑区域≥MIN_LOGICAL_AREA
pub fn supported_scales(resolution: Size<i32, Physical>) -> impl Iterator<Item = f64> {
    // 生成比例序列：MIN_SCALE*STEPS到MAX_SCALE*STEPS
    (MIN_SCALE * STEPS..=MAX_SCALE * STEPS)
        // 转换为浮点比例：x/STEPS
        .map(|x| f64::from(x) / f64::from(STEPS))
        // 过滤无效比例（逻辑区域过小）
        .filter(move |scale| is_valid_for_resolution(resolution, *scale))
}

/// 检查缩放比例对分辨率是否有效
///
/// 有效性标准：
/// 应用缩放后的逻辑区域面积 ≥ MIN_LOGICAL_AREA
fn is_valid_for_resolution(resolution: Size<i32, Physical>, scale: f64) -> bool {
    // 计算逻辑分辨率（物理分辨率 / 缩放比例）
    let logical = resolution.to_f64().to_logical(scale).to_i32_round::<i32>();
    // 检查区域是否达标
    logical.w * logical.h >= MIN_LOGICAL_AREA
}

/// 将缩放比例调整为最接近的可精确表示值
///
/// Wayland分数缩放协议要求：
/// 缩放比例必须是 N/120 的形式（N为整数）
///
/// 转换公式：
///   scale = round(scale * 120) / 120
pub fn closest_representable_scale(scale: f64) -> f64 {
    // 分数缩放分母（Wayland协议规定）
    const FRACTIONAL_SCALE_DENOM: f64 = 120.;

    // 四舍五入到最近的1/120分数
    (scale * FRACTIONAL_SCALE_DENOM).round() / FRACTIONAL_SCALE_DENOM
}

// 单元测试模块
#[cfg(test)]
mod tests {
    use insta::assert_snapshot; // 快照测试库
    use super::*; // 导入父模块

    // 测试辅助函数
    fn check(size_mm: (i32, i32), resolution: (i32, i32)) -> f64 {
        guess_monitor_scale(Size::from(size_mm), Size::from(resolution))
    }

    // 测试各种设备的缩放比例计算
    #[test]
    fn test_guess_monitor_scale() {
        // Librem 5（手机）：逻辑区域不足时提升缩放
        assert_snapshot!(check((65, 129), (720, 1440)), @"1.5");
        // OnePlus 6（手机）
        assert_snapshot!(check((68, 144), (1080, 2280)), @"2.5");
        // Google Pixel 6a（手机）
        assert_snapshot!(check((64, 142), (1080, 2400)), @"2.5");
        // 13英寸MacBook Retina
        assert_snapshot!(check((286, 179), (2560, 1600)), @"1.75");
        // Surface Laptop Studio
        assert_snapshot!(check((303, 202), (2400, 1600)), @"1.5");
        // Dell XPS 9320（笔记本）
        assert_snapshot!(check((290, 180), (3840, 2400)), @"2.5");
        // Lenovo ThinkPad X1 Yoga（笔记本）
        assert_snapshot!(check((300, 190), (3840, 2400)), @"2.5");
        // 23英寸1080p显示器
        assert_snapshot!(check((509, 286), (1920, 1080)), @"1");
        // 23英寸4K显示器
        assert_snapshot!(check((509, 286), (3840, 2160)), @"1.75");
        // 27英寸4K显示器
        assert_snapshot!(check((598, 336), (3840, 2160)), @"1.5");
        // 32英寸4K显示器
        assert_snapshot!(check((708, 398), (3840, 2160)), @"1.25");
        // 25英寸4K显示器（理想比例1.60，取最接近的1.5）
        assert_snapshot!(check((554, 312), (3840, 2160)), @"1.5");
        // 23.5英寸4K显示器（理想比例1.70，取最接近的1.75）
        assert_snapshot!(check((522, 294), (3840, 2160)), @"1.75");
        // Lenovo Legion游戏本16英寸
        assert_snapshot!(check((340, 210), (2560, 1600)), @"1.5");
        // Acer Nitro显示器31.5英寸
        assert_snapshot!(check((700, 390), (2560, 1440)), @"1");
        // Surface Pro 6（平板）
        assert_snapshot!(check((260, 170), (2736, 1824)), @"2");
    }

    // 测试未知物理尺寸的情况
    #[test]
    fn guess_monitor_scale_unknown_size() {
        assert_eq!(check((0, 0), (1920, 1080)), 1.); // 应返回默认值1.0
    }

    // 测试缩放比例舍入功能
    #[test]
    fn test_round_scale() {
        // 精确匹配
        assert_snapshot!(closest_representable_scale(1.3), @"1.3");
        // 1.31 → 1.3083 (157/120)
        assert_snapshot!(closest_representable_scale(1.31), @"1.3083333333333333");
        // 1.32 → 1.3167 (158/120)
        assert_snapshot!(closest_representable_scale(1.32), @"1.3166666666666667");
        // 1.33 → 1.3333 (160/120=4/3)
        assert_snapshot!(closest_representable_scale(1.33), @"1.3333333333333333");
        // 1.34 → 1.3417 (161/120)
        assert_snapshot!(closest_representable_scale(1.34), @"1.3416666666666666");
        // 精确匹配
        assert_snapshot!(closest_representable_scale(1.35), @"1.35");
    }
}