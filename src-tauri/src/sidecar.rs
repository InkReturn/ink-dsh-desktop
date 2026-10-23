//! DSH sidecar 的发现、启动、状态查询与进程树回收。
//!
//! 本模块只负责 Phase 1 的本地 `dsh web` 进程：定位 DSH 入口、选择回环端口、
//! 捕获输出、解析官方就绪地址，并保证应用退出时回收整个子进程树。

use std::{
    collections::VecDeque,
    env,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use tauri::State;

/// DSH npm 包内 CLI 入口的相对路径，相对于安装根目录。
const DSH_PACKAGE_BIN: &str = "node_modules/@deepseek-ai/dsh/lib/bin.js";

/// 官方 `dsh web` 输出就绪地址时使用的前缀。
const READY_PREFIX: &str = "dsh web: ";

/// sidecar 首轮启动等待上限；官方冷启动在低配机器上也需要较长时间。
const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);

/// 内存日志最多保留的行数，避免长期运行持续占用内存。
const LOG_LIMIT: usize = 80;

/// 单行日志最多保留的字符数，避免异常堆栈中的超长行撑爆状态。
const LOG_LINE_LIMIT: usize = 4_096;

/// 启动失败时附带给前端的最近日志行数。
const FAILED_LOG_TAIL: usize = 20;

/// sidecar 日志文件超过该大小后轮转为 `.1`，避免无限增长。
const LOG_FILE_LIMIT: u64 = 5 * 1024 * 1024;

/// Windows 创建无控制台窗口子进程的标志。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// sidecar 对外暴露的生命周期状态。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SidecarStatus {
    /// 已开始定位或拉起进程，尚未拿到就绪地址。
    Starting,
    /// 官方 Web UI 已可访问。
    Ready {
        /// 本次 sidecar 实际监听的 HTTP 端口。
        port: u16,
        /// DSH sidecar 根进程的 PID，供诊断使用。
        pid: u32,
    },
    /// 启动失败；`message` 是可展示原因，`log_tail` 已脱敏。
    Failed {
        /// 面向用户的失败摘要。
        message: String,
        /// 最近若干行 stdout/stderr，token 已替换为 `<redacted>`。
        log_tail: Vec<String>,
    },
    /// 应用主动退出或子进程在就绪后自行退出。
    Stopped,
}

/// 输出读取线程向启动流程发送的一次性事件。
#[derive(Debug)]
enum StartupEvent {
    /// 已解析到官方就绪地址。
    Ready {
        /// 完整的本地 Web UI 地址，包含一次性访问 token。
        url: String,
        /// 从地址中解析出的端口。
        port: u16,
    },
    /// 子进程在启动流程结束前退出。
    Exited {
        /// 平台退出码；被信号终止时可能为空。
        code: Option<i32>,
    },
}

/// 一次 sidecar 启动所需的 Node 与 DSH 入口。
#[derive(Debug, Clone)]
struct LaunchSpec {
    /// Node 可执行文件；`node` 表示交给系统 PATH 解析。
    node: PathBuf,
    /// DSH 的 `lib/bin.js` 入口。
    bin: PathBuf,
    /// 运行时来源，仅用于日志与诊断。
    source: &'static str,
}

/// sidecar 的可变运行状态。
struct RuntimeState {
    /// 当前对外状态。
    status: SidecarStatus,
    /// 就绪后的完整访问地址。
    url: Option<String>,
    /// 仍归本应用所有的根进程 PID。
    pid: Option<u32>,
    /// 有界 stdout/stderr 日志，写入前已脱敏。
    logs: VecDeque<String>,
}

/// 本地 DSH sidecar 管理器，由 Tauri 以 `Arc` 托管。
pub struct SidecarManager {
    /// 生命周期状态与日志的同步保护。
    inner: Mutex<RuntimeState>,
    /// 防止并发或重复启动。
    started: AtomicBool,
    /// 应用正在退出时阻止迟到的 spawn 留下孤儿进程。
    shutdown_requested: AtomicBool,
    /// sidecar 日志文件路径，所有行写入前都会脱敏。
    log_path: PathBuf,
    /// 当前 sidecar 的 PID/端口标记，用于崩溃后下次启动清扫。
    pid_path: PathBuf,
    /// 管理器创建时间，用于日志中的相对时间戳。
    clock: Instant,
    /// 可选离线运行时根目录；存在 `node/` 与 `dsh/` 时优先使用。
    bundled_runtime_root: Option<PathBuf>,
    /// Windows Job Object：应用进程崩溃时由系统杀死整棵 sidecar 进程树。
    #[cfg(windows)]
    process_job: Mutex<Option<ProcessJob>>,
}

impl SidecarManager {
    /// 创建一个尚未启动的管理器。
    ///
    /// 参数：
    /// - `log_path`：sidecar 日志文件路径；父目录不存在时会尝试创建。
    /// - `pid_path`：PID/端口标记文件路径，用于应用崩溃后的启动清扫。
    /// - `bundled_runtime_root`：可选离线运行时根目录；传入不存在的路径
    ///   时自动回退本地/全局 DSH。
    ///
    /// 返回值初始状态为 `Starting`，但不会拉起任何进程；真正的启动由
    /// [`SidecarManager::start`] 完成。
    pub fn new(
        log_path: PathBuf,
        pid_path: PathBuf,
        bundled_runtime_root: Option<PathBuf>,
    ) -> Self {
        // 1. 尽量创建日志和标记的父目录，失败留给首次写入时报错并记录。
        // 2. 日志超过大小上限时轮转一次，避免长期运行无限增长。
        // 3. 初始化空状态与两个原子标记。
        if let Some(parent) = log_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Some(parent) = pid_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        rotate_log_if_needed(&log_path);
        Self {
            inner: Mutex::new(RuntimeState {
                status: SidecarStatus::Starting,
                url: None,
                pid: None,
                logs: VecDeque::new(),
            }),
            started: AtomicBool::new(false),
            shutdown_requested: AtomicBool::new(false),
            log_path,
            pid_path,
            clock: Instant::now(),
            bundled_runtime_root,
            #[cfg(windows)]
            process_job: Mutex::new(None),
        }
    }

