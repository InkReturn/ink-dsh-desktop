//! DSH sidecar 的发现、启动、状态查询与进程树回收。
//!
//! 本模块负责运行时的最终选择与本地 `dsh web` 进程：先由 `runtime` 模块
//! 解析来源、校验版本并按需装配托管运行时，再选择回环端口、捕获输出、解析
//! 官方就绪地址，并保证应用退出时回收整个子进程树。

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

use crate::runtime::{
    manifest::ENV_MANIFEST, BootstrapState, CoreCandidate, CoreSource, InstallStage,
    LoadedManifest, RejectedCandidate, Resolution, RuntimeEnvironment, RuntimeStatus,
};

/// 官方 `dsh web` 输出就绪地址时使用的前缀。
const READY_PREFIX: &str = "dsh web: ";

/// sidecar 首轮启动等待上限。
///
/// 空 `DSH_HOME` 首次启动时，DSH 需要初始化 profile、凭证占位与工作区记录；
/// 本机实测冷启动约 4 分钟（受杀毒扫描与网络探测影响），因此上限放宽到
/// 5 分钟。已有 profile 的后续启动通常在数秒内就绪。
const STARTUP_TIMEOUT: Duration = Duration::from_secs(300);

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
    /// 运行时来源，用于日志与诊断。
    source: CoreSource,
    /// 归一化后的 DSH 版本，用于启动日志。
    version: Option<String>,
}

impl LaunchSpec {
    /// 把来源解析结果转换为启动规格。
    ///
    /// 参数：
    /// - `candidate`：`runtime` 模块选中的候选，已通过版本校验。
    ///
    /// 返回值：可直接交给 [`build_sidecar_command`] 的启动参数。
    fn from_candidate(candidate: &CoreCandidate) -> Self {
        // 1. 只搬运启动必需字段，避免后面误用候选的文件系统信息。
        Self {
            node: candidate.node.clone(),
            bin: candidate.bin.clone(),
            source: candidate.source,
            version: candidate.version.as_ref().map(ToString::to_string),
        }
    }

    /// 生成日志中的来源描述。
    ///
    /// 返回值：`来源（版本）`，版本未知时只写来源。
    fn describe(&self) -> String {
        // 1. 优先写规范化版本，保持与清单一致的写法。
        match &self.version {
            Some(version) => format!("{}（{version}）", self.source.label()),
            None => self.source.label().to_string(),
        }
    }
}

/// sidecar 需要的一组应用路径。
#[derive(Debug, Clone)]
pub struct SidecarPaths {
    /// sidecar 诊断日志文件。
    pub log: PathBuf,
    /// 崩溃后用于清扫孤儿的 PID/端口标记。
    pub pid_marker: PathBuf,
    /// 应用数据目录；托管运行时固定在其下。
    pub data_dir: PathBuf,
    /// Tauri 安装资源根；用于定位捆绑运行时与清单。
    pub resource_dir: Option<PathBuf>,
    /// 编译期 `src-tauri` 目录；仅开发构建用于定位仓库内清单/运行时。
    pub crate_dir: Option<PathBuf>,
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
    /// 首启装配状态机快照。
    bootstrap: Mutex<BootstrapState>,
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
    /// 应用数据目录；托管运行时根目录由它派生。
    data_dir: PathBuf,
    /// 安装资源根；用于定位捆绑运行时与清单。
    resource_dir: Option<PathBuf>,
    /// 编译期源码目录；仅开发构建用于回退查找仓库内资源。
    crate_dir: Option<PathBuf>,
    /// 本次选中 DSH 的包目录；P2.3 据此定位 `dsh-web-frontend`。
    selected_package_dir: Mutex<Option<PathBuf>>,
    /// Windows Job Object：应用进程崩溃时由系统杀死整棵 sidecar 进程树。
    #[cfg(windows)]
    process_job: Mutex<Option<ProcessJob>>,
}

