use zbus::blocking::Connection;
use zbus::object_server::Interface;

use crate::niri::State;

// 定义DBus接口模块
pub mod freedesktop_screensaver;       // 实现freedesktop.org屏幕保护接口
pub mod gnome_shell_introspect;        // GNOME Shell自省接口实现
pub mod gnome_shell_screenshot;        // GNOME Shell截图接口实现
pub mod mutter_display_config;         // Mutter显示配置接口实现
pub mod mutter_service_channel;        // Mutter服务通道接口实现

// 导入各接口实现
use self::freedesktop_screensaver::ScreenSaver;
use self::gnome_shell_introspect::Introspect;
use self::mutter_display_config::DisplayConfig;
use self::mutter_service_channel::ServiceChannel;

// 定义Start trait：统一DBus接口启动方法
// trait解释：Rust中的接口定义，要求实现类型提供特定功能
trait Start: Interface {
    // 启动DBus服务并返回连接
    fn start(self) -> anyhow::Result<zbus::blocking::Connection>;
}

// DBus服务管理器
// 结构体解释：存储所有DBus服务的连接对象
#[derive(Default)]
pub struct DBusServers {
    pub conn_service_channel: Option<Connection>,  // Mutter服务通道连接
    pub conn_display_config: Option<Connection>,   // 显示配置服务连接
    pub conn_screen_saver: Option<Connection>,      // 屏幕保护服务连接
    pub conn_screen_shot: Option<Connection>,       // 截图服务连接
    pub conn_introspect: Option<Connection>,        // 自省服务连接
}

impl DBusServers {
    // 启动所有DBus服务
    // 参数说明：
    //   state - 合成器全局状态
    //   is_session_instance - 标记当前是否为会话主实例
    pub fn start(state: &mut State, is_session_instance: bool) {
        // 性能监控：使用tracy客户端标记代码段
        let _span = tracy_client::span!("DBusServers::start");

        let backend = &state.backend;
        let niri = &mut state.niri;
        // 借用配置：Rust的借用机制确保安全并发访问
        let config = niri.config.borrow();

        let mut dbus = Self::default();  // 创建默认DBus服务管理器

        // 仅会话主实例启动的服务
        if is_session_instance {
            // 创建Mutter服务通道
            // calloop通道：用于跨线程通信的事件通道
            let (to_niri, from_service_channel) = calloop::channel::channel();
            let service_channel = ServiceChannel::new(to_niri);
            
            // 将通道插入事件循环
            // 事件循环：合成器核心的事件处理机制
            niri.event_loop
                .insert_source(from_service_channel, move |event, _, state| match event {
                    // 处理新客户端连接
                    calloop::channel::Event::Msg(new_client) => {
                        state.niri.insert_client(new_client);
                    }
                    calloop::channel::Event::Closed => (),
                })
                .unwrap();
            // 启动服务并存储连接
            dbus.conn_service_channel = try_start(service_channel);
        }

        // 所有实例或调试模式下启动的服务
        if is_session_instance || config.debug.dbus_interfaces_in_non_session_instances {
            // 显示配置服务
            let (to_niri, from_display_config) = calloop::channel::channel();
            // 创建显示配置对象（包含IPC输出信息）
            let display_config = DisplayConfig::new(to_niri, backend.ipc_outputs());
            niri.event_loop
                .insert_source(from_display_config, move |event, _, state| match event {
                    calloop::channel::Event::Msg(new_conf) => {
                        // 更新输出配置
                        for (name, conf) in new_conf {
                            state.modify_output_config(&name, move |output| {
                                if let Some(new_output) = conf {
                                    *output = new_output;  // 更新配置
                                } else {
                                    output.off = true;     // 关闭输出
                                }
                            });
                        }
                        state.reload_output_config();  // 重新加载配置
                    }
                    calloop::channel::Event::Closed => (),
                })
                .unwrap();
            dbus.conn_display_config = try_start(display_config);

            // 屏幕保护服务
            let screen_saver = ScreenSaver::new(niri.is_fdo_idle_inhibited.clone());
            dbus.conn_screen_saver = try_start(screen_saver);

            // 截图服务
            let (to_niri, from_screenshot) = calloop::channel::channel();
            // async_channel：异步通信通道
            let (to_screenshot, from_niri) = async_channel::unbounded();
            niri.event_loop
                .insert_source(from_screenshot, move |event, _, state| match event {
                    calloop::channel::Event::Msg(msg) => {
                        // 处理截图消息
                        state.on_screen_shot_msg(&to_screenshot, msg)
                    }
                    calloop::channel::Event::Closed => (),
                })
                .unwrap();
            let screenshot = gnome_shell_screenshot::Screenshot::new(to_niri, from_niri);
            dbus.conn_screen_shot = try_start(screenshot);

            // 自省服务
            let (to_niri, from_introspect) = calloop::channel::channel();
            let (to_introspect, from_niri) = async_channel::unbounded();
            niri.event_loop
                .insert_source(from_introspect, move |event, _, state| match event {
                    calloop::channel::Event::Msg(msg) => {
                        // 处理自省消息
                        state.on_introspect_msg(&to_introspect, msg)
                    }
                    calloop::channel::Event::Closed => (),
                })
                .unwrap();
            let introspect = Introspect::new(to_niri, from_niri);
            dbus.conn_introspect = try_start(introspect);
        }

        // 将DBus服务管理器存入全局状态
        niri.dbus = Some(dbus);
    }
}

// 辅助函数：尝试启动DBus服务
// 泛型函数：I必须实现Start trait
fn try_start<I: Start>(iface: I) -> Option<Connection> {
    // 模式匹配处理启动结果
    match iface.start() {
        Ok(conn) => Some(conn),  // 成功返回连接
        Err(err) => {
            // 记录警告日志
            warn!("error starting {}: {err:?}", I::name());
            None
        }
    }
}

/*
DBus服务启动流程图：
+-----------------------+
| 开始                  |
+----------+------------+
           |
           v
+----------+------------+
| 创建DBusServers实例   |
+----------+------------+
           |
           v
+----------+------------+  是
| 是否为主会话实例? +-------+---> 启动ServiceChannel
+----------+------------+   |
           | 否            |
           v               |
+----------+------------+   |
| 是否启用调试接口? +-------+---> 启动其他服务
+----------+------------+   |
           |               |
           v               |
+----------+------------+   |
| 启动显示配置服务      |<--+
| 启动屏幕保护服务      |
| 启动截图服务          |
| 启动自省服务          |
| (可选)启动录屏服务    |
+----------+------------+
           |
           v
+----------+------------+
| 存储到全局状态        |
+-----------------------+
*/