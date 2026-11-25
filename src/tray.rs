use crate::engine::App;
use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
use tao::{
    event::{Event, StartCause},
    event_loop::{ControlFlow, EventLoopBuilder},
};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

pub fn run(app: Arc<App>, url: String) -> Result<(), String> {
    let mut builder = EventLoopBuilder::new();
    #[allow(unused_mut)]
    let mut event_loop = builder.build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::EventLoopExtMacOS;
        event_loop.set_activation_policy(tao::platform::macos::ActivationPolicy::Accessory);
    }
    let menu = Menu::new();
    let open = MenuItem::new("打开 Codey", true, None);
    let status = MenuItem::new("Codey · 后台运行中", false, None);
    let quit = MenuItem::new("完全退出", true, None);
    let wait = MenuItem::new("等待任务完成后退出", true, None);
    let interrupt = MenuItem::new("中断任务并退出", true, None);
    menu.append_items(&[
        &open,
        &status,
        &PredefinedMenuItem::separator(),
        &quit,
        &wait,
        &interrupt,
    ])
    .map_err(|e| e.to_string())?;
    let mut tray = None;
    let mut last = Instant::now();
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(200));
        if let Event::NewEvents(StartCause::Init) = event {
            match TrayIconBuilder::new()
                .with_menu(Box::new(menu.clone()))
                .with_menu_on_left_click(false)
                .with_tooltip("Codey · 本地 Codex 调度")
                .with_icon(icon())
                .build()
            {
                Ok(t) => tray = Some(t),
                Err(e) => {
                    eprintln!("托盘创建失败：{e}。可用 --headless 运行。 ");
                    app.request_exit(true);
                }
            }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == open.id() {
                let _ = open::that(&url);
            } else if event.id == wait.id() {
                app.request_exit(false);
            } else if event.id == interrupt.id() {
                app.request_exit(true);
            } else if event.id == quit.id() {
                if app.active.lock().unwrap().is_empty() {
                    app.request_exit(false);
                } else {
                    let _ = open::that(format!("{url}&exit=1"));
                }
            }
        }
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = open::that(&url);
            }
        }
        if last.elapsed() > Duration::from_secs(1) {
            let approvals = app.approvals.lock().unwrap().len();
            let active = app.active.lock().unwrap().len();
            let text = if app.draining.load(Ordering::SeqCst) {
                "Codey · 正在退出".into()
            } else if approvals > 0 {
                format!("Codey · {approvals} 项待审批")
            } else {
                format!("Codey · {active} 项执行中")
            };
            status.set_text(&text);
            if let Some(t) = &tray {
                let _ = t.set_tooltip(Some(&text));
            }
            last = Instant::now();
        }
        if app.stopping.load(Ordering::SeqCst) {
            tray.take();
            *flow = ControlFlow::Exit;
        }
    });
}
fn icon() -> Icon {
    let mut data = vec![0; 32 * 32 * 4];
    for y in 0..32 {
        for x in 0..32 {
            let i = (y * 32 + x) * 4;
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            let inside = dx * dx + dy * dy < 220.;
            if inside {
                data[i..i + 4].copy_from_slice(&[53, 100, 240, 255]);
            }
            if (8..=23).contains(&y)
                && (8..=22).contains(&x)
                && ((8..=11).contains(&x) || (8..=11).contains(&y) || (20..=23).contains(&y))
            {
                data[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    Icon::from_rgba(data, 32, 32).unwrap()
}
