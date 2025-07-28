// cursor.rs
// 此文件实现了光标管理系统，负责加载、缓存和渲染各种光标类型
// 在合成器中，光标管理是用户交互体验的核心组件，支持静态/动态光标和自定义表面光标

use std::cell::RefCell;  // 提供内部可变性
use std::collections::HashMap;  // 哈希表实现
use std::env;  // 环境变量操作
use std::fs::File;  // 文件操作
use std::io::Read;  // 读取文件内容
use std::rc::Rc;  // 引用计数智能指针

use anyhow::{anyhow, Context};  // 错误处理工具
use smithay::backend::allocator::Fourcc;  // 像素格式定义
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;  // 内存渲染缓冲区
use smithay::input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData};  // 光标输入相关
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;  // Wayland表面
use smithay::utils::{IsAlive, Logical, Physical, Point, Transform};  // 实用工具类型
use smithay::wayland::compositor::with_states;  // Wayland状态访问
use xcursor::parser::{parse_xcursor, Image};  // XCursor解析器
use xcursor::CursorTheme;  // 光标主题加载

/// 内置的默认光标图标（左指针）
static FALLBACK_CURSOR_DATA: &[u8] = include_bytes!("../resources/cursor.rgba");

// 缓存类型定义：(光标类型, 缩放比例) -> 光标数据
type XCursorCache = HashMap<(CursorIcon, i32), Option<Rc<XCursor>>>;

/// 光标管理器
/// 负责加载光标主题、管理当前光标状态和缓存光标数据
pub struct CursorManager {
    theme: CursorTheme,            // 当前光标主题
    size: u8,                      // 基础光标大小
    current_cursor: CursorImageStatus, // 当前光标状态（隐藏/表面/命名）
    named_cursor_cache: RefCell<XCursorCache>, // 命名光标缓存（内部可变）
}

impl CursorManager {
    /// 创建新的光标管理器
    /// 参数: theme - 光标主题名称, size - 基础大小
    pub fn new(theme: &str, size: u8) -> Self {
        // 设置环境变量（XCursor库依赖）
        Self::ensure_env(theme, size);

        // 加载光标主题
        let theme = CursorTheme::load(theme);

        Self {
            theme,
            size,
            current_cursor: CursorImageStatus::default_named(), // 初始为默认命名光标
            named_cursor_cache: Default::default(), // 空缓存
        }
    }

    /// 重新加载光标主题
    pub fn reload(&mut self, theme: &str, size: u8) {
        Self::ensure_env(theme, size);
        self.theme = CursorTheme::load(theme);
        self.size = size;
        self.named_cursor_cache.get_mut().clear(); // 清除缓存
    }

    /// 检查光标表面是否存活，若否则清理
    pub fn check_cursor_image_surface_alive(&mut self) {
        if let CursorImageStatus::Surface(surface) = &self.current_cursor {
            // 检查Wayland表面是否仍存活
            if !surface.alive() {
                // 若表面已销毁，回退到默认命名光标
                self.current_cursor = CursorImageStatus::default_named();
            }
        }
    }

