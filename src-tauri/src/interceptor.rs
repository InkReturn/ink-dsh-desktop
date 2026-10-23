//! P2.3 WebView2 请求拦截。
//!
//! 通过 `ICoreWebView2::AddWebResourceRequestedFilter` + 事件回调，把命中
//! `preload::PreloadState` 的静态资源直接从内存写回，不再发往本地 HTTP 服务。
//! 未命中（例如 API、WebSocket 之外的动态请求）保持原样，由 DSH 服务处理。

use std::sync::Arc;

use tauri::WebviewWindow;

use crate::preload::PreloadState;

/// 需要拦截的 URL 模式：静态资源 + 仅文档导航。
pub const RESOURCE_FILTER_PATTERNS: [&str; 2] =
    ["http://127.0.0.1:*/assets/*", "http://127.0.0.1:*/*"];

/// 安装主窗口的请求拦截器。
///
/// 参数：
/// - `window`：已经创建、尚未导航到 DSH Web UI 的主窗口。
/// - `state`：预载缓存与日志；回调持有其 `Arc`。
///
/// 返回值：COM 安装成功返回 `Ok`；WebView2 不可用或接口版本过低时返回错误，
/// 调用方记录日志并继续使用服务端资源（不阻断启动）。
///
/// 副作用：在主窗口 WebView2 上注册资源过滤器与请求回调。
pub fn install(window: &WebviewWindow, state: Arc<PreloadState>) -> Result<(), String> {
    // 1. Windows 走 COM；其它平台没有 WebView2，直接跳过。
    #[cfg(windows)]
    {
        imp::install(window, state)
    }
    #[cfg(not(windows))]
    {
        let _ = (window, state);
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use std::{
        ffi::c_void,
        sync::{mpsc, Arc},
        time::Duration,
    };

    use tauri::{webview::PlatformWebview, WebviewWindow};
    use webview2_com::{
        take_pwstr,
        Microsoft::Web::WebView2::Win32::{
            ICoreWebView2WebResourceResponse, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT,
        },
        WebResourceRequestedEventHandler,
    };
    use windows_core::{Interface, HSTRING, PCWSTR, PWSTR};
    use windows_sys::Win32::UI::Shell::SHCreateMemStream;

    use super::{PreloadState, RESOURCE_FILTER_PATTERNS};

    /// 在主线程安装 COM 回调，并把结果同步回调用方。
    ///
    /// 参数：
    /// - `window`：主窗口。
    /// - `state`：预载状态。
    ///
    /// 返回值：安装结果；5 秒内没有回调视为失败。
    pub fn install(window: &WebviewWindow, state: Arc<PreloadState>) -> Result<(), String> {
        // 1. with_webview 的回调在 WebView 线程执行，用 channel 取回结果。
        let (sender, receiver) = mpsc::channel();
        window
            .with_webview(move |platform| {
                let result = unsafe { install_inner(&platform, state) };
                let _ = sender.send(result);
            })
            .map_err(|error| format!("PRELOAD_INSTALL_DISPATCH: {error}"))?;

        // 2. 等待安装结果；超时视为失败，不影响应用继续启动。
        receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| format!("PRELOAD_INSTALL_TIMEOUT: {error}"))?
    }

    /// 实际注册过滤器与请求回调。
    ///
    /// 参数：
    /// - `platform`：Tauri 暴露的 WebView2 平台句柄。
    /// - `state`：预载状态；回调中按 URL 命中内存缓存。
    ///
    /// 返回值：任何 COM 调用失败都会返回可读错误。
    unsafe fn install_inner(
        platform: &PlatformWebview,
        state: Arc<PreloadState>,
    ) -> Result<(), String> {
        // 1. 取 CoreWebView2 与环境；环境用于构造内存响应。
        let webview = platform
            .controller()
            .CoreWebView2()
            .map_err(|error| format!("PRELOAD_CORE: {error}"))?;
        let environment = platform.environment();

        // 2. 注册静态资源与文档两类过滤器。
        for (pattern, context) in [
            (
                RESOURCE_FILTER_PATTERNS[0],
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            ),
            (
                RESOURCE_FILTER_PATTERNS[1],
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT,
            ),
        ] {
            let pattern = HSTRING::from(pattern);
            webview
                .AddWebResourceRequestedFilter(PCWSTR::from_raw(pattern.as_ptr()), context)
                .map_err(|error| format!("PRELOAD_FILTER: {error}"))?;
        }

        // 3. 请求回调：只处理 GET/HEAD，命中即返回内存响应。
        let mut token = 0;
        webview
            .add_WebResourceRequested(
                &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let request = args.Request()?;

                    let mut uri = PWSTR::null();
                    request.Uri(&mut uri)?;
                    let uri = take_pwstr(uri);
                    let Ok(url) = tauri::Url::parse(&uri) else {
                        return Ok(());
                    };

                    let mut method = PWSTR::null();
                    request.Method(&mut method)?;
                    let method = take_pwstr(method);
                    if method != "GET" && method != "HEAD" {
                        return Ok(());
                    }

                    // 3.1 只处理本机、已启动端口且被预载的 path。
                    let path = url.path().to_string();
                    let Some(hit) = state.lookup(
                        &path,
                        url.query(),
                        url.host_str(),
                        url.port_or_known_default(),
                    ) else {
                        return Ok(());
                    };
                    state.record_served(&path, hit.body.len());

                    // 3.2 构造内存流；HEAD 或空响应不创建流。
                    let length = u32::try_from(hit.body.len()).unwrap_or(0);
                    let stream = if method == "HEAD" || hit.body.is_empty() {
                        std::ptr::null_mut()
                    } else {
                        SHCreateMemStream(hit.body.as_ptr(), length)
                    };
                    if stream.is_null() && method != "HEAD" && !hit.body.is_empty() {
                        return Ok(());
                    }

                    // 3.3 交给 WebView2 生成响应；关闭本地缓存以匹配开发期热更新。
                    let headers = HSTRING::from(format!(
                        "Content-Type: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n",
                        hit.mime
                    ));
                    let reason = HSTRING::from("OK");
                    let mut response: *mut c_void = std::ptr::null_mut();
                    let status = (environment.vtable().CreateWebResourceResponse)(
                        environment.as_raw(),
                        stream,
                        200,
                        PCWSTR(reason.as_ptr()),
                        PCWSTR(headers.as_ptr()),
                        &mut response,
                    );
                    if !stream.is_null() {
                        drop(windows_core::IUnknown::from_raw(stream));
                    }
                    status.ok()?;
                    args.SetResponse(&ICoreWebView2WebResourceResponse::from_raw(response))?;
                    Ok(())
                })),
                &mut token,
            )
            .map_err(|error| format!("PRELOAD_HANDLER: {error}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证过滤器覆盖静态资源与文档导航两类请求。
    #[test]
    fn filters_cover_assets_and_documents() {
        // 1. 防止误删 assets 过滤或改成任意 host。
        assert_eq!(
            RESOURCE_FILTER_PATTERNS,
            ["http://127.0.0.1:*/assets/*", "http://127.0.0.1:*/*"]
        );
    }
}