    /// 同步启动 DSH sidecar 并等待官方 Web UI 就绪。
    ///
    /// 该函数会阻塞直到拿到就绪地址、启动失败或超时，因此调用方必须在
    /// `spawn_blocking` 一类阻塞线程中执行。成功后返回带 token 的完整 URL。
    ///
    /// 返回值：
    /// - `Ok(url)`：`dsh web` 已监听 `127.0.0.1`，`url` 可直接交给 WebView。
    /// - `Err(message)`：定位、spawn、等待或解析失败，状态已切换为 `Failed`
    ///   或 `Stopped`，且已尝试回收子进程树。
    ///
    /// 副作用：拉起 Node 子进程、启动 stdout/stderr 读取线程和退出监控线程。
    pub fn start(self: &Arc<Self>) -> Result<String, String> {
        // 1. 用 CAS 保证一个管理器只启动一次，避免双开 sidecar。
        if self.started.swap(true, Ordering::SeqCst) {
            return Err("DSH sidecar 已经启动过，不能重复启动".to_string());
        }

        // 2. 如果应用在启动前已收到退出请求，直接失败，不创建进程。
        if self.shutdown_requested.load(Ordering::SeqCst) {
            let message = "应用正在退出，已取消启动 DSH sidecar".to_string();
            self.fail_startup(message.clone());
            return Err(message);
        }
        self.set_starting();

        // 3. 先清扫上次崩溃留下的 sidecar，再定位当前可用的 DSH 入口。
        self.sweep_orphan();
        let launch = match resolve_launch_spec(self.bundled_runtime_root.as_deref()) {
            Ok(spec) => spec,
            Err(message) => {
                self.fail_startup(message.clone());
                return Err(message);
            }
        };
        let port = match portpicker::pick_unused_port() {
            Some(port) => port,
            None => {
                let message = "没有找到可用的本地 TCP 端口".to_string();
                self.fail_startup(message.clone());
                return Err(message);
            }
        };
        self.record_log(
            "lifecycle",
            &format!(
                "准备启动 DSH（{}）：node={}，bin={}，端口 {port}",
                launch.source,
                launch.node.display(),
                launch.bin.display()
            ),
        );

        // 4. 通过 node 启动 `dsh web`，并注入稳定的无颜色、无遥测环境。
        let mut command = build_sidecar_command(&launch, port);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let message = format!("无法启动 DSH sidecar（请确认 node 在 PATH 中）：{error}");
                self.fail_startup(message.clone());
                return Err(message);
            }
        };

        // 5. 立即登记 PID 和标记；即使应用在就绪前退出，也能按 PID 回收进程树。
        let pid = child.id();
        self.set_pid(pid);
        #[cfg(windows)]
        match ProcessJob::attach(&child) {
            Ok(job) => self.replace_process_job(job),
            Err(error) => self.record_log(
                "lifecycle",
                &format!("警告：无法创建 sidecar Job Object，将仅依赖 taskkill 回收：{error}"),
            ),
        }
        self.write_pid_marker(pid, port);
        if self.shutdown_requested.load(Ordering::SeqCst) {
            self.mark_stopped();
            terminate_process_tree(pid);
            self.clear_process_job();
            self.remove_pid_marker();
            return Err("应用正在退出，已终止启动中的 DSH sidecar".to_string());
        }
        self.record_log(
            "lifecycle",
            &format!("DSH sidecar 已启动：pid={pid}，等待就绪地址"),
        );

        // 6. 取出输出管道；任何一步失败都要先杀掉已启动的进程。
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let message = "无法捕获 DSH sidecar 的 stdout".to_string();
                self.abort_startup(message.clone());
                return Err(message);
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                let message = "无法捕获 DSH sidecar 的 stderr".to_string();
                self.abort_startup(message.clone());
                return Err(message);
            }
        };

        // 7. 启动独立读取线程，避免 stdout/stderr 任一管道写满导致子进程阻塞。
        let (startup_tx, startup_rx) = mpsc::channel();
        if let Err(error) =
            spawn_output_reader(Arc::clone(self), stdout, "stdout", Some(startup_tx.clone()))
        {
            let message = format!("无法创建 DSH stdout 读取线程：{error}");
            self.abort_startup(message.clone());
            return Err(message);
        }
        if let Err(error) = spawn_output_reader(Arc::clone(self), stderr, "stderr", None) {
            let message = format!("无法创建 DSH stderr 读取线程：{error}");
            self.abort_startup(message.clone());
            return Err(message);
        }
        if let Err(error) = spawn_exit_monitor(Arc::clone(self), child, pid, startup_tx) {
            let message = format!("无法创建 DSH 退出监控线程：{error}");
            self.abort_startup(message.clone());
            return Err(message);
        }

        // 8. 等待官方就绪行；成功后再次确认应用未退出，再发布 Ready 状态。
        match self.await_ready(startup_rx, port) {
            Ok(url) => match self.mark_ready(port, pid, url.clone()) {
                Ok(()) => Ok(url),
                Err(message) => {
                    self.abort_startup(message.clone());
                    Err(message)
                }
            },
            Err(message) => {
                self.abort_startup(message.clone());
                Err(message)
            }
        }
    }

    /// 查询当前 sidecar 状态快照。
    ///
    /// 返回值是克隆后的状态，调用方无法借此修改内部状态；失败状态会附带
    /// 最近 20 行脱敏日志。
    pub fn status(&self) -> SidecarStatus {
        // 1. 短暂持锁读取状态。
        // 2. 失败状态的日志尾已在写入时保存在状态里。
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .status
            .clone()
    }

    /// 取当前 sidecar 的完整访问地址。
    ///
    /// 返回值仅在 `Ready` 期间为 `Some`，地址内含访问 token；退出后立即清空。
    pub fn url(&self) -> Option<String> {
        // 1. 持锁读取并克隆 URL，避免持锁期间跨线程使用。
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .url
            .clone()
    }

    /// 请求回收 sidecar 进程树并切换到停止状态。
    ///
    /// 可重复调用；应用退出事件、窗口销毁事件都会触发。该函数不等待子进程
    /// 监控线程结束，但 Windows 上会同步等待 `taskkill` 完成。
    pub fn shutdown(&self) {
        // 1. 先置退出标记，阻止仍在准备阶段的 start 继续留下进程。
        self.shutdown_requested.store(true, Ordering::SeqCst);

        // 2. 原子地取走 PID 并清空可访问地址，避免退出后仍能拿到 token。
        let pid = {
            let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            let pid = inner.pid.take();
            inner.url = None;
            if matches!(
                inner.status,
                SidecarStatus::Starting | SidecarStatus::Ready { .. }
            ) {
                inner.status = SidecarStatus::Stopped;
            }
            pid
        };

        // 3. 按根 PID 回收整棵进程树，再关闭 Job Object 并清理标记文件。
        if let Some(pid) = pid {
            terminate_process_tree(pid);
        }
        self.clear_process_job();
        self.remove_pid_marker();
    }

    /// 允许用户在失败/停止后重新拉起 sidecar。
    ///
    /// 返回值：当前仍有活动进程或状态不是 `Failed`/`Stopped` 时返回错误；
    /// 成功时把状态重置为 `Starting`，并把一次性的启动 CAS 标记复位。
    /// 调用方应在随后立即调用 [`SidecarManager::start`]。
    pub fn prepare_retry(&self) -> Result<(), String> {
        // 1. 仍有 PID 或处于 Starting/Ready 时拒绝，避免并发双开。
        // 2. 应用退出流程中拒绝重试。
        // 3. 复位启动 CAS、清空上一轮状态，并保留磁盘日志用于追踪历史。
        if self.shutdown_requested.load(Ordering::SeqCst) {
            return Err("应用正在退出，不能重启 sidecar".to_string());
        }
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if inner.pid.is_some()
            || !matches!(
                inner.status,
                SidecarStatus::Failed { .. } | SidecarStatus::Stopped
            )
        {
            return Err("sidecar 当前不处于可重试状态".to_string());
        }
        self.started.store(false, Ordering::SeqCst);
        inner.status = SidecarStatus::Starting;
        inner.url = None;
        inner.pid = None;
        inner.logs.clear();
        Ok(())
    }

    /// 把状态重置为 Starting 并清空上一轮运行数据。
    fn set_starting(&self) {
        // 1. 从空日志、空 PID、空 URL 开始，确保状态命令不会读到旧 token。
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.status = SidecarStatus::Starting;
        inner.url = None;
        inner.pid = None;
        inner.logs.clear();
    }

    /// 登记当前根进程 PID，供窗口关闭或应用退出时回收。
    ///
    /// 参数：
    /// - `pid`：`dsh web` 根进程的 OS 进程号。
    fn set_pid(&self, pid: u32) {
        // 1. 只更新 PID；状态仍由后续就绪或失败事件决定。
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pid = Some(pid);
    }

    /// 发布就绪状态。
    ///
    /// 参数：
    /// - `port`：官方地址报告的监听端口，必须与预选端口一致。
    /// - `pid`：仍归本应用所有的根进程 PID。
    /// - `url`：包含 token 的完整 Web UI 地址。
    ///
    /// 返回 `Err` 表示进程在等待期间已经退出，或应用已请求 shutdown。
    fn mark_ready(&self, port: u16, pid: u32, url: String) -> Result<(), String> {
        // 1. 应用已退出时不允许重新发布地址。
        if self.shutdown_requested.load(Ordering::SeqCst) {
            return Err("应用正在退出，已放弃 DSH 就绪地址".to_string());
        }

        // 2. 只有仍处于 Starting 的实例可以进入 Ready，避免覆盖退出状态。
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if !matches!(inner.status, SidecarStatus::Starting) {
            return Err("DSH sidecar 在就绪前已经退出".to_string());
        }
        inner.status = SidecarStatus::Ready { port, pid };
        inner.url = Some(url);
        inner.pid = Some(pid);
        Ok(())
    }

    /// 处理子进程退出，并把状态从 Starting/Ready 迁移到 Failed/Stopped。
    ///
    /// 参数：
    /// - `pid`：退出的根进程 PID，用于拒绝过期的监控事件。
    /// - `code`：平台退出码；无普通退出码时为空。
    fn mark_exited(&self, pid: u32, code: Option<i32>) {
        // 1. 只处理当前登记进程的事件，避免旧监控覆盖新状态。
        {
            let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            if inner.pid != Some(pid) {
                return;
            }

            // 2. 退出后先清 PID 和 token；随后按原先处于哪个阶段决定终态。
            let was_starting = matches!(inner.status, SidecarStatus::Starting);
            let was_ready = matches!(inner.status, SidecarStatus::Ready { .. });
            inner.pid = None;
            inner.url = None;
            if was_starting {
                let suffix = code
                    .map(|code| format!("，退出码 {code}"))
                    .unwrap_or_default();
                inner.status = SidecarStatus::Failed {
                    message: format!("DSH sidecar 在就绪前退出{suffix}"),
                    log_tail: tail_logs(&inner.logs, FAILED_LOG_TAIL),
                };
            } else if was_ready {
                inner.status = SidecarStatus::Stopped;
            }
        }

        // 3. 释放 Job Object 与 PID 标记，避免陈旧资源影响下一次启动。
        self.clear_process_job();
        self.remove_pid_marker();
    }

    /// 启动失败时记录状态、取走 PID，并回收进程树。
    ///
    /// 参数：
    /// - `message`：已完成脱敏的失败摘要，会原样返回给前端。
    fn abort_startup(&self, message: String) {
        // 1. 仅 Starting 状态需要转 Failed；退出或已失败状态不再覆盖。
        let pid = {
            let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            if !matches!(inner.status, SidecarStatus::Starting) {
                return;
            }
            inner.status = SidecarStatus::Failed {
                message: message.clone(),
                log_tail: tail_logs(&inner.logs, FAILED_LOG_TAIL),
            };
            inner.pid.take()
        };

        // 2. 锁外执行进程树回收，避免 taskkill 阻塞状态查询。
        if let Some(pid) = pid {
            terminate_process_tree(pid);
        }

        // 3. 失败后释放 Job Object 并删除标记文件。
        self.clear_process_job();
        self.remove_pid_marker();
    }

    /// 把当前状态直接置为失败，不触发进程回收。
    ///
    /// 主要供外部线程在无法进入正常启动流程时补充错误状态；已有 pid 时
    /// 调用方应改用 [`SidecarManager::shutdown`] 或 `abort_startup`。
    fn fail_startup(&self, message: String) {
        // 1. 写入失败状态和最近日志。
        // 2. 若已登记 PID 则一并清空，避免状态与进程所有权不一致。
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.status = SidecarStatus::Failed {
            message,
            log_tail: tail_logs(&inner.logs, FAILED_LOG_TAIL),
        };
        inner.url = None;
        inner.pid = None;
        drop(inner);
        self.remove_pid_marker();
    }

    /// 主动停止且不标记为失败。
    fn mark_stopped(&self) {
        // 1. 清空 PID 与 token，并将状态改为 Stopped。
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.pid = None;
        inner.url = None;
        inner.status = SidecarStatus::Stopped;
    }

    /// 记录一行带来源的 sidecar 日志，并在写入前脱敏 token。
    ///
    /// 参数：
    /// - `source`：日志来源，固定为 `stdout`、`stderr` 或 `lifecycle`。
    /// - `line`：原始输出行，允许包含 URL 等敏感内容。
    fn record_log(&self, source: &str, line: &str) {
        // 1. 去掉行尾空白并替换 token 值。
        // 2. 截断超长行后追加到环形日志。
        // 3. 超出上限时从最旧的一行开始丢弃。
        let sanitized: String = redact_token(line.trim_end())
            .chars()
            .take(LOG_LINE_LIMIT)
            .collect();
        let elapsed = self.clock.elapsed().as_secs_f64();
        let entry = format!("[+{elapsed:.3}s] [{source}] {sanitized}");
        {
            let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            inner.logs.push_back(entry.clone());
            while inner.logs.len() > LOG_LIMIT {
                inner.logs.pop_front();
            }
        }
        self.append_log_file(&entry);
    }

    /// 把一行已脱敏日志追加写入磁盘。
    ///
    /// 参数：
    /// - `entry`：已包含相对时间戳、来源和脱敏正文的日志行。
    ///
    /// 副作用：追加写 `self.log_path`；写入失败时只在 stderr 提示，不阻断
    /// sidecar 生命周期。
    fn append_log_file(&self, entry: &str) {
        // 1. 以追加模式打开文件，目录不存在时先补建一次。
        // 2. 写入失败只报告终端，不让诊断日志影响主流程。
        if let Some(parent) = self.log_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            Ok(mut file) => {
                if let Err(error) = writeln!(file, "{entry}") {
                    eprintln!(
                        "[inkdsh] 写入 sidecar 日志失败（{}）：{error}",
                        self.log_path.display()
                    );
                }
            }
            Err(error) => {
                eprintln!(
                    "[inkdsh] 打开 sidecar 日志失败（{}）：{error}",
                    self.log_path.display()
                );
            }
        }
    }

    /// 清扫上次应用崩溃后遗留的 sidecar。
    ///
    /// 识别依据是 PID 标记文件中的 `(pid, port)` 与当前端口监听者双重匹配：
    /// 只有记录端口仍由记录 PID 监听时才结束该进程树，绝不按裸 PID 猜测。
    /// 标记损坏、端口已释放或监听者不匹配时只删除陈旧标记。
    fn sweep_orphan(&self) {
        // 1. 读取并解析两行标记；损坏即视为陈旧垃圾。
        let Ok(text) = fs::read_to_string(&self.pid_path) else {
            return;
        };
        let mut lines = text.lines();
        let (Some(pid), Some(port)) = (
            lines
                .next()
                .and_then(|line| line.trim().parse::<u32>().ok()),
            lines
                .next()
                .and_then(|line| line.trim().parse::<u16>().ok()),
        ) else {
            let _ = fs::remove_file(&self.pid_path);
            return;
        };

        // 2. 端口必须仍由记录中的 PID 监听，才认定为上次运行残留。
        if port_owner_pid(port) != Some(pid) {
            let _ = fs::remove_file(&self.pid_path);
            return;
        }

        // 3. 结束确认过的孤儿进程树，并清理标记。
        self.record_log(
            "lifecycle",
            &format!("发现上次运行残留的 DSH sidecar：pid={pid}，port={port}，开始回收"),
        );
        terminate_process_tree(pid);
        let _ = fs::remove_file(&self.pid_path);
        self.record_log("lifecycle", "残留 DSH sidecar 已回收");
    }

    /// 写入当前 sidecar 的 PID 与端口标记。
    ///
    /// 参数：
    /// - `pid`：DSH 根进程 PID。
    /// - `port`：本次选择的回环监听端口。
    ///
    /// 副作用：覆盖写 `self.pid_path`；失败只记录日志，不影响启动。
    fn write_pid_marker(&self, pid: u32, port: u16) {
        // 1. 使用两行固定格式，便于崩溃后的下一次启动最小化解析。
        if let Err(error) = fs::write(&self.pid_path, format!("{pid}\n{port}\n")) {
            self.record_log(
                "lifecycle",
                &format!(
                    "警告：无法写入 sidecar PID 标记（{}）：{error}",
                    self.pid_path.display()
                ),
            );
        }
    }

    /// 删除当前 sidecar 的 PID 标记文件。
    fn remove_pid_marker(&self) {
        // 1. 文件不存在视为成功，删除失败留给下次启动按端口校验后清理。
        let _ = fs::remove_file(&self.pid_path);
    }

    /// 保存新 sidecar 的 Windows Job Object，替换可能存在的旧句柄。
    ///
    /// 参数：
    /// - `job`：已绑定当前子进程、配置了 `KILL_ON_JOB_CLOSE` 的作业对象。
    #[cfg(windows)]
    fn replace_process_job(&self, job: ProcessJob) {
        // 1. 旧 Job 正常情况下已空；替换时关闭句柄，避免泄漏。
        let old = self
            .process_job
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .replace(job);
        drop(old);
    }

    /// 关闭 Windows Job Object；若其中仍有进程，系统会一并结束。
    #[cfg(windows)]
    fn clear_process_job(&self) {
        // 1. 取走句柄后在锁外关闭，避免 Drop 期间持锁。
        let job = self
            .process_job
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        drop(job);
    }

    /// Unix 平台没有 Job Object，保留空实现以统一调用点。
    #[cfg(not(windows))]
    fn clear_process_job(&self) {
        // 1. Unix 由进程组 TERM/KILL 与 PID 标记兜底。
    }

    /// 等待官方就绪地址、进程退出或启动超时。
    ///
    /// 参数：
    /// - `startup_rx`：stdout 读取线程与退出监控线程共用的事件接收端。
    /// - `expected_port`：启动前由 `portpicker` 选出的回环端口。
    ///
    /// 返回值：就绪地址；超时、退出、通道断开或应用退出时返回错误摘要。
    fn await_ready(
        &self,
        startup_rx: Receiver<StartupEvent>,
        expected_port: u16,
    ) -> Result<String, String> {
        // 1. 计算绝对截止时间，并开始短周期轮询，以便及时响应 shutdown。
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            if self.shutdown_requested.load(Ordering::SeqCst) {
                return Err("应用正在退出，已停止等待 DSH 就绪".to_string());
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "等待 DSH 就绪超时（{} 秒）",
                    STARTUP_TIMEOUT.as_secs()
                ));
            }

            // 2. 每次最多等待 250ms，收到事件后按类型处理。
            match startup_rx.recv_timeout(remaining.min(Duration::from_millis(250))) {
                Ok(StartupEvent::Ready { url, port }) if port == expected_port => {
                    return Ok(url);
                }
                Ok(StartupEvent::Ready { port, .. }) => {
                    return Err(format!(
                        "DSH 就绪端口与预选端口不一致：期望 {expected_port}，实际 {port}"
                    ));
                }
                Ok(StartupEvent::Exited { code }) => {
                    let suffix = code
                        .map(|code| format!("，退出码 {code}"))
                        .unwrap_or_default();
                    return Err(format!("DSH sidecar 在就绪前退出{suffix}"));
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("DSH sidecar 输出通道意外关闭".to_string());
                }
            }
        }
    }
}

