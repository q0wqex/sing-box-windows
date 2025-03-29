use crate::process::manager::ProcessManager;
use std::sync::Arc;
use tracing::{error, info};
use serde_json::json;
use std::path::Path;
use crate::utils::app_util::get_work_dir;
use crate::utils::file_util::{ unzip_file};
use std::os::windows::process::CommandExt;
use tauri::Emitter;
use crate::app::constants::{paths, process, messages, network};
use tauri::{Runtime, Window};
use tokio::task;
use tokio::sync::mpsc;
use futures_util::StreamExt;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;
use serde_json::Value;

// 全局进程管理器
lazy_static::lazy_static! {
    pub(crate) static ref PROCESS_MANAGER: Arc<ProcessManager> = Arc::new(ProcessManager::new());
}

// 检查内核版本
#[tauri::command]
pub async fn check_kernel_version() -> Result<String, String> {
    let kernel_path = paths::get_kernel_path();

    if !kernel_path.exists() {
        return Err(messages::ERR_KERNEL_NOT_FOUND.to_string());
    }

    let output = std::process::Command::new(kernel_path)
        .arg("version")
        .creation_flags(process::CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("{}: {}", messages::ERR_VERSION_CHECK_FAILED, e))?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{}: {}", messages::ERR_GET_VERSION_FAILED, error));
    }

    let version_info = String::from_utf8_lossy(&output.stdout);
    Ok(version_info.to_string())
}

// 运行内核
#[tauri::command]
pub async fn start_kernel() -> Result<(), String> {
    PROCESS_MANAGER.start().await.map_err(|e| e.to_string())
}

// 停止内核
#[tauri::command]
pub async fn stop_kernel() -> Result<(), String> {
    PROCESS_MANAGER.stop().await.map_err(|e| e.to_string())
}

// 重启内核
#[tauri::command]
pub async fn restart_kernel() -> Result<(), String> {
    PROCESS_MANAGER.restart().await.map_err(|e| e.to_string())
}

// 获取进程状态
#[tauri::command]
pub async fn get_process_status() -> serde_json::Value {
    let info = PROCESS_MANAGER.get_status().await;
    json!({
        "status": info.status,
        "pid": info.pid,
        "last_error": info.last_error
    })
}

