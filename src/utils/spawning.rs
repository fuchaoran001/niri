//! 进程生成与系统资源管理模块
//!
//! 在合成器中的作用：
//! 1. 安全执行外部命令
//! 2. 管理文件描述符限制
//! 3. 支持XDG激活令牌
//! 4. 集成systemd进程管理（可选）

use std::ffi::OsStr;
use std::os::unix::process::CommandExt; // Unix命令扩展
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock; // 读写锁
use std::{io, thread};

use atomic::Atomic;
use libc::{getrlimit, rlim_t, rlimit, setrlimit, RLIMIT_NOFILE}; // 系统资源限制
use niri_config::Environment; // 环境变量配置
use smithay::wayland::xdg_activation::XdgActivationToken; // XDG激活令牌

use crate::utils::expand_home; // 主目录路径扩展

/// 控制是否移除RUST_BACKTRACE环境变量
///
/// 设计意图：
/// 防止子进程生成冗长的Rust回溯信息
pub static REMOVE_ENV_RUST_BACKTRACE: AtomicBool = AtomicBool::new(false);

/// 控制是否移除RUST_LIB_BACKTRACE环境变量
pub static REMOVE_ENV_RUST_LIB_BACKTRACE: AtomicBool = AtomicBool::new(false);

/// 子进程环境变量存储
///
/// 关键数据结构设计：
/// 使用RwLock实现高效并发读取，
/// 适用于配置热更新场景
pub static CHILD_ENV: RwLock<Environment> = RwLock::new(Environment(Vec::new()));

/// 原始文件描述符限制（当前值）
static ORIGINAL_NOFILE_RLIMIT_CUR: Atomic<rlim_t> = Atomic::new(0);

/// 原始文件描述符限制（最大值）
static ORIGINAL_NOFILE_RLIMIT_MAX: Atomic<rlim_t> = Atomic::new(0);

/// 存储并增加文件描述符限制
///
/// 在合成器中的作用：
/// 提高Wayland客户端能打开的文件描述符数量，
/// 防止资源耗尽导致的连接失败
pub fn store_and_increase_nofile_rlimit() {
    // 获取当前限制
    let mut rlim = rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { getrlimit(RLIMIT_NOFILE, &mut rlim) } != 0 {
        let err = io::Error::last_os_error();
        warn!("获取nofile资源限制错误: {err:?}");
        return;
    }

    // 存储原始值
    ORIGINAL_NOFILE_RLIMIT_CUR.store(rlim.rlim_cur, Ordering::SeqCst);
    ORIGINAL_NOFILE_RLIMIT_MAX.store(rlim.rlim_max, Ordering::SeqCst);

    trace!(
        "修改nofile资源限制: {} -> {}",
        rlim.rlim_cur,
        rlim.rlim_max
    );
    
    // 提升到最大值
    rlim.rlim_cur = rlim.rlim_max;

    // 应用新限制
    if unsafe { setrlimit(RLIMIT_NOFILE, &rlim) } != 0 {
        let err = io::Error::last_os_error();
        warn!("设置nofile资源限制错误: {err:?}");
    }
}

/// 恢复原始文件描述符限制
pub fn restore_nofile_rlimit() {
    // 获取存储值
    let rlim_cur = ORIGINAL_NOFILE_RLIMIT_CUR.load(Ordering::SeqCst);
    let rlim_max = ORIGINAL_NOFILE_RLIMIT_MAX.load(Ordering::SeqCst);

    if rlim_cur == 0 {
        return; // 未初始化
    }

    // 恢复限制
    let rlim = rlimit { rlim_cur, rlim_max };
    unsafe { setrlimit(RLIMIT_NOFILE, &rlim) };
}

