//! P2.3 资源预载与 BOOT 注入。
//!
//! 启动后读取当前 DSH 安装的 Web 前端 `dist/`，把 `index.html` 与
//! `assets/*` 缓存在内存中；`interceptor` 在 WebView2 请求阶段直接返回这些
//! 字节，避免页面二次从本地 HTTP 服务读取静态资源。同时读取
//! `DSH_HOME/storages/session_projcache/sessions/*.json` 的会话元数据，注入
//! `window.__INKDSH_PRELOADED_SESSIONS__`，供 client 插件首次渲染使用。
//!
//! 本模块只读文件系统与网络无关；写操作仅限应用自己的预载日志。

use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, RwLock,
    },
    time::UNIX_EPOCH,
};

use serde::Serialize;
use tauri::WebviewWindow;

/// 前端静态资源总量上限（256 MiB），避免异常 dist 撑爆内存。
const MAX_STATIC_BYTES: u64 = 256 * 1024 * 1024;

/// 会话预载最多读取的文件数。
const MAX_SESSIONS: usize = 20;

/// 单个会话投影缓存文件大小上限；超过的条目跳过。
const MAX_SESSION_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// 一个可直接返回给 WebView2 的静态资源。
#[derive(Debug, Clone)]
pub struct CachedResponse {
    /// 文件字节。
    pub body: Vec<u8>,
    /// HTTP Content-Type。
    pub mime: &'static str,
}

/// 已加载的前端缓存。
#[derive(Debug)]
struct FrontendCache {
    /// 允许命中的 origin（仅 http://127.0.0.1:<port>）。
    origin: String,
    /// 允许命中的端口。
    port: u16,
    /// 已注入 BOOT 脚本的 index.html。
    index: Vec<u8>,
    /// 以 URL path 为 key 的静态资源（`/assets/...` 与根目录图标）。
    assets: HashMap<String, CachedResponse>,
}

/// 预载阶段报告，用于日志与测试断言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreloadReport {
    /// 缓存的静态资源数量。
    pub assets: usize,
    /// 静态资源总字节数。
    pub bytes: u64,
    /// 注入 BOOT 的会话数量。
    pub sessions: usize,
    /// 命中的 dist 根目录。
    pub root: PathBuf,
    /// 允许命中的 origin。
    pub origin: String,
}

/// WebView2 请求拦截与 BOOT 注入共享的预载状态。
#[derive(Debug)]
pub struct PreloadState {
    /// 最近一次成功加载的前端缓存。
    frontend: RwLock<Option<FrontendCache>>,
    /// BOOT 注入使用的会话 JSON；为空表示没有会话元数据。
    boot_json: RwLock<Option<String>>,
    /// 预载诊断日志路径。
    log_path: PathBuf,
    /// 串行化日志写入。
    log_lock: Mutex<()>,
    /// 已由内存返回的请求数。
    served: AtomicU64,
}