/// 查询 DSH sidecar 当前状态。
///
/// 参数：
/// - `state`：Tauri 注入的管理器状态；应用启动时已注册，不会为空。
///
/// 返回值：状态枚举快照，失败时包含脱敏日志尾。
#[tauri::command]
pub fn sidecar_status(state: State<'_, Arc<SidecarManager>>) -> SidecarStatus {
    // 1. 读取克隆后的状态并直接序列化返回，不把内部锁暴露给前端。
    state.status()
}

/// 查询 DSH Web UI 的完整访问地址。
///
/// 参数：
/// - `state`：Tauri 注入的管理器状态。
///
/// 返回值：仅在 sidecar 就绪时为 `Some(url)`，退出后为 `None`。
#[tauri::command]
pub fn sidecar_url(state: State<'_, Arc<SidecarManager>>) -> Option<String> {
    // 1. 返回存储中的地址；token 只通过本地窗口的 IPC 暴露。
    state.url()
}

/// 解析本次 sidecar 应使用的 Node 与 DSH 入口。
///
/// 优先级：
/// 1. 环境变量 `DSH_BIN`（开发调试显式覆盖）；
/// 2. 可选离线运行时目录（`node/` + `dsh/lib/bin.js`）；
/// 3. 项目本地 `node_modules`；
/// 4. PATH 中 `dsh` shim 对应的全局 npm/pnpm 安装。
///
/// 参数：
/// - `bundled_root`：Tauri 资源目录下可选的 `runtime/`；不存在时回退。
///
/// 返回值：可直接用于构造 Command 的 [`LaunchSpec`]；全部缺失时返回带
/// 修复指引的错误。该函数只读文件系统，不启动进程。
fn resolve_launch_spec(bundled_root: Option<&Path>) -> Result<LaunchSpec, String> {
    // 1. 显式 DSH_BIN 覆盖优先；设置错误时硬失败，避免悄悄换版本。
    if let Some(raw) = env::var_os("DSH_BIN") {
        let bin = PathBuf::from(raw);
        if !bin.is_file() {
            return Err(format!("DSH_BIN 指向的文件不存在：{}", bin.display()));
        }
        return Ok(LaunchSpec {
            node: node_from_env_or_path(),
            bin: normalize_existing_path(&bin),
            source: "DSH_BIN 覆盖",
        });
    }

    // 2. 可选离线运行时优先级高于系统安装，保证离线安装结果可复现。
    if let Some(spec) = bundled_root.and_then(bundled_launch_spec) {
        return Ok(spec);
    }

    // 3. 再查本地依赖与全局安装，均复用系统 Node。
    if let Some((bin, source)) = locate_installed_dsh_bin() {
        return Ok(LaunchSpec {
            node: node_from_env_or_path(),
            bin,
            source,
        });
    }

    Err("未找到 DSH。请安装带可选离线运行时的 inkdsh，或执行 \
         `pnpm install` / `npm i -g @deepseek-ai/dsh`；开发时也可以设置 \
         DSH_BIN 指向 node_modules/@deepseek-ai/dsh/lib/bin.js。"
        .to_string())
}

