/// cli.rs - 命令行接口定义模块
/// 职责：使用 clap 库定义命令行参数和子命令
/// 设计目标：提供用户友好的命令行交互和合成器控制

use std::ffi::OsString;  // 操作系统字符串类型（兼容任意字符）
use std::path::PathBuf;  // 路径对象

use clap::{Parser, Subcommand};  // clap 宏库
use clap_complete::Shell;  // Shell 补全支持
use niri_ipc::{Action, OutputAction};  // IPC 动作类型

use crate::utils::version;  // 版本信息工具

/// 主命令行结构
/// 使用 clap 的派生宏自动生成解析器
#[derive(Parser)]
#[command(
    author,  // 自动嵌入作者信息
    version = version(),  // 自定义版本输出
    about,  // 从 Cargo.toml 获取描述
    long_about = None  // 禁用长描述
)]
#[command(args_conflicts_with_subcommands = true)]  // 参数与子命令互斥
#[command(subcommand_value_name = "SUBCOMMAND")]  // 子命令值名称
#[command(subcommand_help_heading = "Subcommands")]  // 子命令帮助标题
pub struct Cli {
    /// 配置文件路径（默认：`$XDG_CONFIG_HOME/niri/config.kdl`）
    ///
    /// 也可通过 `NIRI_CONFIG` 环境变量设置。
    /// 若两者都存在，命令行参数优先。
    #[arg(short, long)]  // 支持 -c/--config
    pub config: Option<PathBuf>,

    /// 全局导入环境变量到 systemd 和 D-Bus，并运行 D-Bus 服务
    ///
    /// 使用场景：
    ///   - 在 systemd 服务中启动主合成器实例时启用
    ///   - 在 TTY 手动启动主实例时启用
    /// 禁用场景：
    ///   - 嵌套窗口模式运行时
    ///   - 非主合成器实例运行时
    #[arg(long)]
    pub session: bool,

    /// 合成器启动后执行的命令
    /// 特性：支持多个参数（如：--command firefox --new-window）
    #[arg(last = true)]  // 必须放在最后
    pub command: Vec<OsString>,

    /// 子命令集合
    #[command(subcommand)]
    pub subcommand: Option<Sub>,
}

/// 子命令枚举
/// 分类：管理命令、调试命令、IPC命令
#[derive(Subcommand)]
pub enum Sub {
    /// 与运行中的 niri 实例通信（IPC）
    Msg {
        /// IPC 消息子命令
        #[command(subcommand)]
        msg: Msg,
        
        /// 以 JSON 格式输出结果
        #[arg(short, long)]
        json: bool,
    },
    
    /// 验证配置文件语法
    Validate {
        /// 配置文件路径（规则同主命令）
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
    
    /// 触发 panic（用于调试和测试）
    Panic,
    
    /// 生成 shell 自动补全脚本
    Completions { shell: Shell },
}

/// IPC 消息子命令枚举
/// 作用：通过 niri msg 命令与合成器运行时交互
#[derive(Subcommand)]
pub enum Msg {
    /// 列出已连接的显示输出
    Outputs,
    
    /// 列出工作区状态
    Workspaces,
    
    /// 列出所有打开的窗口
    Windows,
    
    /// 列出所有 layer-shell 表面（状态栏/通知等）
    Layers,
    
    /// 获取已配置的键盘布局
    KeyboardLayouts,
    
    /// 打印当前聚焦的输出信息
    FocusedOutput,
    
    /// 打印当前聚焦的窗口信息
    FocusedWindow,
    
    /// 用鼠标选择窗口并打印其信息
    PickWindow,
    
    /// 从屏幕拾取颜色
    PickColor,
    
    /// 执行合成器动作（如切换工作区）
    Action {
        /// 具体动作类型
        #[command(subcommand)]
        action: Action,
    },
    
    /// 临时更改输出配置（不修改配置文件）
    ///
    /// 说明：临时配置在配置文件重载后失效
    Output {
        /// 输出名称（使用 `niri msg outputs` 查看）
        #[arg()]
        output: String,
        
        /// 配置动作（如分辨率、位置调整）
        #[command(subcommand)]
        action: OutputAction,
    },
    
    /// 启动事件流（持续接收合成器事件）
    EventStream,
    
    /// 打印运行中 niri 实例的版本
    Version,
    
    /// 请求合成器返回错误（测试用）
    RequestError,
    
    /// 打印窗口概览状态
    OverviewState,
}

/* 命令行结构示意图：

   niri [全局选项] [启动命令...]
   |
   ├── --config <PATH>   指定配置文件
   ├── --session         启用会话模式
   ├── <COMMAND>...      启动时执行的命令
   │
   └── <子命令>
       ├── msg           与运行实例通信
       │   ├── outputs   列出输出
       │   ├── workspaces 列出工作区
       │   ├── ...       (其他 IPC 命令)
       │   └── --json    JSON 输出
       │
       ├── validate      验证配置
       │   └── --config 指定配置文件
       │
       ├── panic         触发崩溃
       │
       └── completions  生成补全脚本
           └── <SHELL> 目标 shell 类型

使用示例：
1. 主合成器启动：niri --session
2. 验证配置：niri validate
3. IPC 查询：niri msg outputs
4. 执行动作：niri msg action focus-window-left
5. 临时调整输出：niri msg output HDMI-1 scale 1.5
*/