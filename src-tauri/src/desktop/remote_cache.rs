//! P3b 远程静态资源磁盘缓存的**只读侧**。
//!
//! 写入由 dsh-tauri-remote 的 gateway 在已鉴权、200、非 Set-Cookie 的静态响应流上完成；
//! 这里不自行发网络请求，避免绕过 DSH 的启动 token / Cookie / 登录插件。
//! 缓存只覆盖带内容 hash 的 `/assets/<file>` 与站点图标 / manifest，不缓存 HTML、API。

use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub(crate) struct CachedAsset {
    pub body: Vec<u8>,
    pub content_type: String,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    path: String,
    #[serde(rename = "contentType")]
    content_type: String,
}

pub(crate) fn cache_path(path: &str) -> Option<&str> {
    let assets = path.strip_prefix("/assets/").is_some_and(|rest| {
        !rest.is_empty()
            && !rest.contains('/')
            && !rest.contains('?')
            && rest != "."
            && rest != ".."
            && !rest
                .bytes()
                .any(|byte| matches!(byte, b'%' | b'\\' | b':' | 0..=31))
    });
    // `/plugins/??module,module&rev=hash` 是 DSH 的插件客户端合并 bundle：
    // query 自带版本，只作为缓存 key 的一部分，不参与文件路径。
    let plugins = path.strip_prefix("/plugins/??").is_some_and(|rest| {
        !rest.is_empty()
            && path.len() <= 8 * 1024
            && !path
                .bytes()
                .any(|byte| !byte.is_ascii_graphic() || matches!(byte, b'#' | b'\\'))
    });
    (assets
        || plugins
        || matches!(
            path,
            "/manifest.webmanifest" | "/favicon.svg" | "/favicon-dark.svg"
        ))
    .then_some(path)
}

fn key(path: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(path.as_bytes());
    format!("{:x}", digest.finalize())
}

fn valid_content_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || matches!(byte, b' ' | b'\t'))
}

/// 所有失败均返回 None，让调用方回到原始 gateway 网络路径。
pub(crate) fn read(root: &Path, host_id: &str, identity: &str, path: &str) -> Option<CachedAsset> {
    if !host_id
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || !host_id.starts_with(|byte: char| byte.is_ascii_alphanumeric())
    {
        return None;
    }
    let expected_path = cache_path(path)?;
    let identity = key(identity);
    let base = root
        .join(host_id)
        .join(&identity[..16])
        .join(key(expected_path));
    // 先读并校验元数据，再读内容；侧车不匹配/损坏一律视为未命中，绝不猜测路径。
    let metadata: Metadata =
        serde_json::from_slice(&std::fs::read(base.with_extension("meta")).ok()?).ok()?;
    if metadata.path != expected_path || !valid_content_type(&metadata.content_type) {
        return None;
    }
    let body = std::fs::read(base).ok()?;
    Some(CachedAsset {
        body,
        content_type: metadata.content_type,
    })
}

/// P3c 后续接入后台预加载时的根目录接缝；本阶段不在原生侧发起任何回源请求。
#[allow(dead_code)]
pub(crate) fn host_root(root: &Path, host_id: &str) -> PathBuf {
    root.join(host_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_accepts_only_static_frontend_resources() {
        for path in [
            "/assets/index-8f3a2.js",
            "/assets/chunk.css",
            "/manifest.webmanifest",
            "/favicon.svg",
            "/favicon-dark.svg",
            "/plugins/??@deepseek-ai/dsh-client-modules/client.js&rev=abc",
        ] {
            assert_eq!(cache_path(path), Some(path));
        }
        for path in [
            "/",
            "/index.html",
            "/api/session",
            "/plugins/client.js",
            "/plugins/",
            "/plugins/??",
            "/plugins/??a b",
            "/assets/",
            "/assets/index.js?v=1",
            "/assets/nested/index.js",
            "/assets/../index.js",
            "/assets/%2e%2e",
        ] {
            assert_eq!(cache_path(path), None, "{path}");
        }
    }

    #[test]
    fn plugin_bundle_key_keeps_its_revision_query() {
        let root = tempfile::tempdir().unwrap();
        let path = "/plugins/??@deepseek-ai/dsh-client-modules/client.js&rev=abc";
        let base = root
            .path()
            .join("lan-host")
            .join(&key("http://192.168.31.83:3080")[..16])
            .join(key(path));
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        std::fs::write(&base, b"plugin").unwrap();
        std::fs::write(
            base.with_extension("meta"),
            format!(r#"{{"path":"{path}","fetchedAt":1,"contentType":"text/javascript; charset=utf-8","bytes":6}}"#),
        )
        .unwrap();
        assert_eq!(
            read(
                root.path(),
                "lan-host",
                "http://192.168.31.83:3080",
                path
            )
            .unwrap()
            .body,
            b"plugin"
        );
        assert!(read(
            root.path(),
            "lan-host",
            "http://192.168.31.83:3080",
            "/plugins/??@deepseek-ai/dsh-client-modules/client.js&rev=def"
        )
        .is_none());
    }

    #[test]
    fn write_format_roundtrip_and_host_isolation() {
        let root = tempfile::tempdir().unwrap();
        let path = "/assets/index-8f3a2.js";
        let base = root
            .path()
            .join("lan-host")
            .join(&key("http://192.168.31.83:3080")[..16])
            .join(key(path));
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        std::fs::write(&base, b"lan bundle").unwrap();
        std::fs::write(base.with_extension("meta"), format!(r#"{{"path":"{path}","fetchedAt":1,"contentType":"text/javascript; charset=utf-8","bytes":10}}"#)).unwrap();
        let cached = read(root.path(), "lan-host", "http://192.168.31.83:3080", path).unwrap();
        assert_eq!(cached.body, b"lan bundle");
        assert_eq!(cached.content_type, "text/javascript; charset=utf-8");
        assert!(read(root.path(), "https-host", "http://192.168.31.83:3080", path).is_none());
        assert!(read(root.path(), "lan-host", "https://dshs.inkmo.top", path).is_none());
    }

    #[test]
    fn corrupt_or_mismatched_entries_fall_back_to_network() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("lan-host").join(key("/assets/index.js"));
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        std::fs::write(&base, b"payload").unwrap();
        for metadata in [
            "not json",
            r#"{"path":"/assets/other.js","contentType":"text/javascript"}"#,
            r#"{"path":"/assets/index.js","contentType":"text/html\r\nSet-Cookie: secret"}"#,
        ] {
            std::fs::write(base.with_extension("meta"), metadata).unwrap();
            assert!(read(
                root.path(),
                "lan-host",
                "http://192.168.31.83:3080",
                "/assets/index.js"
            )
            .is_none());
        }
    }

    #[test]
    fn host_ids_cannot_escape_cache_root() {
        let root = tempfile::tempdir().unwrap();
        for id in [
            "../outside",
            "/absolute",
            "host/with/slash",
            "",
            "UPPERCASE",
        ] {
            assert!(
                read(
                    root.path(),
                    id,
                    "http://192.168.31.83:3080",
                    "/assets/index.js"
                )
                .is_none(),
                "{id}"
            );
        }
    }
}