    /// 获取当前渲染所需的光标信息
    /// 参数: scale - 缩放比例（考虑HiDPI）
    /// 返回: RenderCursor枚举
    pub fn get_render_cursor(&self, scale: i32) -> RenderCursor {
        match self.current_cursor.clone() {
            // 隐藏状态：不渲染光标
            CursorImageStatus::Hidden => RenderCursor::Hidden,
            // 自定义表面光标
            CursorImageStatus::Surface(surface) => {
                // 从表面状态获取热点位置
                let hotspot = with_states(&surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .hotspot
                });

                RenderCursor::Surface { hotspot, surface }
            }
            // 命名光标（系统主题）
            CursorImageStatus::Named(icon) => self.get_render_cursor_named(icon, scale),
        }
    }

    // 辅助方法：获取命名光标的渲染信息
    fn get_render_cursor_named(&self, icon: CursorIcon, scale: i32) -> RenderCursor {
        // 尝试获取指定光标，失败则使用默认光标
        self.get_cursor_with_name(icon, scale)
            .map(|cursor| RenderCursor::Named {
                icon,
                scale,
                cursor,
            })
            .unwrap_or_else(|| RenderCursor::Named {
                icon: Default::default(), // 回退到默认光标类型
                scale,
                cursor: self.get_default_cursor(scale), // 回退到默认光标数据
            })
    }

    /// 检查当前光标是否为动画光标
    pub fn is_current_cursor_animated(&self, scale: i32) -> bool {
        match &self.current_cursor {
            CursorImageStatus::Hidden => false,
            CursorImageStatus::Surface(_) => false,
            CursorImageStatus::Named(icon) => self
                .get_cursor_with_name(*icon, scale)
                .unwrap_or_else(|| self.get_default_cursor(scale))
                .is_animated_cursor(), // 检查是否有多个帧
        }
    }

    /// 获取指定名称和缩放比例的光标
    pub fn get_cursor_with_name(&self, icon: CursorIcon, scale: i32) -> Option<Rc<XCursor>> {
        // 使用entry API高效处理缓存
        self.named_cursor_cache
            .borrow_mut() // 获取缓存的可变引用
            .entry((icon, scale)) // 创建缓存键
            .or_insert_with_key(|(icon, scale)| {
                // 计算实际所需大小
                let size = self.size as i32 * scale;
                
                // 尝试加载主名称光标
                let mut cursor = Self::load_xcursor(&self.theme, icon.name(), size);

                // 主名称失败时尝试备用名称
                if cursor.is_err() {
                    for name in icon.alt_names() {
                        cursor = Self::load_xcursor(&self.theme, name, size);
                        if cursor.is_ok() {
                            break;
                        }
                    }
                }

                // 记录加载错误
                if let Err(err) = &cursor {
                    warn!("error loading xcursor {}@{size}: {err:?}", icon.name());
                }

                // 默认光标必须有后备
                if *icon == CursorIcon::Default && cursor.is_err() {
                    cursor = Ok(Self::fallback_cursor());
                }

                // 转换为Rc指针
                cursor.ok().map(Rc::new)
            })
            .clone() // 返回缓存的Rc克隆
    }

    /// 获取默认光标（保证存在）
    pub fn get_default_cursor(&self, scale: i32) -> Rc<XCursor> {
        self.get_cursor_with_name(CursorIcon::Default, scale)
            .unwrap()
    }

    /// 获取当前光标状态
    pub fn cursor_image(&self) -> &CursorImageStatus {
        &self.current_cursor
    }

    /// 设置新的光标状态
    pub fn set_cursor_image(&mut self, cursor: CursorImageStatus) {
        self.current_cursor = cursor;
    }

    /// 从文件系统加载光标
    /// 过程:
    ///   1. 查找光标文件路径
    ///   2. 读取文件内容
    ///   3. 解析XCursor格式
    ///   4. 选择最接近请求尺寸的图片
    ///   5. 过滤出该尺寸的所有帧
    fn load_xcursor(theme: &CursorTheme, name: &str, size: i32) -> anyhow::Result<XCursor> {
        let _span = tracy_client::span!("load_xcursor"); // 性能分析

        // 获取光标文件路径
        let path = theme
            .load_icon(name)
            .ok_or_else(|| anyhow!("no default icon"))?;

        // 读取文件内容
        let mut file = File::open(path).context("error opening cursor icon file")?;
        let mut buf = vec![];
        file.read_to_end(&mut buf)
            .context("error reading cursor icon file")?;

        // 解析XCursor文件
        let mut images = parse_xcursor(&buf).context("error parsing cursor icon file")?;

        // 找出最接近请求尺寸的图片
        let (width, height) = images
            .iter()
            .min_by_key(|image| (size - image.size as i32).abs()) // 最小尺寸差
            .map(|image| (image.width, image.height))
            .unwrap();

        // 保留该尺寸的所有帧
        images.retain(move |image| image.width == width && image.height == height);

        // 计算动画总时长
        let animation_duration = images.iter().fold(0, |acc, image| acc + image.delay);

        Ok(XCursor {
            images,
            animation_duration,
        })
    }

    /// 设置XCURSOR环境变量（XCursor库依赖）
    fn ensure_env(theme: &str, size: u8) {
        env::set_var("XCURSOR_THEME", theme);
        env::set_var("XCURSOR_SIZE", size.to_string());
    }

    /// 创建后备光标（内置默认光标）
    fn fallback_cursor() -> XCursor {
        // 创建单帧光标（32x32尺寸）
        let images = vec![Image {
            size: 32,
            width: 64,     // 实际像素宽度（含填充）
            height: 64,    // 实际像素高度
            xhot: 1,       // 热点X坐标（左上角为原点）
            yhot: 1,       // 热点Y坐标
            delay: 0,      // 帧延迟（静态）
            pixels_rgba: Vec::from(FALLBACK_CURSOR_DATA), // RGBA像素数据
            pixels_argb: vec![], // 未使用
        }];

        XCursor {
            images,
            animation_duration: 0,
        }
    }
}

/// 渲染所需的光标信息枚举
pub enum RenderCursor {
    Hidden,  // 不显示光标
    Surface {  // 自定义表面光标
        hotspot: Point<i32, Logical>,  // 热点位置（逻辑坐标）
        surface: WlSurface,            // Wayland表面
    },
    Named {  // 命名光标（主题提供）
        icon: CursorIcon,     // 光标类型
        scale: i32,           // 缩放比例
        cursor: Rc<XCursor>,  // 光标数据
    },
}