impl PreloadState {
    /// 创建预载状态并重置本轮日志。
    ///
    /// 参数：
    /// - `log_path`：应用日志目录下的 `preload.log`。
    ///
    /// 副作用：创建父目录并截断旧日志；读取失败只忽略。
    pub fn new(log_path: PathBuf) -> Self {
        // 1. 确保日志目录存在，并清空上一轮内容（单实例应用）。
        if let Some(parent) = log_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&log_path, b"");
        Self {
            frontend: RwLock::new(None),
            boot_json: RwLock::new(None),
            log_path,
            log_lock: Mutex::new(()),
            served: AtomicU64::new(0),
        }
    }

    /// 记录一行预载日志。
    ///
    /// 参数：
    /// - `line`：不包含凭据的诊断文本。
    ///
    /// 副作用：追加写 `preload.log`；失败只忽略，不影响页面加载。
    pub fn log(&self, line: &str) {
        // 1. 串行化写入，避免多线程交错。
        let _guard = self
            .log_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    /// 尝试命中内存缓存。
    ///
    /// 参数：
    /// - `path`：请求 URL 的 path，例如 `/assets/index.js`。
    /// - `query`：原始 query（不含 `?`）。
    /// - `host`：请求 Host。
    /// - `port`：请求端口。
    ///
    /// 返回值：命中时返回可写入响应的字节与 MIME；未命中返回 `None`。
    /// 只有 `127.0.0.1:<已启动端口>` 会被处理，避免误伤其他本地服务。
    pub fn lookup(
        &self,
        path: &str,
        query: Option<&str>,
        host: Option<&str>,
        port: Option<u16>,
    ) -> Option<CachedResponse> {
        // 1. 快速淘汰非本机与未加载场景。
        if host != Some("127.0.0.1") {
            return None;
        }
        let cache = self
            .frontend
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let cache = cache.as_ref()?;
        if port != Some(cache.port) {
            return None;
        }

        // 2. 文档请求只接受带 token 的导航地址，避免拦截其他根路径请求。
        if path == "/" || path == "/index.html" {
            let has_token = query
                .map(|query| query.split('&').any(|pair| pair.starts_with("token=")))
                .unwrap_or(false);
            if !has_token {
                return None;
            }
            return Some(CachedResponse {
                body: cache.index.clone(),
                mime: "text/html; charset=utf-8",
            });
        }

        // 3. 其余只匹配已缓存的静态资源。
        cache.assets.get(path).cloned()
    }

    /// 在页面加载完成后补一次 BOOT 注入。
    ///
    /// 参数：
    /// - `window`：主窗口；等待页等非 DSH 页面会被跳过。
    ///
    /// 副作用：执行一次 `window.eval`；失败只写预载日志。
    pub fn inject_boot(&self, window: &WebviewWindow) {
        // 1. 只在 DSH 端口页面注入；waiting.html 等页面跳过。
        let Ok(url) = window.url() else {
            return;
        };
        let host = url.host_str();
        let port = url.port_or_known_default();
        {
            let cache = self
                .frontend
                .read()
                .unwrap_or_else(|error| error.into_inner());
            let Some(cache) = cache.as_ref() else {
                return;
            };
            if host != Some("127.0.0.1") || port != Some(cache.port) {
                return;
            }
        }

        // 2. 读取 BOOT JSON；没有数据时也注入空数组，保证全局变量存在。
        let json = self
            .boot_json
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .unwrap_or_else(|| "[]".to_string());
        let script = format!(
            "(() => {{ const list = {json}; if (!Array.isArray(window.__INKDSH_PRELOADED_SESSIONS__)) \
             window.__INKDSH_PRELOADED_SESSIONS__ = list; \
             window.dispatchEvent(new Event('inkdsh:preloaded-sessions')); }})();"
        );
        if let Err(error) = window.eval(script) {
            self.log(&format!("[boot] 注入失败：{error}"));
        }
    }

    /// 安装前端缓存与 BOOT 数据。
    ///
    /// 参数：
    /// - `dsh_package_dir`：已选中的 `@deepseek-ai/dsh` 包目录。
    /// - `dsh_url`：sidecar 就绪地址（用于校验 origin/端口）。
    /// - `dsh_home`：DSH 用户目录；用于读取会话投影缓存。
    ///
    /// 返回值：加载成功时返回报告；前端缺失/非法时返回错误。该函数只读文件。
    pub fn arm(
        &self,
        dsh_package_dir: &Path,
        dsh_url: &str,
        dsh_home: Option<&Path>,
    ) -> Result<PreloadReport, String> {
        // 1. 解析 URL，只接受本机 http 地址；日志与报告只保留 origin，绝不记录 token。
        let url = tauri::Url::parse(dsh_url).map_err(|error| format!("PRELOAD_URL: {error}"))?;
        if url.host_str() != Some("127.0.0.1") {
            return Err("PRELOAD_URL 不是回环地址".to_string());
        }
        let port = url
            .port_or_known_default()
            .ok_or_else(|| "PRELOAD_URL 缺少端口".to_string())?;
        let origin = url.origin().ascii_serialization();

        // 2. 定位前端 dist 并加载静态资源与会话元数据。
        let root = frontend_root(dsh_package_dir).ok_or_else(|| {
            format!(
                "PRELOAD_UNSUPPORTED: 未找到 dsh-web-frontend（{}）",
                dsh_package_dir.display()
            )
        })?;
        let sessions = dsh_home.map(load_session_summaries).unwrap_or_default();
        let boot_json = serde_json::to_string(&sessions)
            .map_err(|error| format!("PRELOAD_BOOT: {error}"))?
            .replace('<', "\\u003c");
        let (mut cache, report) = load_dist(&root, port, &origin, &boot_json)?;

        // 3. 发布缓存；BOOT 数据独立保存，供 on_page_load 兜底注入。
        cache.origin = origin;
        let report = PreloadReport {
            sessions: sessions.len(),
            ..report
        };
        *self
            .frontend
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(cache);
        *self
            .boot_json
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(boot_json);
        self.log(&format!(
            "[preload] dist={} assets={} bytes={} sessions={} origin={}",
            report.root.display(),
            report.assets,
            report.bytes,
            report.sessions,
            report.origin
        ));
        Ok(report)
    }

    /// 记录一次内存响应命中。
    ///
    /// 参数：
    /// - `path`：命中的 URL path。
    /// - `bytes`：返回字节数。
    pub fn record_served(&self, path: &str, bytes: usize) {
        // 1. 计数并写一行诊断日志，验证静态资源确实由内存返回。
        let total = self.served.fetch_add(1, Ordering::Relaxed) + 1;
        self.log(&format!("[serve] #{total} {path} {bytes}B"));
    }
}

