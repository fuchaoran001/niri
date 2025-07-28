use std::os::unix::net::UnixStream;  // Unix域套接字

use zbus::{fdo, interface, zvariant};  // zbus框架组件

use super::Start;  // 从父模块引入Start trait
use crate::niri::NewClient;  // 新客户端结构体

// Mutter服务通道实现
// 作用：提供DBus接口让外部进程建立Wayland连接
pub struct ServiceChannel {
    to_niri: calloop::channel::Sender<NewClient>,  // 发送新客户端到主循环的通道
}

// 实现DBus接口（使用zbus的interface宏）
// 接口名：org.gnome.Mutter.ServiceChannel
#[interface(name = "org.gnome.Mutter.ServiceChannel")]
impl ServiceChannel {
    // 打开Wayland服务连接方法（异步函数）
    // 参数：service_client_type - 客户端类型（目前只支持1）
    // 返回：Unix套接字文件描述符
    async fn open_wayland_service_connection(
        &mut self,
        service_client_type: u32,
    ) -> fdo::Result<zvariant::OwnedFd> {
        // 验证客户端类型（目前只支持类型1）
        if service_client_type != 1 {
            return Err(fdo::Error::InvalidArgs(
                "Invalid service client type".to_owned(),
            ));
        }

        // 创建一对连接的Unix套接字
        let (sock1, sock2) = UnixStream::pair().unwrap();
        // 构建新客户端对象
        let client = NewClient {
            client: sock2,  // 主循环端的套接字
            restricted: false,  // 非受限客户端
            // FIXME: 当前无法通过DBus获取客户端PID
            credentials_unknown: true,  // 标记凭证未知
        };
        // 发送新客户端到主循环
        if let Err(err) = self.to_niri.send(client) {
            warn!("error sending message to niri: {err:?}");
            return Err(fdo::Error::Failed("internal error".to_owned()));
        }

        // 将套接字转换为OwnedFd返回给调用者
        Ok(zvariant::OwnedFd::from(std::os::fd::OwnedFd::from(sock1)))
    }
}

impl ServiceChannel {
    // 构造函数
    pub fn new(to_niri: calloop::channel::Sender<NewClient>) -> Self {
        Self { to_niri }
    }
}

// 实现Start trait以启动DBus服务
impl Start for ServiceChannel {
    fn start(self) -> anyhow::Result<zbus::blocking::Connection> {
        // 创建DBus连接构建器
        let conn = zbus::blocking::connection::Builder::session()?
            // 设置服务名
            .name("org.gnome.Mutter.ServiceChannel")?
            // 注册DBus对象
            .serve_at("/org/gnome/Mutter/ServiceChannel", self)?
            // 构建连接
            .build()?;
        Ok(conn)
    }
}

/*
服务通道工作流程：

+------------------+     +-------------------+     +-----------------+     +------------------+
| DBus客户端        |     | ServiceChannel服务 |     | Niri主循环       |     | Wayland客户端     |
+--------+---------+     +---------+---------+     +--------+--------+     +---------+--------+
         | 调用open_wayland_connection |                         |                        |
         |----------------------->|                         |                        |
         |                         | 创建Unix套接字对 (sock1, sock2) |                        |
         |                         |                         |                        |
         |                         | 构建NewClient对象(sock2) |                        |
         |                         |------------------------>|                        |
         |                         |                         | 创建Wayland客户端       |
         |                         |                         |----------------------->|
         |       返回sock1文件描述符 |                         |                        |
         |<-----------------------|                         |                        |
         | 通过sock1连接Wayland协议 |                         |                        |
         |-------------------------------------------------->|                        |
*/