impl SidecarManager {
    /// 创建一个尚未启动的管理器。
    ///
    /// 参数：
    /// - `paths`：日志、PID 标记、应用数据、安装资源与源码目录的集合；
    ///   托管运行时由 `data_dir` 派生，测试可整体指向临时目录。
    ///
    /// 返回值初始状态为 `Starting`，但不会拉起任何进程；真正的启动由
    /// [`SidecarManager::start`] 完成。
    pub fn new(paths: SidecarPaths) -> Self {
        // 1. 先拆出需要长期保存的路径，避免结构体字段遗漏。
        let SidecarPaths {
            log,
            pid_marker,
            data_dir,
            resource_dir,
            crate_dir,
        } = paths;

        // 2. 尽量创建日志和标记的父目录，失败留给首次写入时报错并记录。
        // 3. 日志超过大小上限时轮转一次，避免长期运行无限增长。
        // 4. 初始化空状态、装配状态机与两个原子标记。
        if let Some(parent) = log.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Some(parent) = pid_marker.parent() {
            let _ = fs::create_dir_all(parent);
        }
        rotate_log_if_needed(&log);
        Self {
            inner: Mutex::new(RuntimeState {
                status: SidecarStatus::Starting,
                url: None,
                pid: None,
                logs: VecDeque::new(),
            }),
            bootstrap: Mutex::new(BootstrapState::default()),
            started: AtomicBool::new(false),
            shutdown_requested: AtomicBool::new(false),
            log_path: log,
            pid_path: pid_marker,
            clock: Instant::now(),
            data_dir,
            resource_dir,
            crate_dir,
            selected_package_dir: Mutex::new(None),
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

        // 3. 先清扫上次崩溃留下的 sidecar，再按来源优先级解析运行时；
        //    托管运行时缺失且清单可安装时，会在这一步完成装配。
        self.sweep_orphan();
        let candidate = match self.resolve_runtime() {
            Ok(candidate) => candidate,
            Err(message) => {
                self.fail_startup(message.clone());
                return Err(message);
            }
        };
        let launch = LaunchSpec::from_candidate(&candidate);
        self.set_selected_package_dir(candidate.package_dir.clone());

        // 3.1 装配期间用户可能已关闭窗口；此时不再 spawn，避免退出前留下进程。
        if self.shutdown_requested.load(Ordering::SeqCst) {
            return Err("应用正在退出，已取消启动 DSH sidecar".to_string());
        }

        // 4. 记录 DSH_HOME 继承情况（只读，不创建目录），并选择回环端口。
        self.record_dsh_home();
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
                launch.describe(),
                launch.node.display(),
                launch.bin.display()
            ),
        );

