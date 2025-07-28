use std::collections::HashMap;
use std::path::PathBuf;

use niri_ipc::PickedColor;  // IPC颜色结构体，用于颜色选择
use zbus::fdo::{self, RequestNameFlags};  // zbus框架的DBus对象接口
use zbus::zvariant::OwnedValue;  // 类型安全的DBus值容器
use zbus::{interface, zvariant};  // zbus宏和数据类型

use super::Start;  // 从父模块引入Start trait

// 截图服务实现结构体
// 作用：处理GNOME Shell的截图和取色DBus请求
pub struct Screenshot {
    to_niri: calloop::channel::Sender<ScreenshotToNiri>,  // 发送消息到主循环的通道
    from_niri: async_channel::Receiver<NiriToScreenshot>,  // 接收主循环响应的通道
}

// 发送到主循环的消息枚举
// 枚举解释：Rust中的枚举允许定义多种可能的消息类型
pub enum ScreenshotToNiri {
    TakeScreenshot { include_cursor: bool },  // 截图请求（是否包含光标）
    PickColor(async_channel::Sender<Option<PickedColor>>),  // 取色请求（包含结果通道）
}

// 主循环返回的消息枚举
pub enum NiriToScreenshot {
    ScreenshotResult(Option<PathBuf>),  // 截图结果（文件路径）
}

// 实现DBus接口（使用zbus的interface宏）
// 接口名：org.gnome.Shell.Screenshot
#[interface(name = "org.gnome.Shell.Screenshot")]
impl Screenshot {
    // 截图方法（异步函数）
    // DBus方法参数：
    //   include_cursor - 是否包含鼠标光标
    //   _flash - 是否闪烁屏幕（未实现）
    //   _filename - 建议保存路径（未使用）
    // 返回：(成功标志, 实际保存路径)
    async fn screenshot(
        &self,
        include_cursor: bool,
        _flash: bool,
        _filename: PathBuf,
    ) -> fdo::Result<(bool, PathBuf)> {
        // 发送截图请求到主事件循环
        if let Err(err) = self
            .to_niri
            .send(ScreenshotToNiri::TakeScreenshot { include_cursor })
        {
            warn!("error sending message to niri: {err:?}");
            // 返回DBus错误
            return Err(fdo::Error::Failed("internal error".to_owned()));
        }

        // 等待主循环返回结果
        let filename = match self.from_niri.recv().await {
            Ok(NiriToScreenshot::ScreenshotResult(Some(filename))) => filename,  // 成功获取路径
            Ok(NiriToScreenshot::ScreenshotResult(None)) => {
                // 内部错误：无路径返回
                return Err(fdo::Error::Failed("internal error".to_owned()));
            }
            Err(err) => {
                // 通道接收错误
                warn!("error receiving message from niri: {err:?}");
                return Err(fdo::Error::Failed("internal error".to_owned()));
            }
        };

        Ok((true, filename))  // 返回成功和路径
    }

    // 取色方法（异步函数）
    // 返回：包含颜色值的字典（DBus要求格式）
    async fn pick_color(&self) -> fdo::Result<HashMap<String, OwnedValue>> {
        // 创建有界通道（容量为1）
        let (tx, rx) = async_channel::bounded(1);
        // 发送取色请求到主循环
        if let Err(err) = self.to_niri.send(ScreenshotToNiri::PickColor(tx)) {
            warn!("error sending pick color message to niri: {err:?}");
            return Err(fdo::Error::Failed("internal error".to_owned()));
        }

        // 等待颜色结果
        let color = match rx.recv().await {
            Ok(Some(color)) => color,  // 成功获取颜色
            Ok(None) => {
                // 用户未选择颜色
                return Err(fdo::Error::Failed("no color picked".to_owned()));
            }
            Err(err) => {
                // 通道接收错误
                warn!("error receiving message from niri: {err:?}");
                return Err(fdo::Error::Failed("internal error".to_owned()));
            }
        };

        // 构造DBus响应字典
        let mut result = HashMap::new();
        let [r, g, b] = color.rgb;  // 解构RGB值
        // 插入颜色值（转换为DBus元组格式）
        result.insert(
            "color".to_string(),
            zvariant::OwnedValue::try_from(zvariant::Value::from((r, g, b))).unwrap(),
        );

        Ok(result)
    }
}

impl Screenshot {
    // 构造函数
    pub fn new(
        to_niri: calloop::channel::Sender<ScreenshotToNiri>,
        from_niri: async_channel::Receiver<NiriToScreenshot>,
    ) -> Self {
        Self { to_niri, from_niri }
    }
}

// 实现Start trait以启动DBus服务
impl Start for Screenshot {
    fn start(self) -> anyhow::Result<zbus::blocking::Connection> {
        // 创建会话总线连接
        let conn = zbus::blocking::Connection::session()?;
        
        // 设置服务名标志：
        //   AllowReplacement - 允许其他服务替换
        //   ReplaceExisting - 替换现有同名服务
        //   DoNotQueue - 不排队等待
        let flags = RequestNameFlags::AllowReplacement
            | RequestNameFlags::ReplaceExisting
            | RequestNameFlags::DoNotQueue;

        // 在指定路径注册DBus对象
        conn.object_server()
            .at("/org/gnome/Shell/Screenshot", self)?;
        // 请求服务名（带标志）
        conn.request_name_with_flags("org.gnome.Shell.Screenshot", flags)?;

        Ok(conn)  // 返回连接对象
    }
}

/*
截图服务工作流程：
+------------------+       +-------------------+       +-----------------+
| DBus客户端        |       | Screenshot服务     |       | Niri主循环       |
+--------+---------+       +---------+---------+       +--------+--------+
         | 调用screenshot()          |                         |
         |------------------------->|                         |
         |                          | 发送TakeScreenshot消息   |
         |                          |------------------------>|
         |                          |                         |--+
         |                          |                         |  | 执行截图
         |                          |                         |<-+
         |                          |      返回ScreenshotResult|
         |                          |<------------------------|
         |       返回(成功, 路径)    |                         |
         |<--------------------------|                         |

取色工作流程：
+------------------+       +-------------------+       +-----------------+
| DBus客户端        |       | Screenshot服务     |       | Niri主循环       |
+--------+---------+       +---------+---------+       +--------+--------+
         | 调用pick_color()           |                         |
         |-------------------------->|                         |
         |                          | 发送PickColor(带通道)     |
         |                          |------------------------>|
         |                          |                         |--+
         |                          |                         |  | 执行取色
         |                          |                         |<-+
         |                          |   通过通道发送颜色结果     |
         |                          |<------------------------|
         |       返回颜色字典         |                         |
         |<--------------------------|                         |
*/