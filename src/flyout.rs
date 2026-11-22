//! Windows 11 style flyout (instead of the native context menu), drawn with
//! Direct2D + DirectWrite on a borderless popup with DWM rounded corners and,
//! where available, the acrylic "transient window" backdrop.
//!
//! All state lives in a thread-local on the UI thread; clicks are turned into
//! `Action`s that the main loop drains after dispatching each message.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_POINT_2F, D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget,
    ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, ValidateRect,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetForegroundWindow, HWND_TOPMOST, IDC_ARROW, LoadCursorW, RegisterClassW,
    SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SetForegroundWindow, SetWindowPos, ShowWindow, WA_INACTIVE, WM_ACTIVATE,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{HSTRING, Result, w};

use crate::model::{Backend, Kind, Snapshot, Status, channel_label, short_name};
use crate::sonar::Mode;

// Segoe Fluent Icons / Segoe MDL2 Assets code points.
const G_SPEAKER: char = '\u{E7F5}';
const G_HEADSET: char = '\u{E7F6}';
const G_MIXED: char = '\u{E767}';
const G_MUTE: char = '\u{E74F}';
const G_CHECK: char = '\u{E73E}';
const G_DOWN: char = '\u{E70D}';
const G_UP: char = '\u{E70E}';
const G_REFRESH: char = '\u{E72C}';
const G_POWER: char = '\u{E7E8}';

const WIDTH: f32 = 360.0;
const WM_MOUSELEAVE: u32 = 0x02A3;
const MARGIN: i32 = 12; // gap to the taskbar / screen edge, logical px

pub enum Action {
    SetAll(String),
    SetChannel(String, String),
    Refresh,
    ToggleAutostart,
    Quit,
}

#[derive(Clone, PartialEq, Debug)]
enum Hit {
    Device(String),
    Channel(String),
    ChannelDevice(String, String),
    Autostart,
    Refresh,
    Quit,
}

#[derive(Clone, Copy, Debug)]
struct R {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl R {
    fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        R { x, y, w, h }
    }
    fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
}

enum El {
    Header { r: R, glyph: char, title: String, subtitle: String },
    Label { r: R, text: String },
    Note { r: R, text: String },
    Row {
        r: R,
        indent: f32,
        glyph: Option<char>,
        text: String,
        trailing: Option<String>,
        chevron: Option<bool>,
        check: bool,
        selected: bool,
        hit: Hit,
    },
    Footer { r: R },
}

#[derive(Clone, Copy)]
struct Theme {
    dark: bool,
    fill: D2D1_COLOR_F,
    text: D2D1_COLOR_F,
    text2: D2D1_COLOR_F,
    hover: D2D1_COLOR_F,
    selected: D2D1_COLOR_F,
    accent: D2D1_COLOR_F,
    on_accent: D2D1_COLOR_F,
    divider: D2D1_COLOR_F,
    footer: D2D1_COLOR_F,
}

fn c(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
}

/// Windows accent colour, picked like the shell does: a lighter shade in dark
/// mode, a darker shade in light mode.
fn accent(dark: bool) -> (u8, u8, u8) {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};
    let palette = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent")
        .and_then(|k| k.get_raw_value("AccentPalette"));
    match palette {
        Ok(v) if v.bytes.len() >= 32 => {
            let i = if dark { 1 } else { 4 } * 4;
            (v.bytes[i], v.bytes[i + 1], v.bytes[i + 2])
        }
        _ => if dark { (96, 205, 255) } else { (0, 95, 184) },
    }
}

