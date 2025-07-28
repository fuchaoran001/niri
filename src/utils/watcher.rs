//! 文件修改监视器模块
//!
//! 实现原理：
//! 通过后台线程定期检查文件的规范化路径和修改时间，
//! 检测到变化时发送通知
//!
//! 设计特点：
//! 1. 处理符号链接和NixOS等特殊文件系统
//! 2. 避免inotify等机制的复杂性和平台差异
//! 3. 500ms轮询间隔平衡响应速度和资源消耗

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use smithay::reexports::calloop::channel::SyncSender; // 同步通道发送端

/// 文件监视器句柄
///
/// 在合成器中的作用：
/// 监控配置文件变化，实现配置热重载
pub struct Watcher {
    /// 停止信号标志
    ///
    /// 关键数据结构设计：
    /// 使用Arc+AtomicBool实现线程安全的状态共享，
    /// 避免使用锁带来的复杂性和潜在死锁
    should_stop: Arc<AtomicBool>,
}

impl Drop for Watcher {
    /// 析构时发送停止信号
    fn drop(&mut self) {
        self.should_stop.store(true, Ordering::SeqCst);
    }
}

impl Watcher {
    /// 创建新监视器（无启动通知）
    pub fn new<T: Send + 'static>(
        path: PathBuf,               // 监控路径
        process: impl FnMut(&Path) -> T + Send + 'static, // 处理函数
        changed: SyncSender<T>,      // 变化通知通道
    ) -> Self {
        Self::with_start_notification(path, process, changed, None)
    }

    /// 创建带启动通知的监视器
    ///
    /// 参数：
    /// - path: 监控路径
    /// - process: 变化处理函数
    /// - changed: 变化通知通道
    /// - started: 线程启动通知通道（可选）
    ///
    /// 在合成器中的作用：
    /// 启动通知确保监视线程就绪后再继续初始化，
    /// 避免竞态条件
    pub fn with_start_notification<T: Send + 'static>(
        path: PathBuf,
        mut process: impl FnMut(&Path) -> T + Send + 'static,
        changed: SyncSender<T>,
        started: Option<mpsc::SyncSender<()>>, // 启动完成信号
    ) -> Self {
        let should_stop = Arc::new(AtomicBool::new(false));

        {
            let should_stop = should_stop.clone();
            // 创建后台监视线程
            thread::Builder::new()
                .name(format!("文件系统监视器: {}", path.to_string_lossy()))
                .spawn(move || {
                    // 文件属性追踪状态：
                    //   Some((修改时间, 规范化路径))
                    //   None 表示文件不存在
                    let mut last_props = path
                        .canonicalize() // 解析符号链接
                        .and_then(|canon| {
                            // 获取元数据和修改时间
                            let meta = canon.metadata()?;
                            let modified = meta.modified()?;
                            Ok((modified, canon))
                        })
                        .ok(); // 出错时设为None

                    // 发送启动完成信号
                    if let Some(started) = started {
                        let _ = started.send(());
                    }

                    // 监视循环
                    loop {
                        // 休眠500ms（降低CPU占用）
                        thread::sleep(Duration::from_millis(500));

                        // 检查停止信号
                        if should_stop.load(Ordering::SeqCst) {
                            break;
                        }

                        // 获取当前文件属性
                        if let Ok(new_props) = path
                            .canonicalize()
                            .and_then(|canon| {
                                let meta = canon.metadata()?;
                                let modified = meta.modified()?;
                                Ok((modified, canon))
                            })
                        {
                            // 检测变化：规范化路径或修改时间改变
                            if last_props.as_ref() != Some(&new_props) {
                                trace!("文件变化: {}", path.to_string_lossy());

                                // 调用处理函数
                                let rv = process(&path);

                                // 发送变化通知
                                if let Err(err) = changed.send(rv) {
                                    warn!("发送变化通知错误: {err:?}");
                                    break;
                                }

                                // 更新最后已知状态
                                last_props = Some(new_props);
                            }
                        }
                    }

                    debug!("退出监视线程: {}", path.to_string_lossy());
                })
                .unwrap();
        }

        Self { should_stop }
    }
}

