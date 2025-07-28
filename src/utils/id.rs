// 文件: utils/id.rs
// 作用: 提供原子ID生成器，用于合成器中分配全局唯一标识符
// 应用场景:
//   - 为Wayland对象分配唯一ID (wl_surface, wl_output等)
//   - 跟踪客户端请求序列号
//   - 管理内部资源标识

use std::sync::atomic::{AtomicU64, Ordering};  
// Rust并发: 
//   AtomicU64 - 线程安全的64位整数类型
//   Ordering - 内存顺序保证，控制原子操作的内存可见性

/// 计数器，返回唯一ID。
// 中文翻译: 该结构提供原子自增计数器，用于生成全局唯一ID
pub struct IdCounter {
    // 原子计数器值
    value: AtomicU64,  
    // 解释: 使用原子操作保证多线程环境下安全生成ID
}

impl IdCounter {
    // 构造函数: 创建新的ID计数器
    pub const fn new() -> Self {
        Self {
            // 从1开始计数，避免某些系统将0视为无效ID
            // 中文翻译: 从1开始以减少其他使用这些ID的代码混淆的可能性
            value: AtomicU64::new(1),
        }
    }

    // 获取下一个唯一ID
    // 流程图:
    //   [读取当前值] -> [原子加1] -> [返回原值]
    // 内存顺序说明:
    //   Ordering::Relaxed - 最宽松的内存顺序，仅保证原子性
    pub fn next(&self) -> u64 {
        // fetch_add(1) 原子操作: 读取当前值并加1，返回原始值
        self.value.fetch_add(1, Ordering::Relaxed)
    }
}

// 为IdCounter实现Default trait
// Rust特性: Default trait允许使用IdCounter::default()创建实例
impl Default for IdCounter {
    fn default() -> Self {
        Self::new()
    }
}

/* 使用示例:
   let counter = IdCounter::new();
   let id1 = counter.next(); // 1
   let id2 = counter.next(); // 2
   
   多线程安全:
    多个线程同时调用next()将获得不同的ID值
*/