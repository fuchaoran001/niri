//! 事务处理模块：用于协调多个Wayland客户端的原子操作
//!
//! 在合成器中的作用：
//! 1. 确保一组相关操作（如窗口调整大小）在多个客户端间同步完成
//! 2. 防止中间状态暴露给用户
//! 3. 提供超时机制防止客户端无响应
//!
//! 核心设计：
//! - 事务状态通过Arc<Inner>跨线程共享
//! - 阻塞器(Blocker)机制延迟表面提交
//! - 超时定时器确保事务最终完成

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender; // 多生产者单消费者通道
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use atomic::Ordering; // 原子操作内存排序
use calloop::ping::{make_ping, Ping}; // 事件循环ping机制
use calloop::timer::{TimeoutAction, Timer}; // 定时器支持
use calloop::LoopHandle; // 事件循环句柄
use smithay::reexports::wayland_server::Client; // Wayland客户端
use smithay::wayland::compositor::{Blocker, BlockerState}; // 表面提交阻塞接口

/// 默认事务超时时间（毫秒）
///
/// 设计意图：
/// 防止客户端无响应导致事务永久挂起，
/// 平衡用户体验和系统响应性
const TIME_LIMIT: Duration = Duration::from_millis(300);

/// 客户端间事务协调器
///
/// 使用流程：
/// 1. 创建事务 `Transaction::new()`
/// 2. 克隆事务到需要同步的客户端
/// 3. 添加完成通知 `add_notification()`
/// 4. 注册超时定时器 `register_deadline_timer()`
/// 5. 为每个表面添加阻塞器 `blocker()`
#[derive(Debug, Clone)]
pub struct Transaction {
    /// 共享事务状态
    ///
    /// 关键数据结构设计：
    /// 使用Arc实现线程安全共享，AtomicBool保证完成状态原子访问，
    /// Mutex保护通知列表（发送频率低，适合互斥锁）
    inner: Arc<Inner>,
    
    /// 超时管理
    ///
    /// 设计意图：
    /// 使用Rc+RefCell实现单线程内可变借用，
    /// 与事件循环交互时无需跨线程同步
    deadline: Rc<RefCell<Deadline>>,
}

/// 表面提交阻塞器
///
/// 在合成器中的作用：
/// 关联到具体表面，延迟其提交直到事务完成
#[derive(Debug)]
pub struct TransactionBlocker(Weak<Inner>); // 弱引用避免循环引用

/// 超时状态机
#[derive(Debug)]
enum Deadline {
    /// 定时器未注册（包含截止时间）
    NotRegistered(Instant),
    
    /// 定时器已注册（包含移除触发器）
    Registered { remove: Ping },
}

/// 事务内部状态
#[derive(Debug)]
struct Inner {
    /// 事务完成标志（原子操作）
    completed: AtomicBool,
    
    /// 完成通知列表
    ///
    /// 数据结构设计：
    /// 使用Option包裹元组，在事务完成后释放内存
    /// - Sender: 通知发送通道
    /// - Vec<Client>: 需要通知的客户端列表
    notifications: Mutex<Option<(Sender<Client>, Vec<Client>)>>,
}

