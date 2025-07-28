use std::collections::hash_map::Entry;  // HashMap项访问枚举
use std::collections::HashMap;           // 哈希表集合
use std::sync::atomic::{AtomicBool, Ordering};  // 原子布尔类型和内存排序
use std::sync::{Arc, Mutex, OnceLock};  // 线程安全原语：原子引用计数、互斥锁、一次性锁

use anyhow::Context;  // 错误上下文处理
use futures_util::StreamExt;  // 异步流扩展
use zbus::fdo::{self, RequestNameFlags};  // DBus基础对象和标志
use zbus::message::Header;  // DBus消息头
use zbus::names::{OwnedUniqueName, UniqueName};  // DBus唯一名称类型
use zbus::zvariant::NoneValue;  // DBus空值表示
use zbus::{interface, Task};  // zbus接口宏和异步任务

use super::Start;  // 从父模块引入Start trait

// 屏幕保护服务实现
// 作用：实现freedesktop.org屏幕保护接口，管理屏幕空闲抑制
pub struct ScreenSaver {
    is_inhibited: Arc<AtomicBool>,  // 原子标记：当前是否有抑制存在
    is_broken: Arc<AtomicBool>,    // 原子标记：监控任务是否失败
    inhibitors: Arc<Mutex<HashMap<u32, OwnedUniqueName>>>,  // 互斥保护的抑制器映射表（cookie -> 客户端名）
    counter: u32,                  // 抑制器cookie计数器
    monitor_task: Arc<OnceLock<Task<()>>>,  // 一次性锁保护的监控任务
}

// 实现DBus接口（使用zbus的interface宏）
// 接口名：org.freedesktop.ScreenSaver
#[interface(name = "org.freedesktop.ScreenSaver")]
impl ScreenSaver {
    // 抑制屏幕保护方法（异步函数）
    // 参数：
    //   hdr - DBus消息头（通过zbus属性注入）
    //   application_name - 应用名称
    //   reason_for_inhibit - 抑制原因
    // 返回：唯一抑制器cookie
    async fn inhibit(
        &mut self,
        #[zbus(header)] hdr: Header<'_>,  // zbus宏：自动注入消息头
        application_name: &str,
        reason_for_inhibit: &str,
    ) -> fdo::Result<u32> {
        // 记录调试信息
        trace!(
            "fdo inhibit, app: `{application_name}`, reason: `{reason_for_inhibit}`, owner: {:?}",
            hdr.sender()
        );

        // 获取发送方唯一名称
        let Some(name) = hdr.sender() else {
            return Err(fdo::Error::Failed(String::from("no sender")));
        };
        let name = OwnedUniqueName::from(name.to_owned());  // 转换为自有类型

        // 锁定抑制器映射表（互斥锁）
        let mut inhibitors = self.inhibitors.lock().unwrap();

        let mut cookie = None;
        // 尝试3次生成唯一cookie
        for _ in 0..3 {
            // 递增计数器（处理回绕）
            self.counter = self.counter.wrapping_add(1);
            if self.counter == 0 {
                self.counter += 1;  // 跳过0值
            }

            // 尝试插入新条目
            if let Entry::Vacant(entry) = inhibitors.entry(self.counter) {
                entry.insert(name);
                // 设置抑制状态
                self.is_inhibited.store(true, Ordering::SeqCst);
                cookie = Some(self.counter);
                break;
            }
        }

        // 返回cookie或错误
        cookie.ok_or_else(|| fdo::Error::Failed(String::from("no available cookie")))
    }

    // 解除抑制方法（异步函数）
    // 参数：cookie - 要解除的抑制器标识
    async fn un_inhibit(&mut self, cookie: u32) -> fdo::Result<()> {
        trace!("fdo uninhibit, cookie: {cookie}");

        let mut inhibitors = self.inhibitors.lock().unwrap();

        // 移除指定cookie
        if inhibitors.remove(&cookie).is_some() {
            // 检查是否还有抑制器
            if inhibitors.is_empty() {
                self.is_inhibited.store(false, Ordering::SeqCst);
            }

            Ok(())
        } else {
            Err(fdo::Error::Failed(String::from("invalid cookie")))
        }
    }
}

impl ScreenSaver {
    // 构造函数
    pub fn new(is_inhibited: Arc<AtomicBool>) -> Self {
        Self {
            is_inhibited,  // 使用传入的原子状态
            is_broken: Arc::new(AtomicBool::new(false)),  // 初始未损坏
            inhibitors: Arc::new(Mutex::new(HashMap::new())),  // 空抑制器映射
            counter: 0,    // 计数器从0开始
            monitor_task: Arc::new(OnceLock::new()),  // 未初始化的任务容器
        }
    }
}

