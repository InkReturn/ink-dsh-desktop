//! inkdsh 桌面入口：创建主窗口，异步拉起本地 DSH，并在就绪后加载官方 Web UI。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod sidecar;

use std::sync::Arc;

use sidecar::SidecarManager;
use tauri::{Manager, RunEvent, State, WebviewWindowBuilder};

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
///
/// 副作用：spawn 后台任务并拉起 DSH 子进程；失败状态由等待页轮询展示。
fn launch_sidecar(manager: Arc<SidecarManager>, window: tauri::WebviewWindow) {
    // 1. 在阻塞线程中执行同步的 sidecar 启动和就绪等待。
    // 2. 成功时解析官方 URL 并导航当前 WebView。
    // 3. 解析或导航失败时把错误写回等待页；启动失败只打印终端，页面轮询状态。
    tauri::async_runtime::spawn(async move {
        let result = tauri::async_runtime::spawn_blocking(move || manager.start()).await;

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
///
/// 返回值：状态不允许重试或应用正在退出时返回错误；成功提交重试后立即
/// 返回 `Ok(())`，实际启动在后台线程继续。
#[tauri::command]
fn sidecar_retry(
    window: tauri::WebviewWindow,
    state: State<'_, Arc<SidecarManager>>,
) -> Result<(), String> {
    // 1. 校验当前不处于 Starting/Ready，并复位一次性启动标记。
    // 2. 复用与首次启动相同的后台启动和导航流程。
    state.prepare_retry()?;
    launch_sidecar(Arc::clone(state.inner()), window);
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
            sidecar_retry
        ])
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let log_dir = data_dir.join("logs");
            std::fs::create_dir_all(&log_dir)?;
            let bundled_runtime_root = std::env::var_os("INKDSH_RUNTIME_DIR")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    app.path()
                        .resource_dir()
                        .ok()
                        .map(|dir| dir.join("runtime"))
                });
            let manager = Arc::new(SidecarManager::new(
                log_dir.join("sidecar.log"),
                data_dir.join("sidecar.pid"),
                bundled_runtime_root,
            ));
            eprintln!("[inkdsh] sidecar 日志目录：{}", log_dir.display());
            app.manage(Arc::clone(&manager));

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
            let window = WebviewWindowBuilder::from_config(app, &window_config)?.build()?;

            let close_manager = Arc::clone(&manager);
            window.on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::Destroyed) {
                    close_manager.shutdown();
                }
            });

            launch_sidecar(manager, window);
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