/// 从 `@deepseek-ai/dsh` 包目录定位前端 dist。
///
/// 参数：
/// - `core`：DSH 包目录（含 `package.json`）。
///
/// 返回值：最多向上 4 层查找 `node_modules/@deepseek-ai/dsh-web-frontend`，
/// 校验包名后返回其 `dist`；找不到时返回 `None`。
pub fn frontend_root(core: &Path) -> Option<PathBuf> {
    // 1. 兼容全局、项目本地、托管与捆绑四种布局。
    core.ancestors().take(4).find_map(|base| {
        let package = base.join("node_modules/@deepseek-ai/dsh-web-frontend");
        let text = fs::read_to_string(package.join("package.json")).ok()?;
        let metadata: serde_json::Value = serde_json::from_str(&text).ok()?;
        if metadata.get("name")?.as_str()? != "@deepseek-ai/dsh-web-frontend" {
            return None;
        }
        let dist = package.join("dist");
        dist.is_dir().then_some(dist)
    })
}

/// 加载 dist 下的 index 与静态资源。
///
/// 参数：
/// - `root`：前端 dist 根目录。
/// - `port`：sidecar 端口，用于 lookup 校验。
/// - `origin`：就绪地址，仅写入诊断。
/// - `boot_json`：注入 index 的会话 JSON。
///
/// 返回值：缓存对象与报告；文件缺失、总量超限或 index 不完整时报错。
fn load_dist(
    root: &Path,
    port: u16,
    origin: &str,
    boot_json: &str,
) -> Result<(FrontendCache, PreloadReport), String> {
    // 1. 读取 index.html 并注入 BOOT 脚本。
    let index_text = fs::read_to_string(root.join("index.html"))
        .map_err(|error| format!("PRELOAD_INDEX: {error}"))?;
    let boot_script =
        format!("<script>window.__INKDSH_PRELOADED_SESSIONS__ = {boot_json};</script>");
    let injected = if let Some(position) = index_text.find("</head>") {
        format!(
            "{}{boot_script}{}",
            &index_text[..position],
            &index_text[position..]
        )
    } else {
        format!("{boot_script}{index_text}")
    };

    // 2. 递归缓存 assets/ 与根目录静态文件。
    let mut assets = HashMap::new();
    let mut bytes = 0u64;
    collect_assets(root, root, &mut assets, &mut bytes)?;

    let report = PreloadReport {
        assets: assets.len(),
        bytes,
        sessions: 0,
        root: root.to_path_buf(),
        origin: origin.to_string(),
    };
    Ok((
        FrontendCache {
            origin: origin.to_string(),
            port,
            index: injected.into_bytes(),
            assets,
        },
        report,
    ))
}