impl Transaction {
    /// 创建新事务
    ///
    /// # Rust原子类型说明
    /// AtomicBool提供无锁线程安全访问，
    /// 适合高频访问的完成状态标志
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner::new()),
            deadline: Rc::new(RefCell::new(Deadline::NotRegistered(
                Instant::now() + TIME_LIMIT, // 设置默认超时
            ))),
        }
    }

    /// 创建事务阻塞器
    ///
    /// 在合成器中的作用：
    /// 将阻塞器附加到表面，延迟其提交直到事务完成
    pub fn blocker(&self) -> TransactionBlocker {
        trace!(transaction = ?Arc::as_ptr(&self.inner), "生成阻塞器");
        TransactionBlocker(Arc::downgrade(&self.inner)) // 降级为弱引用
    }

    /// 添加事务完成通知
    ///
    /// 参数：
    /// - sender: 通知发送通道
    /// - client: 目标客户端
    ///
    /// 设计意图：
    /// 当事务完成时，通知所有相关客户端
    pub fn add_notification(&self, sender: Sender<Client>, client: Client) {
        // 检查事务状态
        if self.is_completed() {
            error!("尝试向已完成的事务添加通知");
            return;
        }

        // 锁保护通知列表
        let mut guard = self.inner.notifications.lock().unwrap();
        let entry = guard.get_or_insert((sender, Vec::new()));
        entry.1.push(client); // 添加客户端到通知列表
    }

    /// 注册超时定时器到事件循环
    ///
    /// 在合成器中的作用：
    /// 设置安全阀，确保事务不会永久阻塞
    pub fn register_deadline_timer<T: 'static>(&self, event_loop: &LoopHandle<'static, T>) {
        let mut cell = self.deadline.borrow_mut();
        // 仅处理未注册状态
        if let Deadline::NotRegistered(deadline) = *cell {
            // 创建定时器源
            let timer = Timer::from_deadline(deadline);
            let inner = Arc::downgrade(&self.inner); // 弱引用避免循环
            
            // 插入定时器到事件循环
            let token = event_loop
                .insert_source(timer, move |_, _, _| {
                    let _span = trace_span!("超时定时器触发", 事务 = ?Weak::as_ptr(&inner)).entered();

                    // 非测试环境处理超时
                    #[cfg(not(test))]
                    if let Some(inner) = inner.upgrade() {
                        trace!("超时到达，强制完成事务");
                        inner.complete(); // 强制完成事务
                    } else {
                        trace!("事务已提前完成");
                    }

                    TimeoutAction::Drop // 移除定时器
                })
                .unwrap();

            // 创建Ping源用于移除定时器
            let (ping, source) = make_ping().unwrap();
            let loop_handle = event_loop.clone();
            event_loop
                .insert_source(source, move |_, _, _| {
                    loop_handle.remove(token); // 移除定时器
                })
                .unwrap();

            // 更新为已注册状态
            *cell = Deadline::Registered { remove: ping };
        }
    }

    /// 检查事务是否已完成
    pub fn is_completed(&self) -> bool {
        self.inner.is_completed()
    }

    /// 检查当前实例是否是事务的最后一个引用
    ///
    /// 在合成器中的作用：
    /// 决定是否需要在drop时自动完成事务
    pub fn is_last(&self) -> bool {
        Arc::strong_count(&self.inner) == 1
    }
}

impl Drop for Transaction {
    /// 事务销毁处理
    ///
    /// 设计意图：
    /// 当最后一个事务引用被丢弃时，自动完成事务并清理资源
    fn drop(&mut self) {
        let _span = trace_span!("销毁事务", 事务 = ?Arc::as_ptr(&self.inner)).entered();

        if self.is_last() {
            // 最后一个引用：强制完成事务
            trace!("最后的事务引用被丢弃，完成事务");
            self.inner.complete();

            // 清理定时器资源
            if let Deadline::Registered { remove } = &*self.deadline.borrow() {
                remove.ping(); // 触发定时器移除
            };
        }
    }
}

impl TransactionBlocker {
    /// 创建已完成的阻塞器（空操作）
    pub fn completed() -> Self {
        Self(Weak::new())
    }
}

impl Blocker for TransactionBlocker {
    /// 实现阻塞器状态检查
    ///
    /// 在合成器中的作用：
    /// 合成器在表面提交前调用此方法，
    /// 返回Pending时延迟提交
    fn state(&self) -> BlockerState {
        // 检查事务是否完成
        if self.0.upgrade().map_or(true, |x| x.is_completed()) {
            BlockerState::Released // 允许提交
        } else {
            BlockerState::Pending // 延迟提交
        }
    }
}

impl Inner {
    /// 初始化事务内部状态
    fn new() -> Self {
        Self {
            completed: AtomicBool::new(false),
            notifications: Mutex::new(None),
        }
    }

    /// 检查事务完成状态
    ///
    /// # Rust原子操作说明
    /// Ordering::Relaxed保证原子性但不限制内存排序，
    /// 适用于状态标志检查
    fn is_completed(&self) -> bool {
        self.completed.load(Ordering::Relaxed)
    }

    /// 完成事务处理
    ///
    /// 处理步骤：
    /// 1. 设置完成标志
    /// 2. 发送所有通知
    /// 3. 清理通知列表
    fn complete(&self) {
        // 设置完成标志
        self.completed.store(true, Ordering::Relaxed);

        // 锁定通知列表
        let mut guard = self.notifications.lock().unwrap();
        if let Some((sender, clients)) = guard.take() {
            // 遍历并通知所有客户端
            for client in clients {
                if let Err(err) = sender.send(client) {
                    warn!("发送阻塞器通知错误: {err:?}");
                };
            }
        }
    }
}