//! 随包运行时清单：最低 DSH 版本与正式核心包制品元数据。
//!
//! 清单文件是 `resources/manifest.json`，打包时通过 tauri.conf.json 映射到
//! 安装资源根。清单缺省或损坏时不阻断启动，但必须把原因写进诊断日志；真正
//! 需要装配托管运行时且 URL/SHA-256 未配置时，由安装器返回 blocked-by-env。

use std::{
    fs,
    path::{Path, PathBuf},
};

use semver::Version;
use serde::Deserialize;

use super::version::parse_version;

/// 资源根下的清单位置。
pub const MANIFEST_FILE_NAME: &str = "manifest.json";

/// 显式指定清单路径的环境变量，供开发与测试使用。
pub const ENV_MANIFEST: &str = "INKDSH_MANIFEST";

/// 运行时清单顶层结构。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RuntimeManifest {
    /// 清单结构版本；目前只识别 1，缺省按 1 处理。
    #[serde(default, rename = "schemaVersion")]
    pub schema_version: Option<u32>,
    /// 引擎版本基线。
    #[serde(default)]
    pub engines: Engines,
    /// 托管核心包制品描述。
    #[serde(default)]
    pub core: CoreDescriptor,
}

/// 引擎基线集合。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Engines {
    /// DSH 相关基线。
    #[serde(default)]
    pub dsh: DshEngine,
}

/// DSH 引擎基线。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DshEngine {
    /// 允许启动的最低 DSH 版本；缺省表示不限制。
    #[serde(default)]
    pub minimum: Option<String>,
}

/// 托管核心包制品描述。
///
/// 字段全部可空：正式发布地址与摘要尚未确定时保持 `null`，安装器据此返回
/// blocked-by-env，而不是伪造或猜测摘要。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreDescriptor {
    /// 制品对应的 DSH 版本，便于日志与人工核对。
    #[serde(default)]
    pub version: Option<String>,
    /// 制品下载地址；仅允许 https（安装器还会二次校验）。
    #[serde(default)]
    pub url: Option<String>,
    /// 制品 SHA-256，允许携带 `sha256:` 前缀。
    #[serde(default)]
    pub sha256: Option<String>,
    /// 期望字节数，仅用于诊断；不参与完整性判定。
    #[serde(default)]
    pub size_bytes: Option<u64>,
    /// 制品文件名；缺省时从 URL 最后一段推导。
    #[serde(default)]
    pub file_name: Option<String>,
}

/// 清单加载结果。
#[derive(Debug, Clone, Default)]
pub struct LoadedManifest {
    /// 解析出来的清单；找不到或损坏时是默认值。
    pub manifest: RuntimeManifest,
    /// 解析成功的最低版本；缺失/非法时为 `None`。
    pub minimum: Option<Version>,
    /// 最低版本的规范文本，用于日志与错误消息。
    pub minimum_text: Option<String>,
    /// 实际读取的清单路径。
    pub path: Option<PathBuf>,
    /// 加载阶段的诊断信息，按发生顺序记录。
    pub diagnostics: Vec<String>,
}

impl LoadedManifest {
    /// 从多个候选位置加载清单。
    ///
    /// 参数：
    /// - `explicit`：`INKDSH_MANIFEST` 指定的路径；为空时忽略。
    /// - `resource_dir`：Tauri 安装资源根；打包后清单位于其下。
    /// - `crate_dir`：编译期 `src-tauri` 目录；开发运行时用它定位仓库内清单。
    ///
    /// 返回值：按「显式路径 → 资源目录 → 开发目录」顺序取第一个可解析文件；
    /// 全部不可用时返回默认清单并附带诊断。该函数只读文件系统。
    pub fn load(
        explicit: Option<&Path>,
        resource_dir: Option<&Path>,
        crate_dir: Option<&Path>,
    ) -> Self {
        // 1. 按优先级收集候选路径，并保持标签用于诊断。
        let mut candidates: Vec<(String, PathBuf)> = Vec::new();
        if let Some(path) = explicit {
            candidates.push(("INKDSH_MANIFEST".to_string(), path.to_path_buf()));
        }
        if let Some(dir) = resource_dir {
            candidates.push(("安装资源".to_string(), dir.join(MANIFEST_FILE_NAME)));
        }
        if let Some(dir) = crate_dir {
            candidates.push((
                "开发目录".to_string(),
                dir.join("resources").join(MANIFEST_FILE_NAME),
            ));
        }

        let mut diagnostics = Vec::new();
        if candidates.is_empty() {
            diagnostics.push("未提供运行时清单候选路径，最低版本校验关闭".to_string());
            return Self {
                diagnostics,
                ..Self::default()
            };
        }

        // 2. 逐个尝试读取；显式路径读失败只记诊断，继续查安装资源。
        for (label, path) in &candidates {
            let text = match fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    diagnostics.push(format!("{label}清单不存在：{}", path.display()));
                    continue;
                }
                Err(error) => {
                    diagnostics.push(format!(
                        "{label}清单读取失败（{}）：{error}",
                        path.display()
                    ));
                    continue;
                }
            };

