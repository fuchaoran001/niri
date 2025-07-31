#[macro_use]
// 启用tracing宏，允许在代码中使用如info!、warn!等日志宏
extern crate tracing;

use std::fmt::Write as _;
// 文件系统操作相关模块
use std::fs::{self, File};
// 输入输出操作相关模块
use std::io::{self, Write};
// 用于从原始文件描述符创建文件
use std::os::fd::FromRawFd;
// 路径操作相关模块
use std::path::{Path, PathBuf};
// 子进程管理
use std::process::Command;
// 环境变量和内存操作
use std::{env, mem};

// 命令行参数解析库
use clap::{CommandFactory, Parser};
// 获取项目目录路径
use directories::ProjectDirs;
// 引入命令行接口定义
use niri::cli::{Cli, Sub};
// IPC客户端消息处理
use niri::ipc::client::handle_msg;
// niri主状态机
use niri::niri::State;
// 子进程生成与环境管理工具
use niri::utils::spawning::{
    spawn, store_and_increase_nofile_rlimit, CHILD_ENV, REMOVE_ENV_RUST_BACKTRACE,
    REMOVE_ENV_RUST_LIB_BACKTRACE,
};
// 配置文件监视器
use niri::utils::watcher::Watcher;
// 工具函数（版本信息、panic触发等）
use niri::utils::{cause_panic, version, IS_SYSTEMD_SERVICE};
// 配置加载模块
use niri_config::Config;
// IPC套接字路径环境变量名
use niri_ipc::socket::SOCKET_PATH_ENV;
// 原子操作
use portable_atomic::Ordering;
// systemd通知接口
use sd_notify::NotifyState;
// 事件循环和Wayland服务器
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
// 日志过滤
use tracing_subscriber::EnvFilter;

// 默认日志过滤规则：niri=debug级别，smithay渲染器错误级别
const DEFAULT_LOG_FILTER: &str = "niri=debug,smithay::backend::renderer::gles=error";

// 条件编译：当启用tracy内存分析时设置全局分配器
#[cfg(feature = "profile-with-tracy-allocations")]
#[global_allocator]
static GLOBAL: tracy_client::ProfiledAllocator<std::alloc::System> =
    tracy_client::ProfiledAllocator::new(std::alloc::System, 100);

