//! 构建脚本：在前端源码变更时重新编译 frontend/，产物 dist 由 rust_embed 打包进二进制。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// 递归收集目录下全部文件
fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn newest_mtime(files: &[PathBuf]) -> Option<SystemTime> {
    files
        .iter()
        .filter_map(|path| fs::metadata(path).ok()?.modified().ok())
        .max()
}

fn npm(args: &[&str]) -> Result<(), String> {
    let mut command = if cfg!(windows) {
        Command::new("npm.cmd")
    } else {
        Command::new("npm")
    };
    command.current_dir("frontend").args(args);
    let output = command
        .output()
        .map_err(|error| format!("npm 无法执行: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let frontend = PathBuf::from("frontend");
    let dist_index = frontend.join("dist").join("index.html");

    let top_files = [
        "index.html",
        "package.json",
        "package-lock.json",
        "tsconfig.json",
        "vite.config.ts",
    ];
    let mut sources: Vec<PathBuf> = top_files.iter().map(|name| frontend.join(name)).collect();
    for name in top_files {
        println!("cargo:rerun-if-changed=frontend/{name}");
    }
    for dir in ["src", "public"] {
        println!("cargo:rerun-if-changed=frontend/{dir}");
        collect_files(&frontend.join(dir), &mut sources);
    }

    let built = fs::metadata(&dist_index)
        .and_then(|meta| meta.modified())
        .ok();
    let up_to_date = matches!(
        (built, newest_mtime(&sources)),
        (Some(built), Some(latest)) if built >= latest
    );
    if up_to_date {
        return;
    }

    let result = (|| {
        if !frontend.join("node_modules").exists() {
            npm(&["ci"])?;
        }
        npm(&["run", "build"])
    })();

    match result {
        Ok(()) => {}
        Err(error) if dist_index.exists() => {
            println!("cargo:warning=前端构建失败，沿用已存在的 dist: {error}");
        }
        Err(error) => panic!(
            "前端构建失败且 frontend/dist 缺失: {error}\n请安装 Node.js 后重试，或手动执行 cd frontend && npm ci && npm run build"
        ),
    }
}