/// 生成独立进程执行命令
///
/// 在合成器中的作用：
/// 启动Wayland客户端应用程序，
/// 支持焦点激活令牌传递
pub fn spawn<T: AsRef<OsStr> + Send + 'static>(
    command: Vec<T>,          // 命令及参数
    token: Option<XdgActivationToken>, // 焦点激活令牌
) {
    let _span = tracy_client::span!(); // 性能分析

    if command.is_empty() {
        return; // 空命令检查
    }

    // 后台线程执行（避免阻塞主线程）
    let res = thread::Builder::new()
        .name("命令生成器".to_owned())
        .spawn(move || {
            let (command, args) = command.split_first().unwrap();
            spawn_sync(command, args, token);
        });

    if let Err(err) = res {
        warn!("生成命令线程错误: {err:?}");
    }
}

/// 同步执行命令生成
fn spawn_sync(
    command: impl AsRef<OsStr>, // 命令路径
    args: impl IntoIterator<Item = impl AsRef<OsStr>>, // 命令参数
    token: Option<XdgActivationToken>, // 激活令牌
) {
    let _span = tracy_client::span!();

    let mut command_ref = command.as_ref();

    // 扩展主目录路径（~）
    let expanded = expand_home(Path::new(command_ref));
    match &expanded {
        Ok(Some(expanded)) => command_ref = expanded.as_ref(),
        Ok(None) => (),
        Err(err) => {
            warn!("主目录扩展错误: {err:?}");
        }
    }

    // 配置命令
    let mut process = Command::new(command_ref);
    process
        .args(args) // 添加参数
        .stdin(Stdio::null()) // 关闭标准输入
        .stdout(Stdio::null()) // 关闭标准输出
        .stderr(Stdio::null()); // 关闭标准错误

    // 按需移除RUST_BACKTRACE环境变量
    if REMOVE_ENV_RUST_BACKTRACE.load(Ordering::Relaxed) {
        process.env_remove("RUST_BACKTRACE");
    }
    if REMOVE_ENV_RUST_LIB_BACKTRACE.load(Ordering::Relaxed) {
        process.env_remove("RUST_LIB_BACKTRACE");
    }

    // 应用配置的环境变量
    {
        let env = CHILD_ENV.read().unwrap();
        for var in &env.0 {
            if let Some(value) = &var.value {
                process.env(&var.name, value); // 设置变量
            } else {
                process.env_remove(&var.name); // 移除变量
            }
        }
    }

    // 传递激活令牌
    if let Some(token) = token.as_ref() {
        process.env("XDG_ACTIVATION_TOKEN", token.as_str());
        process.env("DESKTOP_STARTUP_ID", token.as_str());
    }

    // 执行生成
    let Some(mut child) = do_spawn(command_ref, process) else {
        return;
    };

    // 等待子进程退出
    match child.wait() {
        Ok(status) => {
            if !status.success() {
                warn!("子进程异常退出: {status:?}");
            }
        }
        Err(err) => {
            warn!("等待子进程错误: {err:?}");
        }
    }
}

// 非systemd环境的生成实现
#[cfg(not(feature = "systemd"))]
fn do_spawn(command: &OsStr, mut process: Command) -> Option<Child> {
    // 双重fork技术：避免僵尸进程
    unsafe {
        process.pre_exec(move || {
            match libc::fork() {
                -1 => return Err(io::Error::last_os_error()), // fork失败
                0 => (), // 孙子进程
                _ => libc::_exit(0), // 中间进程立即退出
            }

            // 恢复文件描述符限制
            restore_nofile_rlimit();

            Ok(())
        });
    }

    // 执行命令
    match process.spawn() {
        Ok(child) => Some(child),
        Err(err) => {
            warn!("生成命令失败 {command:?}: {err:?}");
            None
        }
    }
}

// systemd集成模块（条件编译）
#[cfg(feature = "systemd")]
use systemd::do_spawn;