// 主函数
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 设置RUST_BACKTRACE环境变量（如果未设置）
    // Rust概念：原子操作（Ordering::Relaxed）用于无锁同步
    if env::var_os("RUST_BACKTRACE").is_none() {
        env::set_var("RUST_BACKTRACE", "1");
        REMOVE_ENV_RUST_BACKTRACE.store(true, Ordering::Relaxed);
    }
    // 设置RUST_LIB_BACKTRACE环境变量（如果未设置）
    if env::var_os("RUST_LIB_BACKTRACE").is_none() {
        env::set_var("RUST_LIB_BACKTRACE", "0");
        REMOVE_ENV_RUST_LIB_BACKTRACE.store(true, Ordering::Relaxed);
    }

    // 配置日志过滤器
    // 从环境变量RUST_LOG获取，否则使用默认值
    let directives = env::var("RUST_LOG").unwrap_or_else(|_| DEFAULT_LOG_FILTER.to_owned());
    let env_filter = EnvFilter::builder().parse_lossy(directives);
    // 初始化日志系统：紧凑格式、输出到stderr、应用过滤器
    tracing_subscriber::fmt()
        .compact()
        .with_writer(io::stderr)
        .with_env_filter(env_filter)
        .init();

    // 检查是否作为systemd服务运行
    // 合成器作用：适配系统服务管理
    if env::var_os("NOTIFY_SOCKET").is_some() {
        IS_SYSTEMD_SERVICE.store(true, Ordering::Relaxed);

        // 条件编译：当未启用systemd特性时警告
        #[cfg(not(feature = "systemd"))]
        warn!(
            "running as a systemd service, but systemd support is compiled out. \
             Are you sure you did not forget to set `--features systemd`?"
        );
    }

    // 解析命令行参数
    let cli = Cli::parse();

    // 如果以会话模式启动
    if cli.session {
        // 移除可能影响窗口选择的DISPLAY环境变量
        if env::var_os("DISPLAY").is_some() {
            warn!("running as a session but DISPLAY is set, removing it");
            env::remove_var("DISPLAY");
        }
        // 移除可能影响窗口选择的WAYLAND_DISPLAY环境变量
        if env::var_os("WAYLAND_DISPLAY").is_some() {
            warn!("running as a session but WAYLAND_DISPLAY is set, removing it");
            env::remove_var("WAYLAND_DISPLAY");
        }
        // 移除可能影响窗口选择的WAYLAND_SOCKET环境变量
        if env::var_os("WAYLAND_SOCKET").is_some() {
            warn!("running as a session but WAYLAND_SOCKET is set, removing it");
            env::remove_var("WAYLAND_SOCKET");
        }

        // 设置XDG_CURRENT_DESKTOP环境变量（用于桌面门户）
        env::set_var("XDG_CURRENT_DESKTOP", "niri");
        // 设置XDG_SESSION_TYPE环境变量（用于自动启动和Qt应用）
        env::set_var("XDG_SESSION_TYPE", "wayland");
    }

    // 处理子命令
    if let Some(subcommand) = cli.subcommand {
        // Rust概念：模式匹配（match）用于处理枚举变体
        match subcommand {
            // 配置验证子命令
            Sub::Validate { config } => {
                // 启动性能分析器
                tracy_client::Client::start();

                // 获取配置路径
                let (path, _, _) = config_path(config);
                // 加载并验证配置
                Config::load(&path)?;
                info!("config is valid");
                return Ok(());
            }
            // IPC消息处理子命令
            Sub::Msg { msg, json } => {
                // 处理消息并返回
                handle_msg(msg, json)?;
                return Ok(());
            }
            // 触发panic子命令（用于测试）
            Sub::Panic => cause_panic(),
            // 生成自动补全脚本
            Sub::Completions { shell } => {
                // 生成指定shell的补全脚本
                clap_complete::generate(shell, &mut Cli::command(), "niri", &mut io::stdout());
                return Ok(());
            }
        }
    }

    // 启动性能分析器（Tracy）
    tracy_client::Client::start();

    // 打印启动日志（含版本号）
    info!("starting version {}", &version());

    // 获取配置路径、监视路径和是否创建默认配置标志
    let (path, watch_path, create_default) = config_path(cli.config);
    // 清除环境变量避免影响子进程
    env::remove_var("NIRI_CONFIG");
    // 如果需要创建默认配置
    if create_default {
        let default_parent = path.parent().unwrap();

        // 创建配置目录
        match fs::create_dir_all(default_parent) {
            Ok(()) => {
                // 尝试创建新配置文件
                let new_file = File::options()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&path);
                match new_file {
                    Ok(mut new_file) => {
                        // 内置默认配置
                        let default = include_bytes!("../resources/default-config.kdl");
                        // 写入默认配置
                        match new_file.write_all(default) {
                            Ok(()) => info!("wrote default config to {:?}", &path),
                            Err(err) => {
                                warn!("error writing config file at {:?}: {err:?}", &path)
                            }
                        }
                    }
                    // 文件已存在时忽略
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(err) => warn!("error creating config file at {:?}: {err:?}", &path),
                }
            }
            Err(err) => {
                warn!(
                    "error creating config directories {:?}: {err:?}",
                    default_parent
                );
            }
        }
    }

    // 加载配置文件
    let config_load_result = Config::load(&path);
    // 处理配置加载结果：出错时使用默认配置并记录警告
    let mut config = config_load_result
        .map_err(|err| warn!("{err:?}"))
        .unwrap_or_default();

    // 提取启动时需要执行的命令
    let spawn_at_startup = mem::take(&mut config.spawn_at_startup);
    // 存储环境变量配置（用于子进程）
    *CHILD_ENV.write().unwrap() = mem::take(&mut config.environment);

    // 增加文件描述符限制
    store_and_increase_nofile_rlimit();

    // 创建事件循环
    let mut event_loop = EventLoop::try_new().unwrap();
    // 创建Wayland显示
    let display = Display::new().unwrap();
    // 初始化合成器状态机
    // 关键数据结构：State管理合成器所有状态（窗口、输入、渲染等）
    let mut state = State::new(
        config,
        event_loop.handle(),
        event_loop.get_signal(),
        display,
        false,
        true,
    )
    .unwrap();

    // 设置WAYLAND_DISPLAY环境变量（供客户端连接）
    let socket_name = state.niri.socket_name.as_deref().unwrap();
    env::set_var("WAYLAND_DISPLAY", socket_name);
    info!(
        "listening on Wayland socket: {}",
        socket_name.to_string_lossy()
    );

    // 设置IPC套接字环境变量
    if let Some(ipc) = &state.niri.ipc_server {
        let socket_path = ipc.socket_path.as_deref().unwrap();
        env::set_var(SOCKET_PATH_ENV, socket_path);
        info!("IPC listening on: {}", socket_path.to_string_lossy());
    }

    // 会话模式特殊处理
    if cli.session {
        // 导入环境变量到会话管理器
        import_environment();

    }

    // 系统通知处理
    if env::var_os("NIRI_DISABLE_SYSTEM_MANAGER_NOTIFY").map_or(true, |x| x != "1") {
        // 通知systemd服务已就绪
        if let Err(err) = sd_notify::notify(true, &[NotifyState::Ready]) {
            warn!("error notifying systemd: {err:?}");
        };

        // 通过文件描述符发送就绪通知
        if let Err(err) = notify_fd() {
            warn!("error notifying fd: {err:?}");
        }
    }

    // 配置文件监视器初始化
    let _watcher = {
        // 配置文件加载处理闭包
        let process = |path: &Path| {
            Config::load(path).map_err(|err| {
                warn!("{:?}", err.context("error loading config"));
            })
        };

        // 创建通道用于监视事件
        let (tx, rx) = calloop::channel::sync_channel(1);
        // 初始化文件监视器
        let watcher = Watcher::new(watch_path.clone(), process, tx);
        // 将通道加入事件循环
        event_loop
            .handle()
            .insert_source(rx, |event, _, state| match event {
                // 收到新配置时重载
                calloop::channel::Event::Msg(config) => state.reload_config(config),
                calloop::channel::Event::Closed => (),
            })
            .unwrap();
        watcher
    };

    // 启动命令行指定的程序
    spawn(cli.command, None);

    // 启动配置中指定的自启动程序
    for elem in spawn_at_startup {
        spawn(elem.command, None);
    }
    
    // 使用 spawn 函数启动 Alacritty 终端
    spawn(vec!["alacritty".to_string()], None);

    // 主事件循环
    // 流程图：
    //   while 事件循环运行中:
    //     1. 处理所有待处理事件（输入、窗口事件等）
    //     2. 调用state.refresh_and_flush_clients()更新状态并刷新客户端
    //     3. 阻塞等待新事件
    event_loop
        .run(None, &mut state, |state| state.refresh_and_flush_clients())
        .unwrap();

    Ok(())
}