/// 检查可选离线运行时目录并生成启动规格。
///
/// 参数：
/// - `root`：运行时根目录，约定包含 `node/` 与 `dsh/lib/bin.js`。
///
/// 返回值：Node 与 DSH 都存在时返回捆绑来源；任一缺失返回 `None`，由上层
/// 继续尝试系统安装。该函数不读取版本清单，也不执行任何文件。
fn bundled_launch_spec(root: &Path) -> Option<LaunchSpec> {
    // 1. 按平台选择 Node 文件名并校验 DSH 入口存在。
    // 2. Tauri 资源路径在 Windows 上可能是 `\\?\D:\...` verbatim 形式；
    //    Node 会把这种入口参数误解析为盘符 `D:`，必须先去掉 verbatim 前缀。
    // 3. 两者齐全才视为可用捆绑运行时。
    let node = root
        .join("node")
        .join(if cfg!(windows) { "node.exe" } else { "node" });
    let bin = root.join("dsh").join("lib").join("bin.js");
    if !node.is_file() || !bin.is_file() {
        return None;
    }
    Some(LaunchSpec {
        node: normalize_existing_path(&node),
        bin: normalize_existing_path(&bin),
        source: "捆绑运行时",
    })
}

/// 把已存在的路径归一化为不带 Windows verbatim 前缀的绝对路径。
///
/// 参数：
/// - `path`：必须已存在的文件或目录路径。
///
/// 返回值：`dunce::canonicalize` 成功时返回归一化路径；失败时保留原始路径，
/// 由后续启动错误给出更具体的提示。Windows 上去掉 `\\?\` 前缀，避免
/// node.exe 将脚本参数误解析为盘符。
fn normalize_existing_path(path: &Path) -> PathBuf {
    // 1. 使用 dunce 兼容 UNC 与超长路径，同时去掉 Node 不接受的 verbatim 前缀。
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 在项目本地和全局安装中查找 DSH 入口。
///
/// 返回值：`(bin.js 路径, 来源标签)`；未找到返回 `None`。来源标签用于日志，
/// 不参与启动决策。
fn locate_installed_dsh_bin() -> Option<(PathBuf, &'static str)> {
    // 1. 先查本地依赖，pnpm install 后应命中项目内的固定版本。
    for root in local_install_roots() {
        let candidate = root.join(DSH_PACKAGE_BIN);
        if candidate.is_file() {
            return Some((candidate, "项目本地 DSH"));
        }
    }

    // 2. 再查全局安装；PATH shim 目录与 Unix prefix/lib 都会覆盖。
    for root in global_install_roots() {
        for candidate in global_package_candidates(&root) {
            if candidate.is_file() {
                return Some((candidate, "全局 DSH"));
            }
        }
    }

    None
}

/// 读取显式 Node 覆盖，否则交给 PATH 解析 `node`。
///
/// 返回值：`DSH_NODE` 非空时返回其路径；否则返回 `node`。`DSH_NODE` 是开发
/// 调试用的显式覆盖，不参与捆绑运行时选择。
fn node_from_env_or_path() -> PathBuf {
    // 1. 空值等价于未设置，避免命令行传空串导致 spawn 失败。
    env::var_os("DSH_NODE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|path| normalize_existing_path(&path))
        .unwrap_or_else(|| PathBuf::from("node"))
}

/// 收集可能包含本地 DSH 依赖的安装根目录。
///
/// 返回值按优先级排序：当前工作目录的 3 级祖先，然后是 Cargo 项目根。
/// 目录可能重复，调用方只按顺序做存在性探测，因此无需额外去重。
fn local_install_roots() -> Vec<PathBuf> {
    // 1. 加入运行目录及其有限祖先，兼容从仓库根或子目录启动。
    // 2. 加入编译期识别的仓库根，兼容开发时工作目录被 IDE 改写。
    let mut roots = Vec::new();
    if let Ok(current_dir) = env::current_dir() {
        roots.extend(current_dir.ancestors().take(3).map(Path::to_path_buf));
    }
    if let Some(project_root) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
        roots.push(project_root.to_path_buf());
    }
    roots
}

/// 收集全局 DSH 可能的安装根目录。
///
/// 返回值包括 PATH 中实际存在 `dsh` shim 的目录，以及 npm 的常见全局 prefix。
/// 该函数只生成候选，不保证 DSH 包真的存在。
fn global_install_roots() -> Vec<PathBuf> {
    // 1. 扫描 PATH，只有看到 dsh shim 的目录才加入候选，避免无关目录误判。
    // 2. 补充 npm 在 Windows/Unix 上的默认全局目录。
    let mut roots = Vec::new();
    if let Some(path) = env::var_os("PATH") {
        for directory in env::split_paths(&path) {
            let has_shim = ["dsh", "dsh.exe", "dsh.cmd", "dsh.ps1", "dsh.bat"]
                .iter()
                .any(|name| directory.join(name).is_file());
            if has_shim {
                roots.push(directory);
            }
        }
    }

    if let Some(app_data) = env::var_os("APPDATA") {
        roots.push(PathBuf::from(app_data).join("npm"));
    }
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join(".npm-global").join("lib"));
        roots.push(home.join(".local").join("share").join("pnpm"));
    }
    if let Some(profile) = env::var_os("USERPROFILE") {
        roots.push(
            PathBuf::from(profile)
                .join("AppData")
                .join("Roaming")
                .join("npm"),
        );
    }
    roots
}

