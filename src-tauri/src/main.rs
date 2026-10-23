//! inkdsh 桌面入口：创建主窗口，异步拉起本地 DSH，并在就绪后加载官方 Web UI。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod interceptor;
mod preload;
mod runtime;
mod sidecar;

use std::sync::Arc;

use preload::PreloadState;
use sidecar::{SidecarManager, SidecarPaths};
use tauri::{Manager, RunEvent, State, WebviewWindowBuilder};

/// 解析应用数据目录。
///
/// 参数：
/// - `app`：Tauri 应用句柄；生产构建使用系统分配的 AppData 目录。
///
/// 返回值：数据目录绝对路径。调试构建允许通过 `INKDSH_DATA_DIR` 指向
/// `.temp` 下的隔离目录，避免测试写入真实 AppData；发布构建忽略该变量。
fn resolve_data_dir(app: &tauri::App) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    // 1. 仅调试构建接受隔离目录覆盖，防止发布包被环境变量改写数据位置。
    if cfg!(debug_assertions) {
        if let Some(dir) = std::env::var_os("INKDSH_DATA_DIR") {
            if !dir.is_empty() {
                return Ok(std::path::PathBuf::from(dir));
            }
        }
    }

    // 2. 默认使用 Tauri 计算的 AppData 目录。
    Ok(app.path().app_data_dir()?)
}

/// 在等待页上显示启动/导航错误。
///
/// 参数：
/// - `window`：仍停留在等待页的主窗口；页面已定义 `__inkdshShowError`。
/// - `message`：面向用户的错误摘要，不允许包含访问 token。
///
/// 副作用：执行一次 `window.eval`；失败时只写 stderr，不中断应用退出。
fn show_waiting_error(window: &tauri::WebviewWindow, message: &str) {
    // 1. 把错误安全编码为 JSON 字符串，避免引号破坏注入脚本。
    // 2. 调用等待页的可选错误展示函数。
    // 3. eval 失败说明窗口已销毁，只保留终端日志。
    let encoded = serde_json::to_string(message).unwrap_or_else(|_| "\"界面加载失败\"".to_string());
    if let Err(error) = window.eval(format!("window.__inkdshShowError?.({encoded});")) {
        eprintln!("[inkdsh] 无法在等待页显示错误：{error}");
    }
}

/// 在后台启动 sidecar，成功后把等待页导航到官方 Web UI。
///
/// 参数：
/// - `manager`：应用托管的 sidecar 管理器；启动函数会阻塞，必须放入阻塞线程池。
/// - `window`：当前等待页主窗口，成功后直接 `navigate` 到带 token 的地址。
/// - `preload`：P2.3 预载状态；sidecar 就绪后加载前端缓存并供拦截器命中。
///
/// 副作用：spawn 后台任务并拉起 DSH 子进程；成功后在导航前读取前端 dist 与
/// 会话缓存。失败状态由等待页轮询展示。
fn launch_sidecar(
    manager: Arc<SidecarManager>,
    window: tauri::WebviewWindow,
    preload: Arc<PreloadState>,
) {
    // 1. 在阻塞线程中执行同步的 sidecar 启动、就绪等待与前端预载。
    // 2. 成功时解析官方 URL 并导航当前 WebView。
    // 3. 解析或导航失败时把错误写回等待页；启动失败只打印终端，页面轮询状态。
    tauri::async_runtime::spawn(async move {
        let block_manager = Arc::clone(&manager);
        let block_preload = Arc::clone(&preload);
        let result = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
            let url = block_manager.start()?;
            if let Some(package_dir) = block_manager.selected_package_dir() {
                let dsh_home = preload::dsh_home();
                match block_preload.arm(&package_dir, &url, dsh_home.as_deref()) {
                    Ok(report) => block_manager.log_preload(&format!(
                        "前端预载完成：assets={} bytes={} sessions={} root={}",
                        report.assets,
                        report.bytes,
                        report.sessions,
                        report.root.display()
                    )),
                    Err(error) => {
                        block_manager.log_preload(&format!("前端预载失败：{error}"));
                    }
                }
            } else {
                block_manager.log_preload("未取得 DSH 包目录，跳过前端预载");
            }
            Ok(url)
        })
        .await;

        match result {
            Ok(Ok(url)) => match tauri::Url::parse(&url) {
                Ok(url) => {
                    if let Err(error) = window.navigate(url) {
                        show_waiting_error(&window, &format!("无法打开 DSH Web UI：{error}"));
                    }
                }
                Err(error) => {
                    show_waiting_error(&window, &format!("DSH 就绪地址无效：{error}"));
                }
            },
            Ok(Err(message)) => {
                eprintln!("[inkdsh] DSH sidecar 启动失败：{message}");
            }
            Err(error) => {
                eprintln!("[inkdsh] DSH 启动任务异常结束：{error}");
            }
        }
    });
}