fn theme() -> Theme {
    let dark = !crate::light_taskbar();
    let (ar, ag, ab) = accent(dark);
    if dark {
        Theme {
            dark,
            fill: c(40, 40, 40, 1.0),
            text: c(255, 255, 255, 1.0),
            text2: c(255, 255, 255, 0.62),
            hover: c(255, 255, 255, 0.06),
            selected: c(255, 255, 255, 0.09),
            accent: c(ar, ag, ab, 1.0),
            on_accent: c(0, 0, 0, 1.0),
            divider: c(255, 255, 255, 0.08),
            footer: c(0, 0, 0, 0.18),
        }
    } else {
        Theme {
            dark,
            fill: c(243, 243, 243, 1.0),
            text: c(0, 0, 0, 0.9),
            text2: c(0, 0, 0, 0.6),
            hover: c(0, 0, 0, 0.04),
            selected: c(0, 0, 0, 0.06),
            accent: c(ar, ag, ab, 1.0),
            on_accent: c(255, 255, 255, 1.0),
            divider: c(0, 0, 0, 0.08),
            footer: c(0, 0, 0, 0.03),
        }
    }
}

struct Fonts {
    title: IDWriteTextFormat,
    body: IDWriteTextFormat,
    small: IDWriteTextFormat,
    label: IDWriteTextFormat,
    icon20: IDWriteTextFormat,
    icon16: IDWriteTextFormat,
    icon12: IDWriteTextFormat,
}

fn font_exists(dw: &IDWriteFactory, name: &str) -> bool {
    unsafe {
        let mut coll = None;
        if dw.GetSystemFontCollection(&mut coll, false).is_err() {
            return false;
        }
        let Some(coll) = coll else { return false };
        let (mut idx, mut exists) = (0u32, BOOL(0));
        coll.FindFamilyName(&HSTRING::from(name), &mut idx, &mut exists).is_ok() && exists.as_bool()
    }
}