/// 递归收集 dist 下的静态文件。
///
/// 参数：
/// - `root`：dist 根目录。
/// - `directory`：当前遍历目录。
/// - `assets`：输出 map，key 为 URL path。
/// - `bytes`：累计字节数。
fn collect_assets(
    root: &Path,
    directory: &Path,
    assets: &mut HashMap<String, CachedResponse>,
    bytes: &mut u64,
) -> Result<(), String> {
    // 1. 遍历目录；只接受普通文件，跳过符号链接。
    for entry in fs::read_dir(directory).map_err(|error| format!("PRELOAD_ASSETS: {error}"))? {
        let entry = entry.map_err(|error| format!("PRELOAD_ASSETS: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("PRELOAD_ASSETS: {error}"))?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_assets(root, &path, assets, bytes)?;
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|error| format!("PRELOAD_ASSETS: {error}"))?;
        let url_path = format!("/{}", relative.to_string_lossy().replace('\\', "/"));

        // 2. 只预载 assets/ 与根目录三个固定文件。
        let is_asset = url_path.starts_with("/assets/");
        let is_root_file = matches!(
            url_path.as_str(),
            "/manifest.webmanifest" | "/favicon.svg" | "/favicon-dark.svg"
        );
        if !is_asset && !is_root_file {
            continue;
        }

        // 3. 读取并累计；超过总量上限直接失败，避免异常 dist。
        let body = fs::read(&path).map_err(|error| format!("PRELOAD_ASSETS: {error}"))?;
        *bytes = bytes.saturating_add(body.len() as u64);
        if *bytes > MAX_STATIC_BYTES {
            return Err(format!(
                "PRELOAD_ASSETS 总量超过上限 {MAX_STATIC_BYTES} 字节"
            ));
        }
        assets.insert(
            url_path,
            CachedResponse {
                mime: content_type(&path),
                body,
            },
        );
    }
    Ok(())
}

/// 根据扩展名推断 Content-Type。
///
/// 参数：
/// - `path`：静态文件路径。
///
/// 返回值：常见前端资源 MIME；未知扩展返回 `application/octet-stream`。
fn content_type(path: &Path) -> &'static str {
    // 1. 与官方前端 dist 实际使用的扩展保持一致。
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("json" | "map" | "webmanifest") => "application/json",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// 会话预载摘要，注入 `window.__INKDSH_PRELOADED_SESSIONS__`。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionSummary {
    /// 会话 id（文件名去掉扩展名）。
    pub id: String,
    /// 投影里的标题；缺失时为空。
    pub title: String,
    /// 创建时工作目录。
    pub cwd: String,
    /// 投影缓存文件的修改时间（Unix 毫秒）。
    pub updated_at: u64,
}