// 下载内核
#[tauri::command]
pub async fn download_latest_kernel(window: tauri::Window) -> Result<(), String> {
    let work_dir = get_work_dir();
    info!("当前工作目录: {}", work_dir);

    let path = Path::new(&work_dir).join("sing-box/");
    info!("目标下载目录: {}", path.display());

    // 如果目录已存在，先检查是否为有效目录
    if path.exists() {
        if !path.is_dir() {
            error!("sing-box 路径存在但不是目录");
            return Err("sing-box 路径存在但不是目录".to_string());
        }
    }

    // 确保目录存在
    if let Err(e) = std::fs::create_dir_all(&path) {
        error!("创建目录失败: {}", e);
        return Err(format!("创建目录失败: {}", e));
    }
    info!("已确保下载目录存在");

    info!("正在准备下载最新版本...");
    // 发送进度事件
    let _ = window.emit(
        "download-progress",
        json!({
            "status": "checking",
            "progress": 0,
            "message": "正在获取最新版本信息..."
        }),
    );

    // 获取最新版本信息
    let client = reqwest::Client::new();
    let releases_url = "https://api.github.com/repos/SagerNet/sing-box/releases/latest";
    let response = client
        .get(releases_url)
        .header("User-Agent", "sing-box-windows")
        .send()
        .await
        .map_err(|e| format!("获取版本信息失败: {}", e))?;

    let release: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("解析版本信息失败: {}", e))?;

    // 获取版本号
    let version = release["tag_name"]
        .as_str()
        .ok_or("无法获取版本号")?
        .trim_start_matches('v')
        .to_string();

    // 获取当前系统平台和架构
    let platform = std::env::consts::OS;
    let mut arch = std::env::consts::ARCH;
    if arch == "x86_64" {
        arch = "amd64";
    }

    // 构建目标文件名
    let target_asset_name = format!("sing-box-{}-{}-{}.zip", version, platform, arch);
    info!("目标文件名: {}", target_asset_name);

    // 查找Windows版本资源
    let assets = release["assets"].as_array().ok_or("无法获取发布资源")?;
    let asset = assets
        .iter()
        .find(|asset| {
            if let Some(name) = asset["name"].as_str() {
                name.contains("windows-amd64") && name.ends_with(".zip")
            } else {
                false
            }
        })
        .ok_or("未找到适用于Windows的资源")?;

    // 获取下载链接
    let original_url = asset["browser_download_url"]
        .as_str()
        .ok_or("无法获取下载链接")?;

    info!("找到下载链接: {}", original_url);

    let download_path = Path::new(&path).join(&target_asset_name);
    info!("目标下载路径: {}", download_path.display());

    // 发送进度事件
    let _ = window.emit(
        "download-progress",
        json!({
            "status": "downloading",
            "progress": 20,
            "message": format!("开始下载文件: {}", target_asset_name)
        }),
    );

    // 下载文件
    let window_clone = window.clone();
    if let Err(e) = crate::utils::file_util::download_with_fallback(
        original_url, 
        download_path.to_str().unwrap(), 
        move |progress| {
            let real_progress = 20 + (progress as f64 * 0.6) as u32; // 20-80%的进度用于下载
            let _ = window_clone.emit(
                "download-progress",
                json!({
                    "status": "downloading",
                    "progress": real_progress,
                    "message": format!("正在下载: {}%", progress)
                }),
            );
        }).await {
        error!("下载失败: {}", e);
        return Err(format!(
            "下载失败: {}。\n您可以尝试手动下载：\n1. 访问 https://github.com/SagerNet/sing-box/releases/latest\n2. 下载 {}\n3. 解压并将文件放置在 {}/sing-box/ 目录下",
            e, target_asset_name, get_work_dir()
        ));
    }

    // 解压文件
    info!("开始解压文件...");
    // 发送进度事件
    let _ = window.emit(
        "download-progress",
        json!({
            "status": "extracting",
            "progress": 80,
            "message": "正在解压文件..."
        }),
    );

    let out_path = Path::new(&work_dir).join("sing-box");
    match unzip_file(download_path.to_str().unwrap(), out_path.to_str().unwrap()).await {
        Ok(_) => {
            info!("内核已下载并解压到: {}", out_path.display());
            // 发送完成事件
            let _ = window.emit(
                "download-progress",
                json!({
                    "status": "completed",
                    "progress": 100,
                    "message": "下载完成！"
                }),
            );
        }
        Err(e) => {
            error!("解压文件失败: {}", e);
            return Err(format!("解压文件失败: {}", e));
        }
    }

    Ok(())
}

/// 启动WebSocket数据中继
#[tauri::command]
pub async fn start_websocket_relay<R: Runtime>(window: Window<R>) -> Result<(), String> {
    // 启动四个不同类型的WebSocket中继
    start_traffic_relay(window.clone()).await?;
    start_memory_relay(window.clone()).await?;
    start_logs_relay(window.clone()).await?;
    start_connections_relay(window.clone()).await?;
    
    Ok(())
}

/// 启动流量数据中继
async fn start_traffic_relay<R: Runtime>(window: Window<R>) -> Result<(), String> {
    let window_clone = window.clone();
    let (tx, mut rx) = mpsc::channel(32);
    let token = crate::app::proxy_service::get_api_token();
    
    // 启动WebSocket连接和数据处理任务
    let _handle = task::spawn(async move {
        let url = Url::parse(&format!("ws://127.0.0.1:{}/traffic?token={}", 
                                     network::DEFAULT_CLASH_API_PORT, token)).unwrap();
        
        match connect_async(url).await {
            Ok((ws_stream, _)) => {
                let (mut _write, mut read) = ws_stream.split();
                
                // 持续读取WebSocket消息
                while let Some(message) = read.next().await {
                    match message {
                        Ok(Message::Text(text)) => {
                            if let Ok(data) = serde_json::from_str::<Value>(&text) {
                                let _ = tx.send(data).await;
                            }
                        },
                        Ok(Message::Close(_)) => break,
                        Err(e) => {
                            error!("WebSocket流量数据读取错误: {}", e);
                            break;
                        },
                        _ => {}
                    }
                }
            },
            Err(e) => {
                error!("WebSocket流量连接失败: {}", e);
            }
        }
    });
    
    // 启动事件发送任务
    task::spawn(async move {
        while let Some(data) = rx.recv().await {
            let _ = window_clone.emit("traffic-data", data);
        }
    });
    
    Ok(())
}

