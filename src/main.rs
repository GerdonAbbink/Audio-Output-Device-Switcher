#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod flyout;
mod icons;
mod model;
mod sonar;
mod winaudio;
mod worker;

use std::sync::mpsc;

use tray_icon::{MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, LPARAM, WPARAM};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostThreadMessageW, SetTimer, TranslateMessage, WM_APP,
};
use windows::core::w;
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};

use flyout::Action;
use model::{Snapshot, Status};
use worker::Command;

const APP: &str = "AudioOutputDeviceSwitcher";
/// Autostart value written by early builds; removed when autostart is toggled.
const OLD_APP: &str = "SonarTray";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

pub fn light_taskbar() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("SystemUsesLightTheme"))
        .map(|v| v == 1)
        .unwrap_or(false)
}

fn icon_size() -> u32 {
    let dpi = unsafe { GetDpiForSystem() }.max(96);
    (16 * dpi + 48) / 96
}

fn autostart_enabled() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(RUN_KEY, KEY_READ)
        .and_then(|k| k.get_value::<String, _>(APP))
        .is_ok()
}

fn set_autostart(on: bool) {
    let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE) else { return };
    let _ = key.delete_value(OLD_APP);
    if on {
        if let Ok(exe) = std::env::current_exe() {
            let _ = key.set_value(APP, &format!("\"{}\"", exe.display()));
        }
    } else {
        let _ = key.delete_value(APP);
    }
}

struct App {
    tray: TrayIcon,
    cmd: mpsc::Sender<Command>,
    snap: Snapshot,
    drawn: Option<(Status, bool, u32)>,
}

impl App {
    fn redraw_icon(&mut self) {
        let key = (self.snap.status(), light_taskbar(), icon_size());
        if self.drawn != Some(key) {
            let _ = self.tray.set_icon(Some(icons::render(key.0, key.1, key.2)));
            self.drawn = Some(key);
        }
    }

    fn update(&mut self, snap: Snapshot) {
        self.snap = snap;
        worker::log(&format!("state: {:?} | {}", self.snap.status(), self.snap.tooltip().replace('\n', " | ")));
        self.redraw_icon();
        let _ = self.tray.set_tooltip(Some(self.snap.tooltip()));
        flyout::set_state(&self.snap, autostart_enabled());
    }

    /// Returns false when the app should exit.
    fn on_action(&mut self, a: Action) -> bool {
        match a {
            Action::Quit => return false,
            Action::Refresh => {
                let _ = self.cmd.send(Command::Refresh);
            }
            Action::ToggleAutostart => {
                set_autostart(!autostart_enabled());
                flyout::set_state(&self.snap, autostart_enabled());
            }
            Action::SetAll(dev) => {
                let _ = self.cmd.send(Command::SetAll(dev));
            }
            Action::SetChannel(ch, dev) => {
                let _ = self.cmd.send(Command::SetChannel(ch, dev));
            }
        }
        true
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 3 && args[1] == "--dump-icons" {
        icons::dump(&args[2]);
        return;
    }
    // Debug aid: `--show-flyout X Y` opens the flyout pinned at a screen point.
    let pinned_at = (args.len() == 4 && args[1] == "--show-flyout")
        .then(|| (args[2].parse().unwrap_or(0), args[3].parse().unwrap_or(0)));

    unsafe {
        let _mutex = CreateMutexW(None, true, w!("Local\\SonarTray_SingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    flyout::init().expect("create flyout window");

    let main_thread = unsafe { GetCurrentThreadId() };
    let (snap_tx, snap_rx) = mpsc::channel::<Snapshot>();
    let cmd = worker::spawn(move |s| {
        let _ = snap_tx.send(s);
        unsafe {
            let _ = PostThreadMessageW(main_thread, WM_APP, WPARAM(0), LPARAM(0));
        }
    });

    let tray = TrayIconBuilder::new()
        .with_icon(icons::render(Status::Unknown, light_taskbar(), icon_size()))
        .with_tooltip("Audio output: starting...")
        .build()
        .expect("create tray icon");
    let mut app = App { tray, cmd, snap: Snapshot::offline(), drawn: None };
    flyout::set_state(&app.snap, autostart_enabled());
    let mut pending_pin = pinned_at;

    unsafe {
        // Wake up regularly: picks up theme/DPI changes and closes the flyout
        // if it lost focus without us being told.
        SetTimer(None, 0, 250, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);

            if let Some(latest) = snap_rx.try_iter().last() {
                app.update(latest);
                if let Some(at) = pending_pin.take() {
                    flyout::show_pinned(at);
                }
            }
            while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                if let TrayIconEvent::Click { rect, button_state: MouseButtonState::Up, .. } = ev {
                    let x = rect.position.x as i32 + rect.size.width as i32 / 2;
                    let y = rect.position.y as i32;
                    flyout::toggle((x, y));
                }
            }
            for a in flyout::take_actions() {
                if !app.on_action(a) {
                    return;
                }
            }
            flyout::tick();
            app.redraw_icon();
        }
    }
}
