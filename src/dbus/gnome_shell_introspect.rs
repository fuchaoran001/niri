use std::collections::HashMap;  // 哈希表集合

use zbus::fdo::{self, RequestNameFlags};  // DBus基础对象和标志
use zbus::interface;  // zbus接口宏
use zbus::object_server::SignalEmitter;  // DBus信号发射器
use zbus::zvariant::{SerializeDict, Type, Value};  // DBus数据类型支持

use super::Start;  // 从父模块引入Start trait

// GNOME Shell自省服务结构体
// 作用：提供窗口信息查询接口，支持GNOME扩展生态系统
pub struct Introspect {
    to_niri: calloop::channel::Sender<IntrospectToNiri>,  // 发送消息到主循环的通道
    from_niri: async_channel::Receiver<NiriToIntrospect>,  // 接收主循环响应的通道
}

// 发送到主循环的消息枚举
pub enum IntrospectToNiri {
    GetWindows,  // 请求获取当前窗口列表
}

// 主循环返回的消息枚举
pub enum NiriToIntrospect {
    Windows(HashMap<u64, WindowProperties>),  // 返回窗口ID到属性的映射
}

// 窗口属性结构体（使用zbus宏实现DBus字典序列化）
// 作用：描述窗口的元数据信息
#[derive(Debug, SerializeDict, Type, Value)]  // 自动派生序列化和类型实现
#[zvariant(signature = "dict")]  // DBus类型签名为字典
pub struct WindowProperties {
    /// 窗口标题
    pub title: String,
    
    /// 窗口应用ID
    ///
    /// 注意：这实际上是.desktop文件名，GNOME Shell内部会跟踪匹配Wayland应用ID和桌面文件。
    /// 目前niri尚未实现此匹配，因此某些功能（如xdg-desktop-portal-gnome的窗口列表图标）可能缺失。
    #[zvariant(rename = "app-id")]  // DBus字段重命名为"app-id"
    pub app_id: String,
}

// 实现DBus接口（使用zbus的interface宏）
// 接口名：org.gnome.Shell.Introspect
#[interface(name = "org.gnome.Shell.Introspect")]
impl Introspect {
    // 获取当前窗口列表方法（异步函数）
    // 返回：窗口ID到属性的映射字典
    async fn get_windows(&self) -> fdo::Result<HashMap<u64, WindowProperties>> {
        // 发送获取窗口请求到主事件循环
        if let Err(err) = self.to_niri.send(IntrospectToNiri::GetWindows) {
            warn!("error sending message to niri: {err:?}");
            return Err(fdo::Error::Failed("internal error".to_owned()));
        }

        // 等待并处理响应
        match self.from_niri.recv().await {
            Ok(NiriToIntrospect::Windows(windows)) => Ok(windows),  // 成功返回窗口字典
            Err(err) => {
                warn!("error receiving message from niri: {err:?}");
                Err(fdo::Error::Failed("internal error".to_owned()))
            }
        }
    }

    // 窗口变更信号（暂未实现）
    // FIXME: 需要实现窗口变化时触发此信号（待事件流IPC基础设施完善）
    // 信号解释：DBus信号用于主动通知客户端状态变化
    #[zbus(signal)]  // zbus信号宏
    pub async fn windows_changed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;
}

impl Introspect {
    // 构造函数
    pub fn new(
        to_niri: calloop::channel::Sender<IntrospectToNiri>,
        from_niri: async_channel::Receiver<NiriToIntrospect>,
    ) -> Self {
        Self { to_niri, from_niri }
    }
}

// 实现Start trait以启动DBus服务
impl Start for Introspect {
    fn start(self) -> anyhow::Result<zbus::blocking::Connection> {
        // 创建DBus会话连接
        let conn = zbus::blocking::Connection::session()?;
        // 设置服务名标志
        let flags = RequestNameFlags::AllowReplacement
            | RequestNameFlags::ReplaceExisting
            | RequestNameFlags::DoNotQueue;

        // 注册DBus对象到指定路径
        conn.object_server()
            .at("/org/gnome/Shell/Introspect", self)?;
        // 请求服务名
        conn.request_name_with_flags("org.gnome.Shell.Introspect", flags)?;

        Ok(conn)  // 返回连接
    }
}

/*
自省服务工作流程：

+------------------+     +-------------------+     +-----------------+
| DBus客户端        |     | Introspect服务     |     | Niri主循环       |
+--------+---------+     +---------+---------+     +--------+--------+
         | 调用get_windows()        |                         |
         |----------------------->|                         |
         |                         | 发送GetWindows请求       |
         |                         |------------------------>|
         |                         |                         |--+
         |                         |                         |  | 收集窗口信息
         |                         |                         |<-+
         |                         |     返回Windows消息      |
         |                         |<------------------------|
         |       返回窗口字典        |                         |
         |<-----------------------|                         |

未来扩展：
+------------------+     +-------------------+     +-----------------+
| DBus客户端        |     | Introspect服务     |     | Niri主循环       |
+--------+---------+     +---------+---------+     +--------+--------+
         |                         |       窗口创建/销毁/变更 |
         |                         |<------------------------+
         |                         |                         |
         |     发射windows_changed信号 |                         |
         |<--------------------------+                         |
*/