// 纹理缓存类型：(光标类型, 缩放比例) -> 纹理列表
type TextureCache = HashMap<(CursorIcon, i32), Vec<MemoryRenderBuffer>>;

/// 光标纹理缓存
/// 避免重复创建相同光标的纹理
#[derive(Default)]
pub struct CursorTextureCache {
    cache: RefCell<TextureCache>,  // 内部可变缓存
}

impl CursorTextureCache {
    /// 清空缓存
    pub fn clear(&mut self) {
        self.cache.get_mut().clear();
    }

    /// 获取或创建光标纹理
    pub fn get(
        &self,
        icon: CursorIcon,   // 光标类型
        scale: i32,         // 缩放比例
        cursor: &XCursor,   // 光标数据
        idx: usize,         // 帧索引
    ) -> MemoryRenderBuffer {
        // 使用entry API高效处理缓存
        self.cache
            .borrow_mut() // 获取可变引用
            .entry((icon, scale)) // 创建缓存键
            .or_insert_with(|| {
                // 缓存未命中时创建纹理
                cursor
                    .frames()  // 获取所有帧
                    .iter()
                    .map(|frame| {
                        // 从RGBA数据创建内存渲染缓冲区
                        MemoryRenderBuffer::from_slice(
                            &frame.pixels_rgba, // 像素数据
                            Fourcc::Argb8888,   // 像素格式（ARGB32）
                            (frame.width as i32, frame.height as i32), // 尺寸
                            scale,               // 缩放比例
                            Transform::Normal,   // 无变换
                            None,                // 无共享内存
                        )
                    })
                    .collect() // 收集为纹理列表
            })[idx]  // 获取指定帧
            .clone() // 克隆纹理（浅拷贝）
    }
}

/// XCursor光标数据结构
/// 包含光标的图像帧和动画信息
pub struct XCursor {
    /// 图像帧列表（可能包含多帧动画）
    images: Vec<Image>,
    /// 动画总时长（毫秒）
    animation_duration: u32,
}

impl XCursor {
    /// 根据时间计算当前帧
    /// 参数: millis - 当前时间（毫秒）
    /// 返回: (帧索引, 帧图像)
    pub fn frame(&self, mut millis: u32) -> (usize, &Image) {
        // 非动画光标直接返回第一帧
        if self.animation_duration == 0 {
            return (0, &self.images[0]);
        }

        // 时间取模（循环动画）
        millis %= self.animation_duration;

        // 查找当前时间对应的帧
        let mut res = 0;
        for (i, img) in self.images.iter().enumerate() {
            if millis < img.delay {
                res = i;
                break;
            }
            millis -= img.delay;
        }

        (res, &self.images[res])
    }

    /// 获取所有帧
    pub fn frames(&self) -> &[Image] {
        &self.images
    }

    /// 检查是否为动画光标
    pub fn is_animated_cursor(&self) -> bool {
        self.images.len() > 1
    }

    /// 获取图像的热点位置（物理坐标）
    pub fn hotspot(image: &Image) -> Point<i32, Physical> {
        (image.xhot as i32, image.yhot as i32).into()
    }
}

/* 光标管理系统流程图

1. 初始化
   +----------------------+
   | 创建CursorManager     |
   | 设置XCURSOR环境变量   |
   | 加载光标主题          |
   +----------------------+

2. 光标状态更新
   +----------------------+
   | 客户端设置光标状态      |
   | (隐藏/表面/命名)      |
   | -> set_cursor_image() |
   +----------------------+

3. 渲染准备
   +----------------------+
   | 每帧调用get_render_cursor() |
   | 根据状态返回:          |
   |   - Hidden: 跳过渲染   |
   |   - Surface: 返回表面  |
   |   - Named: 返回缓存数据 |
   +----------------------+

4. 纹理管理
   +----------------------+
   | 使用CursorTextureCache |
   | 缓存主题光标的纹理      |
   | 避免重复创建           |
   +----------------------+

5. 动画处理
   +----------------------+
   | 对于动画光标:         |
   |   根据当前时间计算帧    |
   |   通过纹理缓存获取纹理   |
   +----------------------+

6. 环境交互
   +----------------------+
   | 拖拽操作: 可能设置Surface |
   | 主题更改: 调用reload()  |
   | 窗口焦点: 可能隐藏光标   |
   +----------------------+
*/

/* Wayland光标协议说明
1. 光标类型:
   - 隐藏: 不显示光标
   - 表面: 客户端提供自定义光标表面
   - 命名: 使用服务端主题光标

2. 热点(hotspot):
   - 光标的点击位置（如箭头尖端）
   - 逻辑坐标: 与表面内容无关的坐标系统
   - 物理坐标: 实际像素位置

3. 动态光标:
   - 通过多帧图像实现
   - 每帧有显示时长(delay)
   - 循环播放形成动画
*/