/// 启动内存数据中继
async fn start_memory_relay<R: Runtime>(window: Window<R>) -> Result<(), String> {
    let window_clone = window.clone();
    let (tx, mut rx) = mpsc::channel(32);
    let token = crate::app::proxy_service::get_api_token();
    
    // 启动WebSocket连接和数据处理任务
    let _handle = task::spawn(async move {
        let url = Url::parse(&format!("ws://127.0.0.1:{}/memory?token={}", 
                                     network::DEFAULT_CLASH_API_PORT, token)).unwrap();
        
        match connect_async(url).await {
            Ok((ws_stream, _)) => {
                let (mut _write, mut read) = ws_stream.split();
                
                // 持续读取WebSocket消息
                while let Some(message) = read.next().await {
                    match message {
                        Ok(Message::Text(text)) => {
                            if let Ok(data) = serde_json::from_str::<Value>(&text) {
                                let _ = tx.send(data).await;
                            }
                        },
                        Ok(Message::Close(_)) => break,
                        Err(e) => {
                            error!("WebSocket内存数据读取错误: {}", e);
                            break;
                        },
                        _ => {}
                    }
                }
            },
            Err(e) => {
                error!("WebSocket内存连接失败: {}", e);
            }
        }
    });
    
    // 启动事件发送任务
    task::spawn(async move {
        while let Some(data) = rx.recv().await {
            let _ = window_clone.emit("memory-data", data);
        }
    });
    
    Ok(())
}

/// 启动日志数据中继
async fn start_logs_relay<R: Runtime>(window: Window<R>) -> Result<(), String> {
    let window_clone = window.clone();
    let (tx, mut rx) = mpsc::channel(32);
    let token = crate::app::proxy_service::get_api_token();
    
    // 启动WebSocket连接和数据处理任务
    let _handle = task::spawn(async move {
        let url = Url::parse(&format!("ws://127.0.0.1:{}/logs?token={}", 
                                     network::DEFAULT_CLASH_API_PORT, token)).unwrap();
        
        match connect_async(url).await {
            Ok((ws_stream, _)) => {
                let (mut _write, mut read) = ws_stream.split();
                
                // 持续读取WebSocket消息
                while let Some(message) = read.next().await {
                    match message {
                        Ok(Message::Text(text)) => {
                            if let Ok(data) = serde_json::from_str::<Value>(&text) {
                                let _ = tx.send(data).await;
                            }
                        },
                        Ok(Message::Close(_)) => break,
                        Err(e) => {
                            error!("WebSocket日志数据读取错误: {}", e);
                            break;
                        },
                        _ => {}
                    }
                }
            },
            Err(e) => {
                error!("WebSocket日志连接失败: {}", e);
            }
        }
    });
    
    // 启动事件发送任务
    task::spawn(async move {
        while let Some(data) = rx.recv().await {
            let _ = window_clone.emit("log-data", data);
        }
    });
    
    Ok(())
}

/// 启动连接数据中继
async fn start_connections_relay<R: Runtime>(window: Window<R>) -> Result<(), String> {
    let window_clone = window.clone();
    let (tx, mut rx) = mpsc::channel(32);
    let token = crate::app::proxy_service::get_api_token();
    
    // 启动WebSocket连接和数据处理任务
    let _handle = task::spawn(async move {
        let url = Url::parse(&format!("ws://127.0.0.1:{}/connections?token={}", 
                                     network::DEFAULT_CLASH_API_PORT, token)).unwrap();
        
        match connect_async(url).await {
            Ok((ws_stream, _)) => {
                let (mut _write, mut read) = ws_stream.split();
                
                // 持续读取WebSocket消息
                while let Some(message) = read.next().await {
                    match message {
                        Ok(Message::Text(text)) => {
                            if let Ok(data) = serde_json::from_str::<Value>(&text) {
                                let _ = tx.send(data).await;
                            }
                        },
                        Ok(Message::Close(_)) => break,
                        Err(e) => {
                            error!("WebSocket连接数据读取错误: {}", e);
                            break;
                        },
                        _ => {}
                    }
                }
            },
            Err(e) => {
                error!("WebSocket连接数据连接失败: {}", e);
            }
        }
    });
    
    // 启动事件发送任务
    task::spawn(async move {
        while let Some(data) = rx.recv().await {
            let _ = window_clone.emit("connections-data", data);
        }
    });
    
    Ok(())
} 