fn make_format(dw: &IDWriteFactory, family: &str, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<IDWriteTextFormat> {
    unsafe {
        let f = dw.CreateTextFormat(
            &HSTRING::from(family),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            &HSTRING::from(""),
        )?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let sign = dw.CreateEllipsisTrimmingSign(&f)?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        f.SetTrimming(&trim, &sign)?;
        Ok(f)
    }
}

impl Fonts {
    fn new(dw: &IDWriteFactory) -> Result<Self> {
        let text = if font_exists(dw, "Segoe UI Variable Text") { "Segoe UI Variable Text" } else { "Segoe UI" };
        let display = if font_exists(dw, "Segoe UI Variable Display") { "Segoe UI Variable Display" } else { text };
        let icons = if font_exists(dw, "Segoe Fluent Icons") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" };
        Ok(Fonts {
            title: make_format(dw, display, 16.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            body: make_format(dw, text, 14.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            small: make_format(dw, text, 12.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            label: make_format(dw, text, 12.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            icon20: make_format(dw, icons, 20.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            icon16: make_format(dw, icons, 16.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            icon12: make_format(dw, icons, 12.0, DWRITE_FONT_WEIGHT_NORMAL)?,
        })
    }
}

pub struct Flyout {
    hwnd: HWND,
    d2d: ID2D1Factory,
    rt: Option<ID2D1HwndRenderTarget>,
    fonts: Fonts,
    acrylic: bool,
    theme: Theme,

    snap: Snapshot,
    autostart: bool,
    expanded: Option<String>,
    hover: Option<Hit>,
    tracking: bool,

    els: Vec<El>,
    hits: Vec<(R, Hit)>,
    height: f32,

    anchor: (i32, i32),
    dpi: u32,
    visible: bool,
    shown_at: Instant,
    hidden_at: Option<Instant>,
    pinned: bool,
}

thread_local! {
    static FLY: RefCell<Option<Flyout>> = const { RefCell::new(None) };
    static ACTIONS: RefCell<Vec<Action>> = const { RefCell::new(Vec::new()) };
}

fn push(a: Action) {
    ACTIONS.with(|q| q.borrow_mut().push(a));
}

fn with<T>(f: impl FnOnce(&mut Flyout) -> T) -> Option<T> {
    FLY.with(|cell| cell.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// Create the (hidden) flyout window. Call once on the UI thread.
pub fn init() -> Result<()> {
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("AudioOutputDeviceSwitcherFlyout");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            w!("Audio Output Device Switcher"),
            WS_POPUP,
            0,
            0,
            WIDTH as i32,
            200,
            None,
            None,
            hinst,
            None,
        )?;

        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, 4);
        let backdrop = DWMSBT_TRANSIENTWINDOW;
        let acrylic = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &backdrop as *const _ as _, 4).is_ok();
        if acrylic {
            let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        }

        let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let fonts = Fonts::new(&dw)?;

        let fly = Flyout {
            hwnd,
            d2d,
            rt: None,
            fonts,
            acrylic,
            theme: theme(),
            snap: Snapshot::offline(),
            autostart: false,
            expanded: None,
            hover: None,
            tracking: false,
            els: vec![],
            hits: vec![],
            height: 200.0,
            anchor: (0, 0),
            dpi: 96,
            visible: false,
            shown_at: Instant::now(),
            hidden_at: None,
            pinned: false,
        };
        FLY.with(|cell| *cell.borrow_mut() = Some(fly));
    }
    Ok(())
}

pub fn set_state(snap: &Snapshot, autostart: bool) {
    with(|f| {
        f.snap = snap.clone();
        f.autostart = autostart;
        if let Some(ch) = &f.expanded {
            if !f.snap.channels.iter().any(|(c, _)| c == ch) {
                f.expanded = None;
            }
        }
        if f.visible {
            f.relayout();
        }
    });
}

/// Show at the tray icon (physical screen coords), or hide if already open.
pub fn toggle(anchor: (i32, i32)) {
    with(|f| {
        if f.visible {
            f.hide();
        } else if f.hidden_at.is_some_and(|t| t.elapsed() < Duration::from_millis(300)) {
            // This click is what just closed it (focus loss); don't reopen.
        } else {
            f.show(anchor);
        }
    });
}

/// Debug: open and keep open regardless of focus.
pub fn show_pinned(anchor: (i32, i32)) {
    with(|f| {
        f.pinned = true;
        f.show(anchor);
    });
}

/// Called periodically: closes the flyout if it never got / lost focus.
pub fn tick() {
    with(|f| {
        if f.visible && !f.pinned && f.shown_at.elapsed() > Duration::from_millis(600) {
            if unsafe { GetForegroundWindow() } != f.hwnd {
                f.hide();
            }
        }
    });
}

pub fn take_actions() -> Vec<Action> {
    ACTIONS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

fn glyph_for(kind: Kind) -> char {
    match kind {
        Kind::Headset => G_HEADSET,
        Kind::Speakers => G_SPEAKER,
    }
}

impl Flyout {
    fn show(&mut self, anchor: (i32, i32)) {
        self.anchor = anchor;
        self.expanded = None;
        self.hover = None;
        self.theme = theme();
        unsafe {
            let dark = BOOL(self.theme.dark as i32);
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
            // Subtle 1px border like other shell flyouts.
            let border = if self.theme.dark { COLORREF(0x00_3a_3a_3a) } else { COLORREF(0x00_d8_d8_d8) };
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_BORDER_COLOR, &border as *const _ as _, 4);
        }
        self.layout();
        self.place();
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
        }
        self.visible = true;
        self.shown_at = Instant::now();
        self.invalidate();
    }

    fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        self.pinned = false;
        self.hidden_at = Some(Instant::now());
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    fn relayout(&mut self) {
        let old = self.height;
        self.layout();
        if (self.height - old).abs() > 0.5 {
            self.place();
        }
        self.invalidate();
    }

    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    /// Position next to the taskbar, near the tray icon.
    fn place(&mut self) {
        unsafe {
            let mon = MonitorFromPoint(POINT { x: self.anchor.0, y: self.anchor.1 }, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            self.dpi = dx.max(96);
            let s = self.scale();
            let (w, h) = ((WIDTH * s).round() as i32, (self.height * s).round() as i32);
            let gap = (MARGIN as f32 * s) as i32;
            let (work, full): (RECT, RECT) = (mi.rcWork, mi.rcMonitor);
            let clamp = |v: i32, lo: i32, hi: i32| v.max(lo).min(hi.max(lo));

            let (x, y) = if work.bottom < full.bottom || (work.top == full.top && work.left == full.left && work.right == full.right) {
                // Taskbar at the bottom (or auto-hidden).
                (clamp(self.anchor.0 - w / 2, work.left + gap, work.right - w - gap), work.bottom - h - gap)
            } else if work.top > full.top {
                (clamp(self.anchor.0 - w / 2, work.left + gap, work.right - w - gap), work.top + gap)
            } else if work.left > full.left {
                (work.left + gap, clamp(self.anchor.1 - h / 2, work.top + gap, work.bottom - h - gap))
            } else {
                (work.right - w - gap, clamp(self.anchor.1 - h / 2, work.top + gap, work.bottom - h - gap))
            };
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
            if let Some(rt) = &self.rt {
                rt.SetDpi(self.dpi as f32, self.dpi as f32);
                let _ = rt.Resize(&D2D_SIZE_U { width: w as u32, height: h as u32 });
            }
        }
    }

    fn layout(&mut self) {
        let s = &self.snap;
        let mut els = Vec::new();
        let mut hits = Vec::new();
        let mut y = 4.0;

        let current = s.all_device().and_then(|id| s.device(id));
        let (glyph, title) = match s.status() {
            Status::Headset | Status::Speakers => {
                let d = current.expect("status implies device");
                (glyph_for(d.kind), d.name.clone())
            }
            Status::Mixed => (G_MIXED, "Mixed output".to_string()),
            Status::Unknown => (G_MUTE, "No output device".to_string()),
        };
        let subtitle = match s.backend {
            Backend::Sonar(Mode::Classic) => "SteelSeries Sonar \u{b7} Classic mode",
            Backend::Sonar(Mode::Streamer) => "SteelSeries Sonar \u{b7} Streamer mode",
            Backend::Windows => "Windows default device \u{b7} Sonar not running",
            Backend::Offline => "No audio backend found",
        };
        els.push(El::Header { r: R::new(0.0, y, WIDTH, 72.0), glyph, title, subtitle: subtitle.into() });
        y += 72.0;

        let section = match s.backend {
            Backend::Sonar(Mode::Classic) => "All output devices",
            Backend::Sonar(Mode::Streamer) => "Monitoring \u{b7} what you hear",
            _ => "Output device",
        };
        els.push(El::Label { r: R::new(20.0, y, WIDTH - 40.0, 28.0), text: section.into() });
        y += 28.0;

        let all = s.all_device();
        for d in &s.devices {
            let sel = all == Some(d.id.as_str());
            let r = R::new(8.0, y, WIDTH - 16.0, 40.0);
            let hit = Hit::Device(d.id.clone());
            hits.push((r, hit.clone()));
            els.push(El::Row {
                r,
                indent: 0.0,
                glyph: Some(glyph_for(d.kind)),
                text: d.name.clone(),
                trailing: None,
                chevron: None,
                check: sel,
                selected: sel,
                hit,
            });
            y += 42.0;
        }
        if s.devices.is_empty() {
            els.push(El::Note { r: R::new(20.0, y, WIDTH - 40.0, 28.0), text: "No playback devices found".into() });
            y += 28.0;
        }
        if s.status() == Status::Mixed {
            els.push(El::Note { r: R::new(20.0, y, WIDTH - 40.0, 24.0), text: "Channels go to different devices".into() });
            y += 24.0;
        }

        if let Backend::Sonar(mode) = s.backend {
            y += 6.0;
            let label = if mode == Mode::Streamer { "Stream mix" } else { "Channels" };
            els.push(El::Label { r: R::new(20.0, y, WIDTH - 40.0, 28.0), text: label.into() });
            y += 28.0;
            // In streamer mode the monitoring mix is already the list above.
            let channels = s.channels.iter().filter(|(ch, _)| !(mode == Mode::Streamer && ch == "monitoring"));
            for (ch, dev) in channels {
                let expanded = self.expanded.as_deref() == Some(ch.as_str());
                let name = s.any_name(dev).map(|n| short_name(&n).to_string()).unwrap_or_else(|| "None".into());
                let r = R::new(8.0, y, WIDTH - 16.0, 38.0);
                let hit = Hit::Channel(ch.clone());
                hits.push((r, hit.clone()));
                els.push(El::Row {
                    r,
                    indent: 0.0,
                    glyph: None,
                    text: channel_label(ch).into(),
                    trailing: Some(name),
                    chevron: Some(expanded),
                    check: false,
                    selected: expanded,
                    hit,
                });
                y += 40.0;
                if expanded {
                    for d in &s.devices {
                        let r = R::new(8.0, y, WIDTH - 16.0, 36.0);
                        let hit = Hit::ChannelDevice(ch.clone(), d.id.clone());
                        hits.push((r, hit.clone()));
                        els.push(El::Row {
                            r,
                            indent: 20.0,
                            glyph: Some(glyph_for(d.kind)),
                            text: d.name.clone(),
                            trailing: None,
                            chevron: None,
                            check: *dev == d.id,
                            selected: false,
                            hit,
                        });
                        y += 38.0;
                    }
                }
            }
        }

        y += 10.0;
        let fr = R::new(0.0, y, WIDTH, 56.0);
        hits.push((R::new(8.0, y + 10.0, 210.0, 36.0), Hit::Autostart));
        hits.push((R::new(WIDTH - 88.0, y + 12.0, 32.0, 32.0), Hit::Refresh));
        hits.push((R::new(WIDTH - 48.0, y + 12.0, 32.0, 32.0), Hit::Quit));
        els.push(El::Footer { r: fr });
        y += 56.0;

        self.els = els;
        self.hits = hits;
        self.height = y;
    }

    fn hit_at(&self, lp: LPARAM) -> Option<Hit> {
        let s = self.scale();
        let x = (lp.0 & 0xFFFF) as i16 as f32 / s;
        let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s;
        self.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone())
    }

    fn click(&mut self, hit: Hit) {
        match hit {
            Hit::Device(id) => push(Action::SetAll(id)),
            Hit::Channel(ch) => {
                self.expanded = if self.expanded.as_deref() == Some(ch.as_str()) { None } else { Some(ch) };
                self.relayout();
            }
            Hit::ChannelDevice(ch, id) => {
                push(Action::SetChannel(ch, id));
                self.expanded = None;
                self.relayout();
            }
            Hit::Autostart => push(Action::ToggleAutostart),
            Hit::Refresh => push(Action::Refresh),
            Hit::Quit => {
                self.hide();
                push(Action::Quit);
            }
        }
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                if let Err(e) = self.paint() {
                    crate::worker::log(&format!("paint: {e}"));
                    self.rt = None;
                }
                unsafe {
                    let _ = ValidateRect(self.hwnd, None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_MOUSEMOVE => {
                if !self.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut tme);
                    }
                    self.tracking = true;
                }
                let h = self.hit_at(lp);
                if h != self.hover {
                    self.hover = h;
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                if self.hover.take().is_some() {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if let Some(h) = self.hit_at(lp) {
                    self.click(h);
                }
                Some(LRESULT(0))
            }
            WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
                self.hide();
                Some(LRESULT(0))
            }
            WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE && !self.pinned => {
                self.hide();
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    fn ensure_rt(&mut self) -> Result<ID2D1HwndRenderTarget> {
        if let Some(rt) = &self.rt {
            return Ok(rt.clone());
        }
        unsafe {
            let s = self.scale();
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 0.0,
                dpiY: 0.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: self.hwnd,
                pixelSize: D2D_SIZE_U {
                    width: (WIDTH * s).round() as u32,
                    height: (self.height * s).round() as u32,
                },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = self.d2d.CreateHwndRenderTarget(&props, &hprops)?;
            rt.SetDpi(self.dpi as f32, self.dpi as f32);
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            self.rt = Some(rt.clone());
            Ok(rt)
        }
    }

    fn paint(&mut self) -> Result<()> {
        let rt = self.ensure_rt()?;
        let t = self.theme;
        unsafe {
            rt.BeginDraw();
            let bg = if self.acrylic { c(0, 0, 0, 0.0) } else { t.fill };
            rt.Clear(Some(&bg));
            let brush = rt.CreateSolidColorBrush(&t.text, None)?;
            let p = Painter { rt: &rt, brush: &brush };
            for el in &self.els {
                self.draw_el(&p, el, &t);
            }
            rt.EndDraw(None, None)?;
        }
        Ok(())
    }

    fn draw_el(&self, p: &Painter, el: &El, t: &Theme) {
        let f = &self.fonts;
        match el {
            El::Header { r, glyph, title, subtitle } => {
                let tile = R::new(16.0, r.y + 16.0, 40.0, 40.0);
                p.fill_round(tile, 8.0, t.accent);
                p.text(&glyph.to_string(), &f.icon20, tile, t.on_accent, DWRITE_TEXT_ALIGNMENT_CENTER);
                p.text(title, &f.title, R::new(68.0, r.y + 14.0, WIDTH - 84.0, 24.0), t.text, DWRITE_TEXT_ALIGNMENT_LEADING);
                p.text(subtitle, &f.small, R::new(68.0, r.y + 38.0, WIDTH - 84.0, 18.0), t.text2, DWRITE_TEXT_ALIGNMENT_LEADING);
            }
            El::Label { r, text } => p.text(text, &f.label, *r, t.text2, DWRITE_TEXT_ALIGNMENT_LEADING),
            El::Note { r, text } => p.text(text, &f.small, *r, t.text2, DWRITE_TEXT_ALIGNMENT_LEADING),
            El::Row { r, indent, glyph, text, trailing, chevron, check, selected, hit } => {
                let hovered = self.hover.as_ref() == Some(hit);
                if *selected || hovered {
                    let mut col = if *selected { t.selected } else { t.hover };
                    if *selected && hovered {
                        col.a *= 1.5;
                    }
                    p.fill_round(*r, 4.0, col);
                }
                if *selected && chevron.is_none() {
                    p.fill_round(R::new(r.x, r.y + r.h / 2.0 - 8.0, 3.0, 16.0), 1.5, t.accent);
                }
                let mut x = r.x + 14.0 + indent;
                if let Some(g) = glyph {
                    let col = if *check { t.accent } else { t.text };
                    p.text(&g.to_string(), &f.icon16, R::new(x, r.y, 20.0, r.h), col, DWRITE_TEXT_ALIGNMENT_CENTER);
                    x += 32.0;
                }
                let right_reserved = if trailing.is_some() { 150.0 } else { 40.0 };
                p.text(text, &f.body, R::new(x, r.y, r.x + r.w - right_reserved - x, r.h), t.text, DWRITE_TEXT_ALIGNMENT_LEADING);
                if let Some(tr) = trailing {
                    p.text(tr, &f.body, R::new(r.x + r.w - 150.0, r.y, 112.0, r.h), t.text2, DWRITE_TEXT_ALIGNMENT_TRAILING);
                }
                if let Some(open) = chevron {
                    let g = if *open { G_UP } else { G_DOWN };
                    p.text(&g.to_string(), &f.icon12, R::new(r.x + r.w - 32.0, r.y, 20.0, r.h), t.text2, DWRITE_TEXT_ALIGNMENT_CENTER);
                }
                if *check {
                    p.text(&G_CHECK.to_string(), &f.icon16, R::new(r.x + r.w - 34.0, r.y, 20.0, r.h), t.accent, DWRITE_TEXT_ALIGNMENT_CENTER);
                }
            }
            El::Footer { r } => {
                p.fill_rect(*r, t.footer);
                p.fill_rect(R::new(r.x, r.y, r.w, 1.0), t.divider);
                for (hr, h) in &self.hits {
                    if self.hover.as_ref() == Some(h) && matches!(h, Hit::Autostart | Hit::Refresh | Hit::Quit) {
                        p.fill_round(*hr, 4.0, t.hover);
                    }
                }
                // Toggle switch.
                let sw = R::new(18.0, r.y + 18.0, 40.0, 20.0);
                let cy = sw.y + 10.0;
                if self.autostart {
                    p.fill_round(sw, 10.0, t.accent);
                    p.circle(sw.x + sw.w - 10.0, cy, 6.0, t.on_accent);
                } else {
                    p.stroke_round(sw, 10.0, t.text2, 1.0);
                    p.circle(sw.x + 10.0, cy, 5.0, t.text2);
                }
                p.text("Start with Windows", &f.body, R::new(68.0, r.y, 150.0, r.h), t.text, DWRITE_TEXT_ALIGNMENT_LEADING);
                p.text(&G_REFRESH.to_string(), &f.icon16, R::new(WIDTH - 88.0, r.y + 12.0, 32.0, 32.0), t.text, DWRITE_TEXT_ALIGNMENT_CENTER);
                p.text(&G_POWER.to_string(), &f.icon16, R::new(WIDTH - 48.0, r.y + 12.0, 32.0, 32.0), t.text, DWRITE_TEXT_ALIGNMENT_CENTER);
            }
        }
    }
}

struct Painter<'a> {
    rt: &'a ID2D1HwndRenderTarget,
    brush: &'a ID2D1SolidColorBrush,
}

impl Painter<'_> {
    fn color(&self, col: D2D1_COLOR_F) {
        unsafe { self.brush.SetColor(&col) }
    }
    fn fill_rect(&self, r: R, col: D2D1_COLOR_F) {
        self.color(col);
        unsafe { self.rt.FillRectangle(&r.d2d(), self.brush) }
    }
    fn fill_round(&self, r: R, rad: f32, col: D2D1_COLOR_F) {
        self.color(col);
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: rad, radiusY: rad };
        unsafe { self.rt.FillRoundedRectangle(&rr, self.brush) }
    }
    fn stroke_round(&self, r: R, rad: f32, col: D2D1_COLOR_F, width: f32) {
        self.color(col);
        let half = width / 2.0;
        let inner = R::new(r.x + half, r.y + half, r.w - width, r.h - width);
        let rr = D2D1_ROUNDED_RECT { rect: inner.d2d(), radiusX: rad - half, radiusY: rad - half };
        unsafe { self.rt.DrawRoundedRectangle(&rr, self.brush, width, None) }
    }
    fn circle(&self, x: f32, y: f32, rad: f32, col: D2D1_COLOR_F) {
        self.color(col);
        let e = D2D1_ELLIPSE { point: D2D_POINT_2F { x, y }, radiusX: rad, radiusY: rad };
        unsafe { self.rt.FillEllipse(&e, self.brush) }
    }
    fn text(&self, s: &str, fmt: &IDWriteTextFormat, r: R, col: D2D1_COLOR_F, align: DWRITE_TEXT_ALIGNMENT) {
        self.color(col);
        let wide: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let _ = fmt.SetTextAlignment(align);
            self.rt.DrawText(&wide, fmt, &r.d2d(), self.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match with(|f| f.handle(msg, wp, lp)).flatten() {
        Some(r) => r,
        None => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