/// 重新拉起失败的 sidecar。
///
/// 参数：
/// - `window`：调用命令的等待页窗口，重试成功后会在同一窗口导航。
/// - `state`：应用托管的 sidecar 管理器。
/// - `preload`：应用托管的预载状态，重试时复用同一缓存。
///
/// 返回值：状态不允许重试或应用正在退出时返回错误；成功提交重试后立即
/// 返回 `Ok(())`，实际启动在后台线程继续。
#[tauri::command]
fn sidecar_retry(
    window: tauri::WebviewWindow,
    state: State<'_, Arc<SidecarManager>>,
    preload: State<'_, Arc<PreloadState>>,
) -> Result<(), String> {
    // 1. 校验当前不处于 Starting/Ready，并复位一次性启动标记。
    // 2. 复用与首次启动相同的后台启动和导航流程。
    state.prepare_retry()?;
    launch_sidecar(
        Arc::clone(state.inner()),
        window,
        Arc::clone(preload.inner()),
    );
    Ok(())
}

/// 构建并运行 Tauri 应用。
fn main() {
    // 1. 先注册单实例插件，第二次启动时只唤起已有主窗口。
    // 2. 注册 sidecar 命令并在 setup 中准备应用数据目录、日志与 PID 路径。
    // 3. 主窗口先加载等待页，后台拉起 sidecar，就绪后导航到官方 Web UI。
    // 4. 窗口销毁或应用退出时统一回收 sidecar 进程树。
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .invoke_handler(tauri::generate_handler![
            sidecar::sidecar_status,
            sidecar::sidecar_url,
            sidecar::runtime_status,
            sidecar_retry
        ])
        .setup(|app| {
            // 1. 先确定数据目录：调试构建可用 INKDSH_DATA_DIR 指向隔离目录。
            let data_dir = resolve_data_dir(app)?;
            let log_dir = data_dir.join("logs");
            std::fs::create_dir_all(&log_dir)?;

            // 2. 托管运行时固定在数据目录下，捆绑运行时由安装资源定位；
            //    crate 目录只用于开发构建回退查找仓库内清单/运行时。
            let manager = Arc::new(SidecarManager::new(SidecarPaths {
                log: log_dir.join("sidecar.log"),
                pid_marker: data_dir.join("sidecar.pid"),
                data_dir: data_dir.clone(),
                resource_dir: app.path().resource_dir().ok(),
                // 只有调试构建才回退到编译期源码目录；发布包不能探测开发机路径。
                crate_dir: if cfg!(debug_assertions) {
                    Some(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")))
                } else {
                    None
                },
            }));
            eprintln!("[inkdsh] sidecar 日志目录：{}", log_dir.display());
            app.manage(Arc::clone(&manager));

            // 3. P2.3 预载状态：日志落在应用自己的日志目录，不写 DSH_HOME。
            let preload_state = Arc::new(PreloadState::new(log_dir.join("preload.log")));
            app.manage(Arc::clone(&preload_state));

            let Some(window_config) = app
                .config()
                .app
                .windows
                .iter()
                .find(|window| window.label == "main")
                .cloned()
            else {
                return Err("tauri.conf.json 缺少 label=main 的窗口配置".into());
            };

            // 4. 页面加载完成后补一次 BOOT 注入；index 拦截成功后该操作幂等。
            let page_state = Arc::clone(&preload_state);
            let window = WebviewWindowBuilder::from_config(app, &window_config)?
                .on_page_load(move |window, payload| {
                    if payload.event() == tauri::webview::PageLoadEvent::Finished {
                        page_state.inject_boot(&window);
                    }
                })
                .build()?;

            let close_manager = Arc::clone(&manager);
            window.on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::Destroyed) {
                    close_manager.shutdown();
                }
            });

            // 5. 在导航到 DSH 之前安装请求拦截器；失败只降级，不阻断启动。
            if let Err(error) = interceptor::install(&window, Arc::clone(&preload_state)) {
                eprintln!("[inkdsh] 预载拦截器安装失败：{error}");
                manager.log_preload(&format!("拦截器安装失败：{error}"));
            }

            launch_sidecar(manager, window, preload_state);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("inkdsh 构建 Tauri 应用失败");

    app.run(|app_handle, event| {
        // 1. ExitRequested 与 Exit 都尝试清理，shutdown 自身幂等。
        // 2. 通过托管状态取管理器，避免在事件回调里重新创建实例。
        if matches!(event, RunEvent::ExitRequested { .. } | RunEvent::Exit) {
            if let Some(manager) = app_handle.try_state::<Arc<SidecarManager>>() {
                manager.shutdown();
            }
        }
    });
}