        // 5. 通过 node 启动 `dsh web`，并注入稳定的无颜色、无遥测环境。
        let mut command = build_sidecar_command(&launch, port);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let message = format!("无法启动 DSH sidecar（请确认 node 在 PATH 中）：{error}");
                self.fail_startup(message.clone());
                return Err(message);
            }
        };

        // 6. 立即登记 PID 和标记；即使应用在就绪前退出，也能按 PID 回收进程树。
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

        // 7. 取出输出管道；任何一步失败都要先杀掉已启动的进程。
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

        // 8. 启动独立读取线程，避免 stdout/stderr 任一管道写满导致子进程阻塞。
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

        // 9. 等待官方就绪行；成功后再次确认应用未退出，再发布 Ready 状态。
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
        drop(inner);

        // 4. 装配状态回到探测，让等待页在重试时立即看到新一轮进度。
        self.bootstrap
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .detect();
        Ok(())
    }

    /// 读取首启装配状态快照。
    ///
    /// 返回值：`detect` / `installing` / `ready` / `failed` 之一，供等待页
    /// 展示装配阶段、错误与重试入口。
    pub fn runtime_snapshot(&self) -> RuntimeStatus {
        // 1. 锁只覆盖克隆，避免把内部锁暴露给调用方。
        self.bootstrap
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .status()
    }

    /// 记录本次选中的 DSH 包目录。
    ///
    /// 参数：
    /// - `path`：`@deepseek-ai/dsh` 包根目录，供 P2.3 定位前端 dist。
    fn set_selected_package_dir(&self, path: PathBuf) {
        // 1. 只保存路径，不读取或写入包内容。
        *self
            .selected_package_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(path);
    }

    /// 返回本次选中的 DSH 包目录。
    ///
    /// 返回值：sidecar 尚未解析来源时为 `None`。
    pub fn selected_package_dir(&self) -> Option<PathBuf> {
        // 1. 克隆路径，避免调用方长期持有锁。
        self.selected_package_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// 写一行预载日志到 sidecar 日志，便于与启动流程一起排查。
    ///
    /// 参数：
    /// - `line`：已脱敏的预载诊断文本。
    pub fn log_preload(&self, line: &str) {
        // 1. 复用统一的日志脱敏与落盘逻辑。
        self.record_log("preload", line);
    }

    /// 解析来源，并在需要时装配托管运行时。
    ///
    /// 返回值：通过版本校验、可直接启动的候选；全部来源失败时返回用户可见
    /// 的错误摘要，同时把逐来源原因写入装配状态与日志。
    ///
    /// 副作用：读取清单与文件系统；托管安装时写 staging、替换 AppData 下的
    /// `runtime/`；不会写用户 `DSH_HOME`。
    fn resolve_runtime(&self) -> Result<CoreCandidate, String> {
        // 1. 加载运行时清单并记录诊断（清单缺失不阻断，只关闭最低版本校验）。
        //    显式路径仅调试构建可用，防止发布包被环境变量改写最低版本基线。
        let explicit_manifest = if cfg!(debug_assertions) {
            env::var_os(ENV_MANIFEST).map(PathBuf::from)
        } else {
            None
        };
        let manifest = LoadedManifest::load(
            explicit_manifest.as_deref(),
            self.resource_dir.as_deref(),
            self.crate_dir.as_deref(),
        );
        for line in &manifest.diagnostics {
            self.record_log("runtime", line);
        }
        self.record_log("runtime", &manifest.summary());

        // 2. 构造来源环境，并先处理上次崩溃留下的中断切换。
        let environment = RuntimeEnvironment::discover(
            &self.data_dir,
            self.resource_dir.as_deref(),
            self.crate_dir.as_deref(),
            &manifest,
        );
        for line in &environment.diagnostics {
            self.record_log("runtime", line);
        }
        if let Some(message) = environment.managed.recover_interrupted_activation() {
            self.record_log("runtime", &message);
        }

        // 3. 解析 → 必要时装配 → 装配失败后继续回退，直到成功或全部失败。
        let mut resolution = environment.resolve();
        let mut install_error: Option<String> = None;
        loop {
            match resolution {
                Resolution::Use {
                    candidate,
                    rejected: prior,
                    diagnostics,
                } => {
                    self.log_resolution(&prior, &diagnostics);
                    self.record_log(
                        "runtime",
                        &format!(
                            "运行时来源确定：{}，版本 {}，node={}，bin={}",
                            candidate.source.label(),
                            candidate.version_label(),
                            candidate.node.display(),
                            candidate.bin.display()
                        ),
                    );
                    self.set_runtime_ready(
                        candidate.source,
                        candidate.version.as_ref().map(ToString::to_string),
                    );
                    return Ok(candidate);
                }
                Resolution::InstallManaged {
                    artifact,
                    rejected: prior,
                    diagnostics,
                } => {
                    self.log_resolution(&prior, &diagnostics);
                    let mut rejected = prior;
                    self.record_log("runtime", "开始装配托管运行时（Managed）");
                    self.record_log(
                        "runtime",
                        &format!("托管运行时目录：{}", environment.managed.root().display()),
                    );
                    self.set_runtime_stage(InstallStage::Verify, "准备装配运行时".to_string());

                    // 3.1 安装器的阶段回调同时更新状态机与日志，等待页因此能
                    //     看到「校验 → 解压 → 启用」。
                    let mut progress = |stage: InstallStage, detail: String| {
                        self.set_runtime_stage(stage, detail.clone());
                        self.record_log("runtime", &format!("[{}] {detail}", stage.label()));
                    };
                    match environment.managed.install(
                        &artifact,
                        environment.minimum.as_ref(),
                        &mut progress,
                    ) {
                        Ok(candidate) => {
                            self.record_log(
                                "runtime",
                                &format!(
                                    "托管运行时装配完成：版本 {}，node={}，bin={}",
                                    candidate.version_label(),
                                    candidate.node.display(),
                                    candidate.bin.display()
                                ),
                            );
                            self.set_runtime_ready(
                                candidate.source,
                                candidate.version.as_ref().map(ToString::to_string),
                            );
                            return Ok(candidate);
                        }
                        Err(error) => {
                            // 3.2 装配失败只记录并回退到更低优先级来源；全部
                            //     失败时才把该错误作为用户可见摘要。
                            let detail = error.detail();
                            self.record_log(
                                "runtime",
                                &format!("托管运行时装配失败（{}）：{detail}", error.code()),
                            );
                            rejected.push(RejectedCandidate {
                                source: CoreSource::Managed,
                                detail: format!("{}: {detail}", error.code()),
                            });
                            install_error = Some(error.user_message());
                            resolution =
                                environment.resolve_after_managed_failure(rejected.clone());
                        }
                    }
                }
                Resolution::Unavailable {
                    reasons,
                    diagnostics,
                } => {
                    // 4. 全部来源不可用：逐条落日志，并把原因交给等待页。
                    for line in &diagnostics {
                        self.record_log("runtime", line);
                    }
                    for reason in &reasons {
                        self.record_log("runtime", &format!("来源不可用：{reason}"));
                    }
                    let message = install_error
                        .clone()
                        .or_else(|| {
                            // 单一硬失败（例如 DSH_BIN 无效）直接把具体原因作为标题。
                            (reasons.len() == 1).then(|| reasons[0].clone())
                        })
                        .unwrap_or_else(|| "未找到可用的 DSH 运行时".to_string());
                    self.set_runtime_failed(
                        message.clone(),
                        "点击重试可重新装配；也可查看日志中的逐来源原因".to_string(),
                        reasons,
                    );
                    return Err(message);
                }
            }
        }
    }

    /// 记录一轮解析中的回退与诊断。
    ///
    /// 参数：
    /// - `rejected`：优先级更高但被拒绝的候选。
    /// - `diagnostics`：解析器附加的环境诊断。
    fn log_resolution(&self, rejected: &[RejectedCandidate], diagnostics: &[String]) {
        // 1. 先写环境诊断，再写逐来源回退，保持时间顺序可读。
        for line in diagnostics {
            self.record_log("runtime", line);
        }
        for item in rejected {
            self.record_log("runtime", &format!("来源回退：{}", item.describe()));
        }
    }

    /// 记录子进程将继承的 DSH_HOME 来源（只读，不创建目录）。
    ///
    /// 副作用：只写日志。生产环境不设置也不改动 `DSH_HOME`，用户数据目录
    /// 仍由 DSH 自身决定；测试通过外部环境变量注入隔离目录。
    fn record_dsh_home(&self) {
        // 1. 有显式环境变量时记录路径；没有时说明会使用 DSH 默认目录。
        match env::var_os("DSH_HOME") {
            Some(value) => self.record_log(
                "runtime",
                &format!(
                    "DSH_HOME 由环境变量指定：{}",
                    PathBuf::from(value).display()
                ),
            ),
            None => self.record_log("runtime", "DSH_HOME 未设置，子进程使用 DSH 默认用户目录"),
        }
    }

    /// 更新装配阶段状态。
    ///
    /// 参数：
    /// - `stage`：当前阶段。
    /// - `detail`：阶段细节，供等待页展示。
    fn set_runtime_stage(&self, stage: InstallStage, detail: String) {
        // 1. 状态机只允许在探测/装配阶段更新，其它阶段保持现状。
        self.bootstrap
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .begin_install(CoreSource::Managed, stage, detail);
    }

    /// 把装配状态置为就绪。
    ///
    /// 参数：
    /// - `source`：最终选中的来源。
    /// - `version`：DSH 版本；读取失败时为 `None`。
    fn set_runtime_ready(&self, source: CoreSource, version: Option<String>) {
        // 1. 就绪后等待页改为等待 sidecar 状态，不再显示装配进度。
        self.bootstrap
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .ready(source, version);
    }

    /// 把装配状态置为失败。
    ///
    /// 参数：
    /// - `message`：用户可见的一句话。
    /// - `hint`：修复建议。
    /// - `reasons`：逐来源原因。
    fn set_runtime_failed(&self, message: String, hint: String, reasons: Vec<String>) {
        // 1. 失败状态允许重试；重试入口由 sidecar_retry 触发。
        self.bootstrap
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .fail(message, Some(hint), reasons);
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

/// 查询首启运行时装配状态。
///
/// 参数：
/// - `state`：Tauri 注入的管理器状态；应用启动时已注册，不会为空。
///
/// 返回值：装配阶段快照；等待页据此展示进度、错误与重试入口。
#[tauri::command]
pub fn runtime_status(state: State<'_, Arc<SidecarManager>>) -> RuntimeStatus {
    // 1. 只读克隆状态，不把内部锁暴露给前端。
    state.runtime_snapshot()
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