/// 读取并解析最近若干会话投影缓存。
///
/// 参数：
/// - `dsh_home`：DSH 用户目录。
///
/// 返回值：按文件修改时间倒序的会话摘要（最多 [`MAX_SESSIONS`] 条）；目录
/// 不存在或条目损坏时返回空/跳过，不报错。
pub fn load_session_summaries(dsh_home: &Path) -> Vec<SessionSummary> {
    // 1. 收集 sessions/*.json 并按修改时间倒序。
    let directory = dsh_home
        .join("storages")
        .join("session_projcache")
        .join("sessions");
    let Ok(entries) = fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut files: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            if metadata.len() > MAX_SESSION_FILE_BYTES {
                return None;
            }
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or_default();
            Some((modified, entry.path()))
        })
        .collect();
    files.sort_by_key(|file| std::cmp::Reverse(file.0));
    files.truncate(MAX_SESSIONS);

    // 2. 逐条解析；损坏条目跳过。
    files
        .into_iter()
        .filter_map(|(updated_at, path)| {
            let text = fs::read_to_string(&path).ok()?;
            let value: serde_json::Value = serde_json::from_str(&text).ok()?;
            let rows = value.get("record")?.get("rows")?;
            let title = rows
                .get("title")
                .and_then(|title| title.get("val"))
                .and_then(|title| title.as_str())
                .unwrap_or("")
                .to_string();
            let identity = value.get("record")?.get("identity");
            let cwd = identity
                .and_then(|identity| identity.get("cwd"))
                .and_then(|cwd| cwd.as_str())
                .unwrap_or("")
                .to_string();
            let id = path.file_stem()?.to_string_lossy().to_string();
            Some(SessionSummary {
                id,
                title,
                cwd,
                updated_at,
            })
        })
        .collect()
}