            // 3. 解析成功即采用；JSON 损坏时记录原因并继续。
            match serde_json::from_str::<RuntimeManifest>(&text) {
                Ok(manifest) => {
                    let mut loaded = Self {
                        manifest,
                        path: Some(path.clone()),
                        diagnostics,
                        ..Self::default()
                    };
                    loaded.warn_unknown_schema();
                    loaded.resolve_minimum();
                    return loaded;
                }
                Err(error) => diagnostics.push(format!(
                    "{label}清单解析失败（{}）：{error}",
                    path.display()
                )),
            }
        }

        // 4. 没有任何可用清单：默认值 + 汇总诊断，不阻断启动。
        diagnostics.push("没有可用的运行时清单，最低版本校验关闭".to_string());
        Self {
            diagnostics,
            ..Self::default()
        }
    }

    /// 检查清单结构版本并记录不识别版本。
    ///
    /// 副作用：仅追加诊断。未知版本仍按当前 v1 结构读取，避免因未来新增
    /// 字段导致整个清单被丢弃；诊断会随启动日志暴露给维护者。
    fn warn_unknown_schema(&mut self) {
        // 1. 缺省按 1 处理；只有明确的不同版本才告警。
        match self.manifest.schema_version {
            None | Some(1) => {}
            Some(version) => self.diagnostics.push(format!(
                "清单 schemaVersion {version} 未识别，按 v1 结构读取"
            )),
        }
    }

    /// 解析清单中的最低版本并写回字段。
    ///
    /// 副作用：更新 `minimum` / `minimum_text`；版本非法时追加诊断并保持
    /// `None`，避免把损坏的基线当成 0.0.0 误杀所有候选。
    fn resolve_minimum(&mut self) {
        // 1. 取 engines.dsh.minimum 的原始文本。
        let Some(raw) = self.manifest.engines.dsh.minimum.as_deref() else {
            return;
        };
        if raw.trim().is_empty() {
            return;
        }
        // 2. 解析成功才启用基线；失败时明确记录，而不是静默忽略。
        match parse_version(raw) {
            Some(version) => {
                self.minimum_text = Some(version.to_string());
                self.minimum = Some(version);
            }
            None => self
                .diagnostics
                .push(format!("清单最低版本无法解析：{raw}，最低版本校验关闭")),
        }
    }

    /// 生成一条可读的「当前清单状态」日志。
    ///
    /// 返回值：包含清单路径、最低版本与制品配置状态的单行文本。
    pub fn summary(&self) -> String {
        // 1. 路径缺失时明确写「未加载」。
        // 2. 最低版本、核心包版本与 URL/SHA 是否配置都写出来，便于定位
        //    blocked-by-env。
        let path = self
            .path
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "未加载".to_string());
        let minimum = self.minimum_text.as_deref().unwrap_or("未设置");
        let version = self
            .manifest
            .core
            .version
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or("未配置");
        let url = self.manifest.core.url.as_deref().unwrap_or("");
        let sha = self.manifest.core.sha256.as_deref().unwrap_or("");
        format!(
            "清单 {path}；最低 DSH 版本 {minimum}；核心包版本 {version}；url={}；sha256={}",
            if url.is_empty() {
                "未配置"
            } else {
                "已配置"
            },
            if sha.is_empty() {
                "未配置"
            } else {
                "已配置"
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建唯一的临时目录，测试结束后由调用方清理。
    fn temp_dir(name: &str) -> PathBuf {
        // 1. 用系统时间纳秒 + 进程内名称避免并行测试冲突。
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("inkdsh-manifest-{name}-{unique}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 验证完整清单能解析出最低版本与制品字段。
    #[test]
    fn parses_manifest_with_minimum_and_artifact() {
        // 1. 写入一份包含预发布基线与制品描述的清单。
        let dir = temp_dir("full");
        let path = dir.join(MANIFEST_FILE_NAME);
        fs::write(
            &path,
            r#"{
              "engines": { "dsh": { "minimum": "v0.2.0" } },
              "core": {
                "version": "0.2.1-rc.2",
                "url": "https://example.invalid/dsh-core.zip",
                "sha256": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "sizeBytes": 1024,
                "fileName": "dsh-core.zip"
              }
            }"#,
        )
        .unwrap();

        // 2. 加载后断言版本已归一化、字段原样保留。
        let loaded = LoadedManifest::load(Some(&path), None, None);
        assert_eq!(loaded.minimum_text.as_deref(), Some("0.2.0"));
        assert_eq!(loaded.minimum, parse_version("0.2.0"));
        assert_eq!(
            loaded.manifest.core.file_name.as_deref(),
            Some("dsh-core.zip")
        );
        assert_eq!(loaded.manifest.core.size_bytes, Some(1024));
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);

        let _ = fs::remove_dir_all(dir);
    }

    /// 验证缺失清单不阻断启动，但留下诊断。
    #[test]
    fn missing_manifest_is_diagnostic_only() {
        // 1. 指向不存在的资源目录。
        // 2. 断言没有最低版本，且诊断包含「不存在」。
        let dir = temp_dir("missing");
        let loaded = LoadedManifest::load(None, Some(&dir), None);
        assert!(loaded.minimum.is_none());
        assert!(loaded.summary().contains("未设置"));
        assert!(loaded
            .diagnostics
            .iter()
            .any(|line| line.contains("不存在")));

        let _ = fs::remove_dir_all(dir);
    }

    /// 验证损坏 JSON 与非法最低版本都只记录诊断，不误导为可用基线。
    #[test]
    fn invalid_manifest_or_minimum_degrades_safely() {
        // 1. 损坏 JSON：回退默认清单。
        let dir = temp_dir("broken");
        let path = dir.join(MANIFEST_FILE_NAME);
        fs::write(&path, "{ not json").unwrap();
        let loaded = LoadedManifest::load(Some(&path), None, None);
        assert!(loaded.minimum.is_none());
        assert!(loaded
            .diagnostics
            .iter()
            .any(|line| line.contains("解析失败")));

        // 2. 非法最低版本：不产生基线。
        fs::write(&path, r#"{"engines":{"dsh":{"minimum":"latest"}}}"#).unwrap();
        let loaded = LoadedManifest::load(Some(&path), None, None);
        assert!(loaded.minimum.is_none());
        assert!(loaded
            .diagnostics
            .iter()
            .any(|line| line.contains("无法解析")));

        // 3. 未识别的 schemaVersion：保留 v1 读取，但明确告警。
        fs::write(
            &path,
            r#"{"schemaVersion":2,"engines":{"dsh":{"minimum":"0.2.0"}}}"#,
        )
        .unwrap();
        let loaded = LoadedManifest::load(Some(&path), None, None);
        assert_eq!(loaded.minimum_text.as_deref(), Some("0.2.0"));
        assert!(loaded
            .diagnostics
            .iter()
            .any(|line| line.contains("schemaVersion 2")));

        let _ = fs::remove_dir_all(dir);
    }

    /// 验证显式路径优先于资源目录。
    #[test]
    fn explicit_manifest_wins_over_resource_dir() {
        // 1. 资源目录放 0.3.0，显式路径放 0.4.0。
        let dir = temp_dir("priority");
        let resource = dir.join("resource");
        let explicit = dir.join("explicit.json");
        fs::create_dir_all(&resource).unwrap();
        fs::write(
            resource.join(MANIFEST_FILE_NAME),
            r#"{"engines":{"dsh":{"minimum":"0.3.0"}}}"#,
        )
        .unwrap();
        fs::write(&explicit, r#"{"engines":{"dsh":{"minimum":"0.4.0"}}}"#).unwrap();

        // 2. 断言采用显式清单。
        let loaded = LoadedManifest::load(Some(&explicit), Some(&resource), None);
        assert_eq!(loaded.minimum_text.as_deref(), Some("0.4.0"));
        assert_eq!(loaded.path.as_deref(), Some(explicit.as_path()));

        let _ = fs::remove_dir_all(dir);
    }
}