/// 根据全局 prefix 推导 DSH 包的 `lib/bin.js` 候选路径。
///
/// 参数：
/// - `prefix`：PATH 中的 shim 目录或 npm 全局 prefix。
///
/// 返回值：既覆盖 Windows 的 `prefix/node_modules`，也覆盖 Unix 的
/// `prefix/../lib/node_modules`。
fn global_package_candidates(prefix: &Path) -> Vec<PathBuf> {
    // 1. Windows / npm prefix 直接把 node_modules 放在 prefix 下。
    // 2. Unix 的 shim 目录是 prefix/bin，模块在 prefix/lib/node_modules 下。
    let mut candidates = vec![prefix.join(DSH_PACKAGE_BIN)];
    if let Some(parent) = prefix.parent() {
        candidates.push(parent.join("lib").join(DSH_PACKAGE_BIN));
    }
    candidates
}

/// 构造 `<node> <bin.js> web --host 127.0.0.1 --port <port> --no-open` 命令。
///
/// 参数：
/// - `launch`：已解析的 Node 与 DSH 入口，可来自捆绑、本地或全局安装。
/// - `port`：仅用于回环监听的 TCP 端口，由 `portpicker` 选出。
///
/// 返回值：尚未 spawn 的 Command。Windows 下隐藏控制台窗口，Unix 下创建
/// 独立进程组，便于退出时整组回收。
fn build_sidecar_command(launch: &LaunchSpec, port: u16) -> Command {
    // 1. 使用 LaunchSpec 中确定的 Node 与 DSH 入口。
    // 2. 将官方 Web UI 限定监听 127.0.0.1，并禁用自动打开浏览器。
    // 3. 关闭颜色和遥测，保证就绪行可稳定解析。
    // 4. 按平台设置控制台隐藏或独立进程组。
    let mut command = Command::new(&launch.node);
    command
        .arg(&launch.bin)
        .arg("web")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--no-open")
        .env("NO_COLOR", "1")
        .env("DSH_TELEMETRY_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    command
}

/// 启动一个后台线程，逐行读取 sidecar 输出并记录日志。
///
/// 参数：
/// - `manager`：用于写日志和在 stdout 中发布就绪事件的管理器。
/// - `reader`：子进程 stdout 或 stderr 的拥有型读取端。
/// - `source`：日志来源标签，写入 `[stdout]` 或 `[stderr]`。
/// - `startup_tx`：仅 stdout 传入；解析到就绪地址时向启动流程发送事件。
///
/// 返回值：线程句柄；创建线程失败时返回底层 IO 错误。线程正常结束时无需
/// 调用方 join。
fn spawn_output_reader<R>(
    manager: Arc<SidecarManager>,
    reader: R,
    source: &'static str,
    startup_tx: Option<Sender<StartupEvent>>,
) -> std::io::Result<thread::JoinHandle<()>>
where
    R: Read + Send + 'static,
{
    // 1. 用有界行读取器消费管道，防止子进程因输出缓冲区写满而卡死。
    // 2. 每行先脱敏记录；stdout 再尝试解析官方就绪地址。
    // 3. 发送一次 Ready 后清空发送端，避免重复发送同一个地址。
    thread::Builder::new()
        .name(format!("inkdsh-sidecar-{source}"))
        .spawn(move || {
            let mut ready_tx = startup_tx;
            for line in BufReader::new(reader).lines() {
                match line {
                    Ok(line) => {
                        manager.record_log(source, &line);
                        if let Some((url, port)) = parse_ready_url(&line) {
                            if let Some(tx) = ready_tx.take() {
                                let _ = tx.send(StartupEvent::Ready { url, port });
                            }
                        }
                    }
                    Err(error) => {
                        manager.record_log(source, &format!("读取 sidecar 输出失败：{error}"));
                        break;
                    }
                }
            }
        })
}

/// 启动退出监控线程，wait 子进程并同步生命周期状态。
///
/// 参数：
/// - `manager`：接收退出状态并更新内部状态的管理器。
/// - `child`：由监控线程独占的根子进程句柄，用于阻塞 wait 和回收僵尸进程。
/// - `pid`：spawn 时记录的根进程 PID，用于校验状态仍属于该进程。
/// - `startup_tx`：启动阶段事件发送端；过早退出会唤醒等待中的启动流程。
///
/// 返回值：线程句柄；创建线程失败时返回底层 IO 错误。
fn spawn_exit_monitor(
    manager: Arc<SidecarManager>,
    mut child: Child,
    pid: u32,
    startup_tx: Sender<StartupEvent>,
) -> std::io::Result<thread::JoinHandle<()>> {
    // 1. 在独立线程中阻塞 wait，避免占用 Tauri 的事件循环。
    // 2. 正常退出读取退出码，异常 wait 也按已退出处理并记录日志。
    // 3. 先更新状态，再通知启动流程，保证前端查询看到一致结果。
    thread::Builder::new()
        .name("inkdsh-sidecar-monitor".to_string())
        .spawn(move || {
            let wait_result = child.wait();
            let code = wait_result.as_ref().ok().and_then(|status| status.code());
            manager.mark_exited(pid, code);
            match wait_result {
                Ok(status) => {
                    manager.record_log("lifecycle", &format!("DSH sidecar 已退出：{status}"))
                }
                Err(error) => {
                    manager.record_log("lifecycle", &format!("等待 DSH sidecar 退出失败：{error}"))
                }
            }
            let _ = startup_tx.send(StartupEvent::Exited { code });
        })
}

/// 当日志文件超过上限时轮转为 `<文件名>.1`。
///
/// 参数：
/// - `path`：sidecar 日志文件路径；不存在或未超限时不做任何事。
///
/// 副作用：删除旧的 `.1` 文件并重命名当前日志；失败静默忽略，不能让日志
/// 轮转阻止应用启动。
fn rotate_log_if_needed(path: &Path) {
    // 1. 读取文件大小；不存在或未超限直接返回。
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.len() <= LOG_FILE_LIMIT {
        return;
    }

    // 2. 只保留一个历史文件，先删旧档再重命名当前日志。
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("sidecar.log");
    let rotated = path.with_file_name(format!("{name}.1"));
    let _ = fs::remove_file(&rotated);
    let _ = fs::rename(path, rotated);
}

/// 查询监听指定 TCP 端口的进程 PID。
///
/// 参数：
/// - `port`：待查询的本地端口，必须是 1–65535。
///
/// 返回值：仅当能确认某个进程处于 LISTENING 时返回 PID；命令缺失、解析
/// 失败或没有监听者时返回 `None`，调用方不得据此杀进程。
fn port_owner_pid(port: u16) -> Option<u32> {
    #[cfg(windows)]
    {
        // 1. 使用 netstat 获取监听表；GUI 进程必须隐藏控制台窗口。
        use std::os::windows::process::CommandExt;
        let output = Command::new("netstat")
            .arg("-ano")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        parse_netstat_owner(&String::from_utf8_lossy(&output.stdout), port)
    }

    #[cfg(unix)]
    {
        // 1. lsof 在 macOS 默认可用，Linux 可能缺失；缺失时放弃清扫。
        let output = Command::new("lsof")
            .args(["-ti", &format!("tcp:{port}")])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .and_then(|line| line.trim().parse().ok())
    }

    #[cfg(not(any(windows, unix)))]
    {
        let _ = port;
        None
    }
}

/// 从 Windows `netstat -ano` 输出中解析指定端口的监听 PID。
///
/// 参数：
/// - `output`：netstat 的完整标准输出。
/// - `port`：目标本地端口。
///
/// 返回值：第一个匹配 `TCP <本地地址>:<port> ... LISTENING <pid>` 的 PID；
/// 未找到或字段异常时返回 `None`。
fn parse_netstat_owner(output: &str, port: u16) -> Option<u32> {
    // 1. 按行切分，只接受至少 5 列的 TCP 记录。
    // 2. 本地地址必须以 `:<port>` 结尾，状态必须是 LISTENING。
    // 3. 返回末列 PID，解析失败则继续找下一行。
    let needle = format!(":{port}");
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 5 || !fields[0].eq_ignore_ascii_case("TCP") {
            continue;
        }
        if !fields[1].ends_with(&needle) || !fields[3].eq_ignore_ascii_case("LISTENING") {
            continue;
        }
        if let Ok(pid) = fields[4].parse::<u32>() {
            return Some(pid);
        }
    }
    None
}