/// 解析生产环境的 DSH_HOME。
///
/// 返回值：`DSH_HOME` 环境变量；否则 `%USERPROFILE%\.dsh`（Windows）或
/// `$HOME/.dsh`。该函数只读环境变量，不检查目录是否存在。
pub fn dsh_home() -> Option<PathBuf> {
    // 1. 显式环境变量优先。
    if let Some(value) = std::env::var_os("DSH_HOME") {
        if !value.is_empty() {
            return Some(PathBuf::from(value));
        }
    }

    // 2. 回退用户目录。
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join(".dsh"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建唯一临时目录。
    fn temp_dir(name: &str) -> PathBuf {
        // 1. 时间纳秒 + 进程号避免并行测试冲突。
        let unique = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "inkdsh-preload-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 写入一个最小 dist 布局。
    fn write_dist(root: &Path) {
        // 1. index + assets + 根目录图标。
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::write(
            root.join("index.html"),
            "<html><head><title>x</title></head><body><div id=\"root\"></div></body></html>",
        )
        .unwrap();
        fs::write(root.join("assets/index.js"), b"console.log(1);").unwrap();
        fs::write(root.join("assets/index.css"), b"body{}").unwrap();
        fs::write(root.join("favicon.svg"), b"<svg/>").unwrap();
    }

    /// 验证 dist 加载、BOOT 注入与内存查找。
    #[test]
    fn loads_dist_and_serves_from_memory() {
        // 1. 构造 dist 与状态。
        let root = temp_dir("dist");
        write_dist(&root);
        let state = PreloadState::new(root.join("preload.log"));
        let (cache, report) = load_dist(&root, 43210, "http://127.0.0.1:43210/", "[]").unwrap();
        assert_eq!(report.assets, 3);
        assert!(String::from_utf8_lossy(&cache.index).contains("__INKDSH_PRELOADED_SESSIONS__"));
        *state.frontend.write().unwrap() = Some(cache);

        // 2. 资源与带 token 的文档命中；其它 host/端口/根路径不命中。
        assert!(state
            .lookup("/assets/index.js", None, Some("127.0.0.1"), Some(43210))
            .is_some());
        assert!(state
            .lookup("/", Some("token=abc"), Some("127.0.0.1"), Some(43210))
            .is_some());
        assert!(state
            .lookup("/", None, Some("127.0.0.1"), Some(43210))
            .is_none());
        assert!(state
            .lookup("/", Some("token=abc"), Some("127.0.0.1"), Some(43211))
            .is_none());
        assert!(state
            .lookup("/assets/index.js", None, Some("localhost"), Some(43210))
            .is_none());

        let _ = fs::remove_dir_all(root);
    }

    /// 验证预载报告与日志只保留 origin，不泄露访问 token。
    #[test]
    fn arm_redacts_token_from_report_and_log() {
        // 1. 构造真实的包目录 + frontend dist 布局。
        let base = temp_dir("arm");
        let core = base.join("node_modules/@deepseek-ai/dsh");
        let frontend = core.join("node_modules/@deepseek-ai/dsh-web-frontend");
        fs::create_dir_all(&frontend).unwrap();
        fs::write(
            frontend.join("package.json"),
            r#"{"name":"@deepseek-ai/dsh-web-frontend"}"#,
        )
        .unwrap();
        write_dist(&frontend.join("dist"));

        // 2. 带 token 的 URL 只写入 origin，日志同样不含 token。
        let state = PreloadState::new(base.join("preload.log"));
        let report = state
            .arm(&core, "http://127.0.0.1:43210/?token=super-secret", None)
            .expect("预载应成功");
        assert_eq!(report.origin, "http://127.0.0.1:43210");
        let log = fs::read_to_string(base.join("preload.log")).unwrap();
        assert!(!log.contains("super-secret"), "{log}");
        assert!(log.contains("origin=http://127.0.0.1:43210"));

        let _ = fs::remove_dir_all(base);
    }

    /// 验证会话投影缓存解析、排序与损坏条目跳过。
    #[test]
    fn parses_session_projection_cache() {
        // 1. 写两个合法缓存和一个损坏缓存。
        let home = temp_dir("home");
        let sessions = home.join("storages/session_projcache/sessions");
        fs::create_dir_all(&sessions).unwrap();
        for (name, title) in [("s1.json", "第一个会话"), ("s2.json", "第二个会话")] {
            let body = format!(
                r#"{{"version":7,"record":{{"identity":{{"cwd":"D:\\Dev","createdAt":1}},"rows":{{"title":{{"ver":1,"seq":2,"val":"{title}"}}}}}}}}"#
            );
            fs::write(sessions.join(name), body).unwrap();
        }
        fs::write(sessions.join("broken.json"), "{not json").unwrap();

        // 2. 断言只解析合法条目，标题/CWD/ID 正确。
        let summaries = load_session_summaries(&home);
        assert_eq!(summaries.len(), 2);
        let titles: Vec<_> = summaries.iter().map(|item| item.title.as_str()).collect();
        assert!(titles.contains(&"第一个会话"));
        assert!(titles.contains(&"第二个会话"));
        assert!(summaries.iter().all(|item| item.cwd == "D:\\Dev"));

        let _ = fs::remove_dir_all(home);
    }

    /// 验证 BOOT JSON 的 `<` 会被转义，避免提前结束 script。
    #[test]
    fn escapes_boot_payload() {
        // 1. 标题包含 `</script>`。
        let home = temp_dir("escape-home");
        let sessions = home.join("storages/session_projcache/sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("evil.json"),
            r#"{"record":{"identity":{"cwd":"D:\\Dev"},"rows":{"title":{"val":"</script><script>alert(1)</script>"}}}}"#,
        )
        .unwrap();
        let summaries = load_session_summaries(&home);
        let json = serde_json::to_string(&summaries)
            .unwrap()
            .replace('<', "\\u003c");
        assert!(!json.contains("</script>"));
        assert!(json.contains("\\u003c/script>"));

        let _ = fs::remove_dir_all(home);
    }

    /// 验证扩展名到 MIME 的映射。
    #[test]
    fn maps_content_types() {
        // 1. 覆盖前端主要资源类型。
        assert_eq!(
            content_type(Path::new("a.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type(Path::new("a.css")), "text/css; charset=utf-8");
        assert_eq!(content_type(Path::new("a.svg")), "image/svg+xml");
        assert_eq!(content_type(Path::new("a.woff2")), "font/woff2");
        assert_eq!(
            content_type(Path::new("a.unknown")),
            "application/octet-stream"
        );
    }
}
