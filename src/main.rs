#![windows_subsystem = "windows"] // sem janela de console

use std::thread;
use std::time::{Duration, Instant};

use hidapi::HidApi;
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};

const SONY_VID: u16 = 0x054C;
const DUALSENSE_PID: u16 = 0x0CE6;
const DUALSENSE_EDGE_PID: u16 = 0x0DF2;
const POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    Disconnected,
    Battery { pct: u8, charging: bool, full: bool, bt: bool },
}

/// Lê o status da bateria do primeiro DualSense encontrado.
fn read_state(api: &mut HidApi) -> State {
    let _ = api.refresh_devices();

    for info in api.device_list() {
        if info.vendor_id() != SONY_VID
            || !matches!(info.product_id(), DUALSENSE_PID | DUALSENSE_EDGE_PID)
        {
            continue;
        }
        let Ok(dev) = info.open_device(api) else { continue };

        // Pedir o feature report 0x05 (calibração) faz o controle via Bluetooth
        // sair do report "simples" (0x01) e mandar o report completo (0x31).
        let mut feat = [0u8; 64];
        feat[0] = 0x05;
        let _ = dev.get_feature_report(&mut feat);

        let mut buf = [0u8; 128];
        for _ in 0..15 {
            let Ok(n) = dev.read_timeout(&mut buf, 200) else { break };
            // USB: report 0x01 com 64 bytes, status no byte 53
            // BT:  report 0x31 com 78 bytes, status no byte 54
            let (status, bt) = match buf[0] {
                0x01 if n >= 64 => (buf[53], false),
                0x31 if n >= 78 => (buf[54], true),
                _ => continue,
            };
            let level = status & 0x0F; // 0..10
            let charge = status >> 4; // 0 = descarregando, 1 = carregando, 2 = cheio
            let full = charge == 2;
            let pct = if full { 100 } else { (level * 10 + 5).min(100) };
            return State::Battery { pct, charging: charge == 1, full, bt };
        }
    }
    State::Disconnected
}

fn tooltip(s: State) -> String {
    match s {
        State::Disconnected => "DualSense: desconectado".into(),
        State::Battery { pct, charging, full, bt } => {
            let conn = if bt { "Bluetooth" } else { "USB" };
            let extra = if full {
                " (cheio)"
            } else if charging {
                " (carregando)"
            } else {
                ""
            };
            format!("DualSense: ~{pct}%{extra} — {conn}")
        }
    }
}

/// Desenha um ícone de bateria 32x32 em RGBA.
fn make_icon(s: State) -> Icon {
    const W: usize = 32;
    const H: usize = 32;
    let mut px = vec![0u8; W * H * 4];
    let mut put = |x: usize, y: usize, c: [u8; 4]| {
        let i = (y * W + x) * 4;
        px[i..i + 4].copy_from_slice(&c);
    };

    let outline = match s {
        State::Disconnected => [120, 120, 120, 255],
        _ => [235, 235, 235, 255],
    };

    // corpo: x 1..=27, y 8..=23 / polo: x 28..=30, y 13..=18
    for x in 1..=27 {
        for y in 8..=23 {
            if x <= 2 || x >= 26 || y <= 9 || y >= 22 {
                put(x, y, outline);
            }
        }
    }
    for x in 28..=30 {
        for y in 13..=18 {
            put(x, y, outline);
        }
    }

    if let State::Battery { pct, charging, full, .. } = s {
        let fill = if charging || full {
            [60, 160, 255, 255] // azul carregando
        } else if pct > 50 {
            [70, 200, 90, 255]
        } else if pct > 20 {
            [240, 190, 40, 255]
        } else {
            [230, 60, 60, 255]
        };
        let inner_w = 22; // x 4..=25
        let w = (inner_w * pct as usize).div_ceil(100).max(1);
        for x in 4..4 + w {
            for y in 11..=20 {
                put(x, y, fill);
            }
        }
    }

    Icon::from_rgba(px, W as u32, H as u32).expect("ícone inválido")
}

fn main() {
    let event_loop = EventLoopBuilder::<State>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // Thread que consulta o controle e manda o estado pro loop principal.
    thread::spawn(move || {
        let Ok(mut api) = HidApi::new() else { return };
        let mut last = None;
        loop {
            let s = read_state(&mut api);
            if last != Some(s) {
                if proxy.send_event(s).is_err() {
                    return;
                }
                last = Some(s);
            }
            thread::sleep(POLL_INTERVAL);
        }
    });

    let menu = Menu::new();
    let quit = MenuItem::new("Sair", true, None);
    menu.append(&quit).unwrap();
    let quit_id = quit.id().clone();

    let initial = State::Disconnected;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(make_icon(initial))
        .with_tooltip(tooltip(initial))
        .build()
        .expect("falha ao criar o ícone da bandeja");

    event_loop.run(move |event, _, cf| {
        *cf = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(250));

        if let Event::UserEvent(s) = event {
            let _ = tray.set_icon(Some(make_icon(s)));
            let _ = tray.set_tooltip(Some(tooltip(s)));
        }

        if let Ok(ev) = MenuEvent::receiver().try_recv() {
            if ev.id == quit_id {
                *cf = ControlFlow::Exit;
            }
        }
    });
}