/// Windows Job Object 包装句柄，配置为句柄关闭时杀死作业内所有进程。
#[cfg(windows)]
struct ProcessJob {
    /// 原生 `HANDLE`；使用 `isize` 保存以保证结构可跨线程放进 Mutex。
    handle: isize,
}

#[cfg(windows)]
impl ProcessJob {
    /// 创建 Job Object 并把已 spawn 的 DSH 根进程加入其中。
    ///
    /// 参数：
    /// - `child`：刚启动的 DSH 子进程；其进程句柄仍需由调用方持有。
    ///
    /// 返回值：绑定成功的作业对象；任何 Windows API 失败时返回可读错误，
    /// 调用方会降级为 taskkill 回收。
    fn attach(child: &Child) -> Result<Self, String> {
        use std::mem::{size_of, zeroed};
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        // 1. 创建匿名 Job Object。
        // 2. 设置 KILL_ON_JOB_CLOSE：应用崩溃时系统关闭句柄并杀死 sidecar 树。
        // 3. 将当前 sidecar 根进程加入作业；失败时关闭句柄并返回错误。
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(format!(
                    "CreateJobObjectW 失败：{}",
                    std::io::Error::last_os_error()
                ));
            }

            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if configured == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(handle);
                return Err(format!("SetInformationJobObject 失败：{error}"));
            }

            let assigned = AssignProcessToJobObject(handle, child.as_raw_handle() as HANDLE);
            if assigned == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(handle);
                return Err(format!("AssignProcessToJobObject 失败：{error}"));
            }

            Ok(Self {
                handle: handle as isize,
            })
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessJob {
    /// 关闭作业句柄；若应用仍持有该 Job，系统会同时结束作业内进程。
    fn drop(&mut self) {
        // 1. 空句柄不处理。
        // 2. 调用 CloseHandle 触发 KILL_ON_JOB_CLOSE 语义。
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        if self.handle != 0 {
            unsafe {
                CloseHandle(self.handle as HANDLE);
            }
        }
    }
}