// 导入环境变量到会话系统
fn import_environment() {
    // 需要导入的环境变量列表
    let variables = [
        "WAYLAND_DISPLAY",
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_TYPE",
        SOCKET_PATH_ENV,
    ]
    .join(" ");

    // 构建初始化系统导入命令
    let mut init_system_import = String::new();
    // 条件编译：systemd支持
    if cfg!(feature = "systemd") {
        write!(
            init_system_import,
            "systemctl --user import-environment {variables};"
        )
        .unwrap();
    }
    // 条件编译：dinit支持
    if cfg!(feature = "dinit") {
        write!(init_system_import, "dinitctl setenv {variables};").unwrap();
    }

    // 执行环境导入命令
    let rv = Command::new("/bin/sh")
        .args([
            "-c",
            &format!(
                "{init_system_import}\
                 hash dbus-update-activation-environment 2>/dev/null && \
                 dbus-update-activation-environment {variables}"
            ),
        ])
        .spawn();
    // 等待命令完成
    match rv {
        Ok(mut child) => match child.wait() {
            Ok(status) => {
                if !status.success() {
                    warn!("import environment shell exited with {status}");
                }
            }
            Err(err) => {
                warn!("error waiting for import environment shell: {err:?}");
            }
        },
        Err(err) => {
            warn!("error spawning shell to import environment: {err:?}");
        }
    }
}

// 从环境变量获取配置路径
fn env_config_path() -> Option<PathBuf> {
    env::var_os("NIRI_CONFIG")
        .filter(|x| !x.is_empty())
        .map(PathBuf::from)
}

// 获取默认配置路径（用户配置目录）
fn default_config_path() -> Option<PathBuf> {
    let Some(dirs) = ProjectDirs::from("", "", "niri") else {
        warn!("error retrieving home directory");
        return None;
    };

    let mut path = dirs.config_dir().to_owned();
    path.push("config.kdl");
    Some(path)
}

// 获取系统级配置路径
fn system_config_path() -> PathBuf {
    PathBuf::from("/etc/niri/config.kdl")
}

/// 解析配置路径
/// 返回：(实际加载路径, 监视路径, 是否创建默认配置)
/// 决策逻辑：
///   1. 如果命令行或环境变量指定路径 -> 使用指定路径
///   2. 否则检查 ~/.config/niri/config.kdl 是否存在
///   3. 若不存在则检查 /etc/niri/config.kdl
///   4. 都无则使用用户配置路径并创建默认配置
fn config_path(cli_path: Option<PathBuf>) -> (PathBuf, PathBuf, bool) {
    // 显式指定的路径优先
    if let Some(explicit) = cli_path.or_else(env_config_path) {
        return (explicit.clone(), explicit, false);
    }

    let system_path = system_config_path();
    if let Some(path) = default_config_path() {
        // 用户配置文件存在
        if path.exists() {
            return (path.clone(), path, false);
        }

        // 系统配置文件存在
        if system_path.exists() {
            (system_path, path, false)
        } else {
            // 无配置文件，使用用户路径并创建默认
            (path.clone(), path, true)
        }
    } else {
        // 无用户目录时使用系统路径
        (system_path.clone(), system_path, false)
    }
}

// 通过文件描述符发送就绪通知
fn notify_fd() -> anyhow::Result<()> {
    // 获取NOTIFY_FD环境变量
    let fd = match env::var("NOTIFY_FD") {
        Ok(notify_fd) => notify_fd.parse()?,
        Err(env::VarError::NotPresent) => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    // 清除环境变量
    env::remove_var("NOTIFY_FD");
    // 从文件描述符创建File对象
    let mut notif = unsafe { File::from_raw_fd(fd) };
    // 写入就绪通知
    notif.write_all(b"READY=1\n")?;
    Ok(())
}