// 监控客户端消失的异步任务
// 作用：当客户端断开连接时自动清理其抑制器
async fn monitor_disappeared_clients(
    conn: &zbus::Connection,  // DBus连接
    is_inhibited: Arc<AtomicBool>,  // 抑制状态
    inhibitors: Arc<Mutex<HashMap<u32, OwnedUniqueName>>>,  // 抑制器映射
) -> anyhow::Result<()> {
    // 创建DBus代理
    let proxy = fdo::DBusProxy::new(conn)
        .await
        .context("error creating a DBusProxy")?;

    // 创建名称所有者变更事件流
    // 过滤条件：参数2为null（表示名称消失）
    let mut stream = proxy
        .receive_name_owner_changed_with_args(&[(2, UniqueName::null_value())])
        .await
        .context("error creating a NameOwnerChanged stream")?;

    // 处理事件流
    while let Some(signal) = stream.next().await {
        // 获取事件参数
        let args = signal
            .args()
            .context("error retrieving NameOwnerChanged args")?;

        // 获取旧所有者名称（非空）
        let Some(name) = &**args.old_owner() else {
            continue;
        };

        // 检查是否是名称消失事件
        if args.new_owner().is_none() {
            trace!("fdo ScreenSaver client disappeared: {name}");

            // 锁定抑制器映射
            let mut inhibitors = inhibitors.lock().unwrap();
            // 移除该客户端的所有抑制器
            inhibitors.retain(|_, owner| owner != name);
            // 更新抑制状态
            is_inhibited.store(!inhibitors.is_empty(), Ordering::SeqCst);
        } else {
            // 理论上不应发生（因已过滤）
            error!("non-null new_owner should've been filtered out");
        }
    }

    Ok(())
}

// 实现Start trait以启动DBus服务
impl Start for ScreenSaver {
    fn start(self) -> anyhow::Result<zbus::blocking::Connection> {
        // 克隆共享状态
        let is_inhibited = self.is_inhibited.clone();
        let is_broken = self.is_broken.clone();
        let inhibitors = self.inhibitors.clone();
        let monitor_task = self.monitor_task.clone();

        // 创建DBus会话连接
        let conn = zbus::blocking::Connection::session()?;
        // 设置服务名标志
        let flags = RequestNameFlags::AllowReplacement
            | RequestNameFlags::ReplaceExisting
            | RequestNameFlags::DoNotQueue;

        // 注册DBus对象
        conn.object_server()
            .at("/org/freedesktop/ScreenSaver", self)?;
        // 请求服务名
        conn.request_name_with_flags("org.freedesktop.ScreenSaver", flags)?;

        // 获取异步连接引用
        let async_conn = conn.inner();
        // 创建监控任务闭包
        let future = {
            let conn = async_conn.clone();
            async move {
                // 执行监控任务
                if let Err(err) =
                    monitor_disappeared_clients(&conn, is_inhibited.clone(), inhibitors.clone())
                        .await
                {
                    // 任务失败处理
                    warn!("error monitoring org.freedesktop.ScreenSaver clients: {err:?}");
                    // 标记服务损坏
                    is_broken.store(true, Ordering::SeqCst);
                    // 清除抑制状态
                    is_inhibited.store(false, Ordering::SeqCst);
                    // 清空抑制器
                    inhibitors.lock().unwrap().clear();
                }
            }
        };
        // 在zbus执行器中生成任务
        let task = async_conn
            .executor()
            .spawn(future, "monitor disappearing clients");
        // 存储任务到OnceLock
        monitor_task.set(task).unwrap();

        Ok(conn)  // 返回连接
    }
}

/*
服务工作流程图：

+------------------+     +-------------------+     +-----------------+     +------------------+
| DBus客户端        |     | ScreenSaver服务    |     | 监控任务         |     | DBus守护进程      |
+--------+---------+     +---------+---------+     +--------+--------+     +---------+--------+
         | 调用inhibit()            |                         |                        |
         |----------------------->|                         |                        |
         |                        | 存储cookie<->客户端映射   |                        |
         |                        |------------------------>| (存储到共享状态)        |
         |       返回cookie        |                         |                        |
         |<-----------------------|                         |                        |
         |                        |                         |                        |
         | 调用un_inhibit()        |                         |                        |
         |----------------------->|                         |                        |
         |                        | 移除指定cookie           |                        |
         |                        |------------------------>| (更新共享状态)          |
         |       返回成功          |                         |                        |
         |<-----------------------|                         |                        |
         |                        |                         | 监听NameOwnerChanged   |
         |                        |                         |----------------------->|
         |                        |                         |                        |
         | 客户端断开连接           |                         |                        |
         |------------------------+------------------------>| (发送事件)              |
         |                        |                         |                        |
         |                        |                         | 收到名称消失事件         |
         |                        |<------------------------|                        |
         |                        | 清理该客户端所有抑制器    |                        |
         |                        |------------------------>| (更新共享状态)          |
*/