/// 从官方 stdout 行中解析完整的 Web UI 地址与端口。
///
/// 官方格式为 `dsh web: http://127.0.0.1:<port>/?token=<token>`。
///
/// 参数：
/// - `line`：sidecar stdout 的单行文本，可包含其他前缀日志。
///
/// 返回值：形如 `(url, port)` 的元组；主机不是回环地址、端口非法、缺少
/// 查询参数或 token 为空时返回 `None`。
fn parse_ready_url(line: &str) -> Option<(String, u16)> {
    // 1. 定位官方固定前缀，并截取首个空白分隔的 URL。
    let marker = line.find(READY_PREFIX)?;
    let candidate = line[marker + READY_PREFIX.len()..]
        .split_whitespace()
        .next()?;

    // 2. 只接受 127.0.0.1 上的明文 HTTP，避免误识别公网或外部地址。
    let authority_and_path = candidate.strip_prefix("http://127.0.0.1:")?;
    let (port_text, path_and_query) = authority_and_path.split_once('/')?;
    let port = port_text.parse::<u16>().ok()?;
    if port == 0 {
        return None;
    }

    // 3. 必须带非空 token；官方 Web UI 的访问授权依赖该查询参数。
    let token = path_and_query.strip_prefix("?token=")?;
    if token.is_empty() {
        return None;
    }

    Some((candidate.to_string(), port))
}

