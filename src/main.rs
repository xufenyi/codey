#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod engine;
mod model;
mod rpc;
mod runtime;
mod server;
mod store;
mod tray;

fn main() {
    if let Err(error) = run() {
        eprintln!("Codey: {error}");
        let _ = notify_rust::Notification::new()
            .summary("Codey 未能启动")
            .body(&error)
            .appname("Codey")
            .show();
        std::process::exit(1);
    }
}
fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let headless = args.iter().any(|a| a == "--headless");
    let no_browser = args.iter().any(|a| a == "--no-browser");
    if args.iter().any(|a| a == "--help") {
        println!(
            "Codey 0.1.0\n  --headless       不创建托盘（服务/测试环境）\n  --no-browser     启动时不打开浏览器\n  --data-dir PATH  指定本机数据目录\n  --check-runtime  仅检查 PATH 中的 Codex，随后退出"
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--check-runtime") {
        let info = runtime::discover(None)?;
        println!("{}", serde_json::to_string_pretty(&info).unwrap());
        return Ok(());
    }
    let dir = if let Some(i) = args.iter().position(|a| a == "--data-dir") {
        std::path::PathBuf::from(args.get(i + 1).ok_or("--data-dir 缺少路径")?)
    } else {
        dirs::data_local_dir()
            .ok_or("无法确定本机数据目录")?
            .join("Codey")
    };
    let store = match store::Store::load(dir.clone()) {
        Ok(s) => s,
        Err(e) => {
            if e.starts_with("Codey 已在运行") {
                let value = store::read_small(&dir.join("session.json"))?;
                let session: serde_json::Value =
                    serde_json::from_str(&value).map_err(|e| e.to_string())?;
                let url = session["url"].as_str().ok_or("已有实例的会话信息无效")?;
                // Validate persisted endpoint before invoking the platform URL handler.
                if !url.starts_with("http://127.0.0.1:") {
                    return Err("已有实例地址无效".into());
                }
                open::that(url).map_err(|e| e.to_string())?;
                return Ok(());
            }
            return Err(e);
        }
    };
    let app = engine::App::new(store);
    let url = server::start(app.clone())?;
    engine::start(app.clone());
    println!(
        "Codey 正在本机运行。通过托盘打开管理页面。数据目录：{}",
        dir.display()
    );
    if !no_browser && let Err(e) = open::that(&url) {
        eprintln!("浏览器打开失败：{e}。请通过托盘重新打开。 ");
    }
    if headless {
        while !app.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    } else {
        tray::run(app.clone(), url)?;
    }
    app.shutdown(true)?;
    let _ = std::fs::remove_file(dir.join("session.json"));
    Ok(())
}