// 单元测试模块
#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fs::File;
    use std::io::Write;
    use std::sync::atomic::AtomicU8;

    use calloop::channel::sync_channel; // 同步通道
    use calloop::EventLoop; // 事件循环
    use smithay::reexports::rustix::fs::{futimens, Timestamps}; // 文件时间设置
    use smithay::reexports::rustix::time::Timespec; // 时间戳
    use xshell::{cmd, Shell}; // 跨平台shell命令

    use super::*;

    /// 测试辅助函数
    ///
    /// 执行流程：
    /// 1. 创建临时目录
    /// 2. 初始设置（setup）
    /// 3. 启动监视器
    /// 4. 执行变更（change）
    /// 5. 验证变更通知
    /// 6. 验证持续监视能力
    fn check(
        setup: impl FnOnce(&Shell) -> Result<(), Box<dyn Error>>, // 初始设置回调
        change: impl FnOnce(&Shell) -> Result<(), Box<dyn Error>>, // 变更回调
    ) {
        // 创建临时shell环境
        let sh = Shell::new().unwrap();
        let temp_dir = sh.create_temp_dir().unwrap();
        sh.change_dir(temp_dir.path());

        // 配置文件路径
        let mut config_path = sh.current_dir();
        config_path.push("niri");
        config_path.push("config.kdl");

        // 执行初始设置
        setup(&sh).unwrap();

        // 变更计数器
        let changed = AtomicU8::new(0);

        // 创建事件循环
        let mut event_loop = EventLoop::try_new().unwrap();
        let loop_handle = event_loop.handle();

        // 创建监视通道
        let (tx, rx) = sync_channel(1);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let _watcher = Watcher::with_start_notification(
            config_path.clone(),
            |_| (), // 空处理函数
            tx,
            Some(started_tx),
        );
        
        // 插入通道到事件循环
        loop_handle
            .insert_source(rx, |_, _, _| {
                changed.fetch_add(1, Ordering::SeqCst); // 计数变更
            })
            .unwrap();
        
        // 等待监视线程启动
        started_rx.recv().unwrap();

        // 避免相同修改时间（文件系统时间精度限制）
        thread::sleep(Duration::from_millis(100));

        // 执行变更操作
        change(&sh).unwrap();

        // 处理事件（等待通知）
        event_loop
            .dispatch(Duration::from_millis(750), &mut ())
            .unwrap();

        // 验证变更计数
        assert_eq!(changed.load(Ordering::SeqCst), 1);

        // 二次变更验证持续监视
        sh.write_file(&config_path, "c").unwrap();

        event_loop
            .dispatch(Duration::from_millis(750), &mut ())
            .unwrap();

        assert_eq!(changed.load(Ordering::SeqCst), 2);
    }

    // 测试文件内容变更
    #[test]
    fn change_file() {
        check(
            |sh| {
                sh.write_file("niri/config.kdl", "a")?;
                Ok(())
            },
            |sh| {
                sh.write_file("niri/config.kdl", "b")?;
                Ok(())
            },
        );
    }

    // 测试文件创建
    #[test]
    fn create_file() {
        check(
            |sh| {
                sh.create_dir("niri")?; // 只创建目录
                Ok(())
            },
            |sh| {
                sh.write_file("niri/config.kdl", "a")?; // 创建文件
                Ok(())
            },
        );
    }

    // 测试目录和文件创建
    #[test]
    fn create_dir_and_file() {
        check(
            |_sh| Ok(()), // 空初始状态
            |sh| {
                sh.write_file("niri/config.kdl", "a")?; // 创建目录和文件
                Ok(())
            },
        );
    }

    // 测试符号链接目标变更
    #[test]
    fn change_linked_file() {
        check(
            |sh| {
                sh.write_file("niri/config2.kdl", "a")?;
                cmd!(sh, "ln -s config2.kdl niri/config.kdl").run()?; // 创建符号链接
                Ok(())
            },
            |sh| {
                sh.write_file("niri/config2.kdl", "b")?; // 修改目标文件
                Ok(())
            },
        );
    }

    // 测试符号链接目录内文件变更
    #[test]
    fn change_file_in_linked_dir() {
        check(
            |sh| {
                sh.write_file("niri2/config.kdl", "a")?;
                cmd!(sh, "ln -s niri2 niri").run()?; // 目录符号链接
                Ok(())
            },
            |sh| {
                sh.write_file("niri2/config.kdl", "b")?; // 修改目标目录内文件
                Ok(())
            },
        );
    }

    // 测试文件删除重建
    #[test]
    fn recreate_file() {
        check(
            |sh| {
                sh.write_file("niri/config.kdl", "a")?;
                Ok(())
            },
            |sh| {
                sh.remove_path("niri/config.kdl")?; // 删除文件
                sh.write_file("niri/config.kdl", "b")?; // 重建文件
                Ok(())
            },
        );
    }

    // 测试目录删除重建
    #[test]
    fn recreate_dir() {
        check(
            |sh| {
                sh.write_file("niri/config.kdl", "a")?;
                Ok(())
            },
            |sh| {
                sh.remove_path("niri")?; // 删除目录
                sh.write_file("niri/config.kdl", "b")?; // 重建目录和文件
                Ok(())
            },
        );
    }

    // 测试目录替换
    #[test]
    fn swap_dir() {
        check(
            |sh| {
                sh.write_file("niri/config.kdl", "a")?;
                Ok(())
            },
            |sh| {
                sh.write_file("niri2/config.kdl", "b")?;
                sh.remove_path("niri")?; // 删除旧目录
                cmd!(sh, "mv niri2 niri").run()?; // 移动新目录
                Ok(())
            },
        );
    }

    // 测试NixOS风格符号链接切换（相同修改时间）
    #[test]
    fn swap_just_link() {
        check(
            |sh| {
                let mut dir = sh.current_dir();
                dir.push("niri");
                sh.create_dir(&dir)?;

                // 创建文件1（固定修改时间为1970年）
                let mut d2 = dir.clone();
                d2.push("config2.kdl");
                let mut c2 = File::create(d2).unwrap();
                write!(c2, "a")?;
                c2.flush()?;
                futimens(
                    &c2,
                    &Timestamps {
                        last_access: Timespec { tv_sec: 0, tv_nsec: 0 },
                        last_modification: Timespec { tv_sec: 0, tv_nsec: 0 },
                    },
                )?;
                c2.sync_all()?;
                drop(c2);

                // 创建文件2（相同修改时间）
                let mut d3 = dir.clone();
                d3.push("config3.kdl");
                let mut c3 = File::create(d3).unwrap();
                write!(c3, "b")?;
                c3.flush()?;
                futimens(
                    &c3,
                    &Timestamps {
                        last_access: Timespec { tv_sec: 0, tv_nsec: 0 },
                        last_modification: Timespec { tv_sec: 0, tv_nsec: 0 },
                    },
                )?;
                c3.sync_all()?;
                drop(c3);

                // 初始符号链接指向文件1
                cmd!(sh, "ln -s config2.kdl niri/config.kdl").run()?;
                Ok(())
            },
            |sh| {
                // 切换符号链接到文件2
                cmd!(sh, "unlink niri/config.kdl").run()?;
                cmd!(sh, "ln -s config3.kdl niri/config.kdl").run()?;
                Ok(())
            },
        );
    }

    // 测试目录符号链接切换
    #[test]
    fn swap_dir_link() {
        check(
            |sh| {
                sh.write_file("niri2/config.kdl", "a")?;
                cmd!(sh, "ln -s niri2 niri").run()?; // 初始符号链接
                Ok(())
            },
            |sh| {
                sh.write_file("niri3/config.kdl", "b")?;
                cmd!(sh, "unlink niri").run()?; // 删除旧链接
                cmd!(sh, "ln -s niri3 niri").run()?; // 创建新链接
                Ok(())
            },
        );
    }
}