/// 把文本中第一个 `token=` 查询参数的值替换为 `<redacted>`。
///
/// 参数：
/// - `line`：可能包含访问 token 的原始日志行。
///
/// 返回值：脱敏后的新字符串；未出现 `token=` 时保持原内容。识别 `&`、空格、
/// `#` 和换行作为值结束符，避免把后续查询参数一并吞掉。
fn redact_token(line: &str) -> String {
    // 1. 找到 token 键；不存在则直接复制原行。
    let Some(key_start) = line.find("token=") else {
        return line.to_string();
    };

    // 2. 从等号后找到值的结束位置；即使值很长也只替换值本身。
    let value_start = key_start + "token=".len();
    let value_end = line[value_start..]
        .find(['&', ' ', '#', '\r', '\n'])
        .map(|offset| value_start + offset)
        .unwrap_or(line.len());

    // 3. 拼接前缀、占位符和值之后的剩余内容。
    format!(
        "{}token=<redacted>{}",
        &line[..key_start],
        &line[value_end..]
    )
}

/// 从有界日志尾部提取最近若干行。
///
/// 参数：
/// - `logs`：按时间顺序保存的运行日志。
/// - `limit`：最多返回的行数，至少为 1 时才有意义。
///
/// 返回值：保持原始时间顺序的最近日志快照。
fn tail_logs(logs: &VecDeque<String>, limit: usize) -> Vec<String> {
    // 1. 从尾部倒取 limit 行。
    // 2. 再反转回时间正序，便于前端直接展示。
    logs.iter().rev().take(limit).rev().cloned().collect()
}

/// 强制终止指定 PID 的整个进程树。
///
/// 参数：
/// - `pid`：DSH sidecar 根进程 PID；调用方必须确认该 PID 仍归本应用所有。
///
/// 副作用：Windows 调用 `taskkill /T /F`，Unix 先向进程组发 TERM，短暂等待
/// 后补 KILL。函数忽略回收失败，因为退出路径不能因清理错误再次崩溃。
fn terminate_process_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }

    #[cfg(unix)]
    {
        // 负 PID 表示整个进程组；spawn 时已通过 process_group(0) 建组。
        let process_group = format!("-{pid}");
        let _ = Command::new("kill")
            .args(["-TERM", &process_group])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        thread::sleep(Duration::from_millis(300));
        let _ = Command::new("kill")
            .args(["-KILL", &process_group])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证可选离线运行时目录能按约定解析出 Node 与 DSH 入口。
    #[test]
    fn resolves_bundled_runtime_layout() {
        // 1. 在临时目录构造 runtime/node 与 runtime/dsh/lib/bin.js。
        // 2. 断言来源为捆绑运行时且路径完全匹配。
        // 3. 清理临时目录。
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = env::temp_dir().join(format!("inkdsh-runtime-{unique}"));
        let node = root
            .join("node")
            .join(if cfg!(windows) { "node.exe" } else { "node" });
        let bin = root.join("dsh").join("lib").join("bin.js");
        fs::create_dir_all(node.parent().unwrap()).unwrap();
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&node, b"").unwrap();
        fs::write(&bin, b"").unwrap();

        let spec = bundled_launch_spec(&root).expect("bundled runtime should resolve");
        assert_eq!(spec.node, normalize_existing_path(&node));
        assert_eq!(spec.bin, normalize_existing_path(&bin));
        assert_eq!(spec.source, "捆绑运行时");

        let _ = fs::remove_dir_all(root);
    }

    /// 验证官方 stdout 就绪行能被完整解析为 URL 与端口。
    #[test]
    fn parses_official_ready_line() {
        // 1. 使用官方 e2e 测试中的真实格式。
        // 2. 断言 URL 保留 token，端口解析为数字。
        let line =
            "dsh web: http://127.0.0.1:57517/?token=OgERUpZz9jeAnCYj9KcNHErRlw7ZliZO8tt726opWDQ";
        assert_eq!(
            parse_ready_url(line),
            Some((
                "http://127.0.0.1:57517/?token=OgERUpZz9jeAnCYj9KcNHErRlw7ZliZO8tt726opWDQ"
                    .to_string(),
                57517
            ))
        );
    }

    /// 验证缺少 token、非回环主机和非法端口不会被误判为就绪。
    #[test]
    fn rejects_incomplete_or_unsafe_ready_lines() {
        // 1. 逐一构造缺少关键字段的行。
        // 2. 断言全部返回 None。
        for line in [
            "dsh web: http://127.0.0.1:57517/",
            "dsh web: http://127.0.0.1:57517/?token=",
            "dsh web: http://192.168.1.2:57517/?token=abc",
            "dsh web: http://127.0.0.1:not-a-port/?token=abc",
            "dsh web: http://127.0.0.1:0/?token=abc",
        ] {
            assert_eq!(parse_ready_url(line), None, "line={line}");
        }
    }

    /// 验证 netstat 监听记录能按端口解析出所属 PID。
    #[test]
    fn parses_windows_netstat_listener_owner() {
        // 1. 构造包含目标端口、无关端口和 IPv6 地址的 netstat 输出。
        // 2. 断言只返回目标端口对应的监听 PID。
        let output = r#"
  TCP    127.0.0.1:57517        0.0.0.0:0              LISTENING       1234
  TCP    127.0.0.1:3080         0.0.0.0:0              LISTENING       777
  TCP    [::1]:57517            [::]:0                 LISTENING       1234
  TCP    127.0.0.1:57518        127.0.0.1:60000        ESTABLISHED     9999
"#;
        assert_eq!(parse_netstat_owner(output, 57517), Some(1234));
        assert_eq!(parse_netstat_owner(output, 57518), None);
    }

    /// 验证日志脱敏只替换 token 值，不破坏周围文本。
    #[test]
    fn redacts_token_values() {
        // 1. 覆盖行尾 token 和带后续查询参数两种情况。
        // 2. 断言前缀、参数名和后续参数都保留。
        assert_eq!(
            redact_token("dsh web: http://127.0.0.1:1/?token=secret"),
            "dsh web: http://127.0.0.1:1/?token=<redacted>"
        );
        assert_eq!(
            redact_token("url?token=secret&x=1 done"),
            "url?token=<redacted>&x=1 done"
        );
    }
}
