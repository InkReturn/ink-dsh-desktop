fn main() {
    // 1. 交给 tauri-build 读取 tauri.conf.json、生成权限清单与 Windows 资源。
    tauri_build::build();
}