#[cfg(feature = "systemd")]
mod systemd {
    use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};

    use smithay::reexports::rustix;
    use smithay::reexports::rustix::io::{close, read, retry_on_intr, write};
    use smithay::reexports::rustix::pipe::{pipe_with, PipeFlags};

    use super::*;

    /// systemd环境下的进程生成
    ///
    /// 特殊处理：
    /// 1. 创建进程间通信管道
    /// 2. 使用双重fork
    /// 3. 创建systemd临时作用域
    pub fn do_spawn(command: &OsStr, mut process: Command) -> Option<Child> {
        use libc::close_range; // 文件描述符范围关闭

        // 创建PID传输管道
        let (pipe_pid_read, pipe_pid_write) = pipe_with(PipeFlags::CLOEXEC)
            .map_err(|err| {
                warn!("创建PID传输管道错误: {err:?}");
            })
            .ok()
            .unzip();
        // 创建等待管道
        let (pipe_wait_read, pipe_wait_write) = pipe_with(PipeFlags::CLOEXEC)
            .map_err(|err| {
                warn!("创建等待管道错误: {err:?}");
            })
            .ok()
            .unzip();

        unsafe {
            // 获取原始文件描述符
            let mut pipe_pid_read_fd = pipe_pid_read.as_ref().map(|fd| fd.as_raw_fd());
            let mut pipe_pid_write_fd = pipe_pid_write.as_ref().map(|fd| fd.as_raw_fd());
            let mut pipe_wait_read_fd = pipe_wait_read.as_ref().map(|fd| fd.as_raw_fd());
            let mut pipe_wait_write_fd = pipe_wait_write.as_ref().map(|fd| fd.as_raw_fd());

            // 双重fork
            process.pre_exec(move || {
                // 清理不需要的文件描述符
                if let Some(fd) = pipe_pid_read_fd.take() {
                    close(fd);
                }
                if let Some(fd) = pipe_wait_write_fd.take() {
                    close(fd);
                }

                // 转换管道为自有描述符（自动关闭）
                let pipe_pid_write = pipe_pid_write_fd.take().map(|fd| OwnedFd::from_raw_fd(fd));
                let pipe_wait_read = pipe_wait_read_fd.take().map(|fd| OwnedFd::from_raw_fd(fd));

                match libc::fork() {
                    -1 => return Err(io::Error::last_os_error()),
                    0 => (), // 孙子进程继续
                    grandchild_pid => {
                        // 发送孙子进程PID
                        if let Some(pipe) = pipe_pid_write {
                            let _ = write_all(pipe, &grandchild_pid.to_ne_bytes());
                        }

                        // 等待父进程信号
                        if let Some(pipe) = pipe_wait_read {
                            // 关闭所有其他文件描述符
                            let raw = pipe.as_raw_fd() as u32;
                            let _ = close_range(0, raw - 1, 0);
                            let _ = close_range(raw + 1, !0, 0);

                            // 阻塞读取
                            let _ = read_all(pipe, &mut [0]);
                        }

                        // 中间进程退出
                        libc::_exit(0)
                    }
                }

                // 恢复文件描述符限制
                restore_nofile_rlimit();

                Ok(())
            });
        }

        // 生成子进程
        let child = match process.spawn() {
            Ok(child) => child,
            Err(err) => {
                warn!("生成命令失败 {command:?}: {err:?}");
                return None;
            }
        };

        // 清理写端管道
        drop(pipe_pid_write);
        drop(pipe_wait_read);

        // 接收孙子进程PID
        if let Some(pipe) = pipe_pid_read {
            let mut buf = [0; 4]; // PID缓冲区
            match read_all(pipe, &mut buf) {
                Ok(()) => {
                    let pid = i32::from_ne_bytes(buf);
                    trace!("生成的孙子进程PID: {pid}");

                    // 创建systemd临时作用域
                    if let Err(err) = start_systemd_scope(command, child.id(), pid as u32) {
                        trace!("创建systemd作用域错误: {err:?}");
                    }
                }
                Err(err) => {
                    warn!("读取PID错误: {err:?}");
                }
            }
        }

        // 通知中间进程退出
        trace!("通知中间进程退出");
        drop(pipe_wait_write);

        Some(child)
    }

    /// 完整写入数据到文件描述符
    fn write_all(fd: impl AsFd, buf: &[u8]) -> rustix::io::Result<()> {
        let mut written = 0;
        loop {
            // 带中断重试的写入
            let n = retry_on_intr(|| write(&fd, &buf[written..]))?;
            if n == 0 {
                return Err(rustix::io::Errno::CANCELED); // 意外结束
            }

            written += n;
            if written == buf.len() {
                return Ok(()); // 写入完成
            }
        }
    }

    /// 完整读取数据从文件描述符
    fn read_all(fd: impl AsFd, buf: &mut [u8]) -> rustix::io::Result<()> {
        let mut start = 0;
        loop {
            // 带中断重试的读取
            let n = retry_on_intr(|| read(&fd, &mut buf[start..]))?;
            if n == 0 {
                return Err(rustix::io::Errno::CANCELED); // 意外结束
            }

            start += n;
            if start == buf.len() {
                return Ok(()); // 读取完成
            }
        }
    }

    /// 创建systemd临时作用域
    ///
    /// 在合成器中的作用：
    /// 1. 隔离客户端进程
    /// 2. 防止OOM killer影响合成器
    /// 3. 改善资源管理
    fn start_systemd_scope(
        name: &OsStr,          // 进程名称
        intermediate_pid: u32, // 中间进程PID
        child_pid: u32,        // 孙子进程PID
    ) -> anyhow::Result<()> {
        use std::fmt::Write as _;
        use std::os::unix::ffi::OsStrExt;
        use std::sync::OnceLock;

        use anyhow::Context;
        use zbus::zvariant::{OwnedObjectPath, Value};

        use crate::utils::IS_SYSTEMD_SERVICE; // systemd服务标志

        // 检查是否在systemd服务中运行
        if !IS_SYSTEMD_SERVICE.load(Ordering::Relaxed) {
            return Ok(());
        }

        let _span = tracy_client::span!();

        // 提取基础名称
        let name = Path::new(name).file_name().unwrap_or(name);

        // 构建作用域名称
        let mut scope_name = String::from("app-niri-");

        // 名称转义（兼容systemd）
        for &c in name.as_bytes() {
            if c.is_ascii_alphanumeric() || matches!(c, b':' | b'_' | b'.') {
                scope_name.push(char::from(c));
            } else {
                let _ = write!(scope_name, "\\x{c:02x}"); // 十六进制转义
            }
        }

        let _ = write!(scope_name, "-{child_pid}.scope"); // 添加PID后缀

        // 连接systemd D-Bus
        static CONNECTION: OnceLock<zbus::Result<zbus::blocking::Connection>> = OnceLock::new();
        let conn = CONNECTION
            .get_or_init(zbus::blocking::Connection::session)
            .clone()
            .context("连接会话总线错误")?;

        // 创建D-Bus代理
        let proxy = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
        )
        .context("创建代理错误")?;

        // 监听任务完成信号
        let signals = proxy
            .receive_signal("JobRemoved")
            .context("创建信号迭代器错误")?;

        // 设置作用域属性
        let pids: &[_] = &[intermediate_pid, child_pid];
        let properties: &[_] = &[
            ("PIDs", Value::new(pids)), // 进程ID列表
            ("CollectMode", Value::new("inactive-or-failed")), // 收集模式
        ];
        let aux: &[(&str, &[(&str, Value)])] = &[]; // 辅助属性

        // 创建临时作用域
        let job: OwnedObjectPath = proxy
            .call("StartTransientUnit", &(scope_name, "fail", properties, aux))
            .context("调用StartTransientUnit错误")?;

        // 等待作用域创建完成
        trace!("等待JobRemoved信号");
        for message in signals {
            let body = message.body();
            let body: (u32, OwnedObjectPath, &str, &str) =
                body.deserialize().context("解析信号错误")?;

            if body.1 == job {
                break; // 我们的任务完成
            }
        }

        Ok(())
    }
}