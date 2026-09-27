//! On-screen popups, drawn with GDI on the tray thread as click-through layered windows:
//! the volume popup, and the cheat sheet that lists what every control does.
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Default, Debug)]
pub enum Icon {
    #[default]
    None,
    Exe(String), // full path
    Speaker,
    Mic,
    Profile,
    Light,
}

/// One control on the cheat sheet.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct SheetItem {
    /// "K1", "S3"
    pub tag: String,
    /// Its label, or what turning it does; empty when unused.
    pub title: String,
    /// What turning does (when the title is a label), then its press, double press and hold.
    pub lines: Vec<String>,
    /// The control's light color.
    pub color: [u8; 3],
    pub knob: bool,
}

#[derive(Clone, Default, Debug)]
pub struct Info {
    pub title: String,
    pub exe_name: bool, // title is a fallback; prefer the exe's friendly name
    pub icon: Icon,
    pub level: Option<f32>,
    /// Where the physical control is while it hasn't picked up the volume yet.
    pub marker: Option<f32>,
    pub muted: bool,
    pub hint: String,
    pub top: bool,
    /// Cheat sheet cards, one per control. Shown instead of the volume popup.
    pub sheet: Vec<SheetItem>,
    /// How long to show it (0 = the usual moment).
    pub stay_ms: u64,
    /// Fade out the cheat sheet now.
    pub hide: bool,
}

const TIMER: usize = 1;
const SHOW_FOR: Duration = Duration::from_millis(1300);

struct State {
    hwnd: HWND,
    dc: HDC,
    bmp: HBITMAP,
    size: SIZE,
    pos: POINT,
    alpha: u8,
    until: Instant,
    names: HashMap<String, String>,
    icons: HashMap<String, HICON>,
}

thread_local! {
    /// [volume popup, cheat sheet]: separate windows, so a volume change doesn't cover the sheet.
    static STATE: RefCell<Vec<State>> = const { RefCell::new(Vec::new()) };
}

pub fn init() {
    unsafe {
        let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst.into(), lpszClassName: w!("PCPanelReviveOSD"), ..Default::default() };
        RegisterClassW(&class);
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        for _ in 0..2 {
            let Ok(hwnd) = CreateWindowExW(ex, w!("PCPanelReviveOSD"), w!(""), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst.into()), None) else { return };
            STATE.with(|s| s.borrow_mut().push(State {
                hwnd, dc: HDC::default(), bmp: HBITMAP::default(), size: SIZE::default(), pos: POINT::default(), alpha: 0,
                until: Instant::now(), names: HashMap::new(), icons: HashMap::new(),
            }));
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_TIMER {
        STATE.with(|s| {
            if let Some(st) = s.borrow_mut().iter_mut().find(|st| st.hwnd == hwnd) {
                // Fade in fast, hold, fade out.
                let target = if Instant::now() < st.until { 255 } else { 0 };
                let alpha = match target {
                    255 => st.alpha.saturating_add(85),
                    _ => st.alpha.saturating_sub(20),
                };
                // Only redraw while fading; at full opacity it holds still.
                if alpha != st.alpha {
                    st.alpha = alpha;
                    present(st);
                }
                if st.alpha == 0 {
                    let _ = KillTimer(Some(hwnd), TIMER);
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
        });
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

unsafe fn present(st: &State) {
    let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: st.alpha, AlphaFormat: AC_SRC_ALPHA as u8 };
    let src = POINT::default();
    let _ = UpdateLayeredWindow(st.hwnd, None, Some(&st.pos), Some(&st.size), Some(st.dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

pub fn show(info: &Info) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let sheet = !info.sheet.is_empty() || info.hide;
        let Some(st) = s.get_mut(sheet as usize) else { return };
        if info.hide {
            st.until = Instant::now(); // the timer fades it out
            return;
        }
        unsafe {
            if sheet { render_sheet(st, info) } else { render(st, info) }
        }
        st.until = Instant::now() + if info.stay_ms > 0 { Duration::from_millis(info.stay_ms) } else { SHOW_FOR };
        unsafe {
            present(st);
            let _ = ShowWindow(st.hwnd, SW_SHOWNOACTIVATE);
            SetTimer(Some(st.hwnd), TIMER, 16, None);
        }
    });
}

unsafe fn render(st: &mut State, info: &Info) {
    let k = GetDpiForSystem() as f32 / 96.0;
    let px = |v: f32| (v * k).round() as i32;
    let (w, h) = (px(340.0), px(78.0));

    // Friendly name and icon come from the exe, cached per path.
    let mut title = info.title.clone();
    let mut hicon = HICON::default();
    if let Icon::Exe(path) = &info.icon {
        if info.exe_name {
            title = st.names.entry(path.clone()).or_insert_with(|| crate::apps::friendly_name(path)).clone();
        }
        hicon = *st.icons.entry(path.clone()).or_insert_with(|| {
            let mut icons = [HICON::default()];
            let mut name = [0u16; 260];
            let p = wide(path);
            name[..p.len().min(259)].copy_from_slice(&p[..p.len().min(259)]);
            PrivateExtractIconsW(&name, 0, px(40.0), px(40.0), Some(&mut icons), None, 0);
            icons[0]
        });
    }

    let bits = canvas(st, w, h);

    // Icon (app icon, or a Segoe MDL2 glyph).
    let (ix, iy, isz) = (px(18.0), px(19.0), px(40.0));
    let glyph = match info.icon {
        Icon::Speaker => Some(if info.muted { '\u{E74F}' } else { '\u{E767}' }),
        Icon::Mic => Some(if info.muted { '\u{EC54}' } else { '\u{E720}' }),
        Icon::Profile => Some('\u{E8F1}'),
        Icon::Light => Some('\u{E706}'),
        Icon::Exe(_) if hicon.is_invalid() => Some('\u{E768}'),
        _ => None,
    };
    if !hicon.is_invalid() {
        let _ = DrawIconEx(st.dc, ix, iy, hicon, isz, isz, 0, None, DI_NORMAL);
    } else if let Some(g) = glyph {
        let font = CreateFontW(px(30.0), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe MDL2 Assets"));
        let old = SelectObject(st.dc, font.into());
        SetTextColor(st.dc, rgb(232, 232, 238));
        let mut r = RECT { left: ix, top: iy, right: ix + isz, bottom: iy + isz };
        let mut t: Vec<u16> = g.to_string().encode_utf16().collect();
        DrawTextW(st.dc, &mut t, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        SelectObject(st.dc, old);
        let _ = DeleteObject(font.into());
    }

    // Title, and level or hint on the right.
    let x0 = px(72.0);
    let right = w - px(18.0);
    let font = CreateFontW(px(17.0), 0, 0, 0, 600, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
    let small = CreateFontW(px(14.0), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
    let old = SelectObject(st.dc, font.into());
    let side = if !info.hint.is_empty() {
        info.hint.clone()
    } else if info.muted {
        "Muted".into()
    } else {
        info.level.map(|l| format!("{}%", (l * 100.0).round() as i32)).unwrap_or_default()
    };
    SelectObject(st.dc, if info.hint.is_empty() { font.into() } else { small.into() });
    SetTextColor(st.dc, if !info.hint.is_empty() { rgb(255, 196, 92) } else if info.muted { rgb(255, 99, 88) } else { rgb(200, 200, 210) });
    let mut rs = RECT { left: x0, top: px(14.0), right, bottom: px(38.0) };
    let mut t: Vec<u16> = side.encode_utf16().collect();
    DrawTextW(st.dc, &mut t, &mut rs, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
    let mut measure = RECT::default();
    DrawTextW(st.dc, &mut t, &mut measure, DT_CALCRECT | DT_SINGLELINE);
    SelectObject(st.dc, font.into());
    SetTextColor(st.dc, rgb(240, 240, 245));
    let mut rt = RECT { left: x0, top: px(14.0), right: right - (measure.right - measure.left) - px(10.0), bottom: px(38.0) };
    let mut t: Vec<u16> = title.encode_utf16().collect();
    DrawTextW(st.dc, &mut t, &mut rt, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
    SelectObject(st.dc, old);
    let _ = DeleteObject(font.into());
    let _ = DeleteObject(small.into());

    // Level bar with optional pickup marker.
    if let Some(level) = info.level {
        let (by, bh) = (px(48.0), px(6.0));
        let fill_to = x0 + ((right - x0) as f32 * level.clamp(0.0, 1.0)) as i32;
        let track = CreateSolidBrush(rgb(58, 58, 68));
        let fill = CreateSolidBrush(if info.muted { rgb(110, 110, 120) } else { rgb(79, 157, 255) });
        let pen = GetStockObject(NULL_PEN);
        let oldpen = SelectObject(st.dc, pen);
        let oldbr = SelectObject(st.dc, track.into());
        let _ = RoundRect(st.dc, x0, by, right, by + bh, bh, bh);
        SelectObject(st.dc, fill.into());
        if fill_to > x0 {
            let _ = RoundRect(st.dc, x0, by, fill_to.max(x0 + bh), by + bh, bh, bh);
        }
        if let Some(m) = info.marker {
            let white = CreateSolidBrush(rgb(255, 196, 92));
            SelectObject(st.dc, white.into());
            let mx = x0 + ((right - x0) as f32 * m.clamp(0.0, 1.0)) as i32;
            let _ = RoundRect(st.dc, mx - px(2.0), by - px(5.0), mx + px(2.0), by + bh + px(5.0), px(3.0), px(3.0));
            SelectObject(st.dc, fill.into());
            let _ = DeleteObject(white.into());
        }
        SelectObject(st.dc, oldbr);
        SelectObject(st.dc, oldpen);
        let _ = DeleteObject(track.into());
        let _ = DeleteObject(fill.into());
    }

    card_edges(bits, w, h, px(14.0) as f32);

    // Center on the primary work area, above the taskbar or below the top edge.
    let work = work_area();
    let x = work.left + (work.right - work.left - w) / 2;
    let y = if info.top { work.top + px(28.0) } else { work.bottom - h - px(48.0) };
    st.pos = POINT { x, y };
}

fn work_area() -> RECT {
    let mut work = RECT::default();
    unsafe { let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)); }
    work
}

/// A fresh 32-bit top-down canvas of w x h, filled with the card color. Returns its pixels.
unsafe fn canvas(st: &mut State, w: i32, h: i32) -> *mut std::ffi::c_void {
    if !st.dc.is_invalid() {
        let _ = DeleteDC(st.dc);
        let _ = DeleteObject(st.bmp.into());
    }
    let screen = GetDC(None);
    st.dc = CreateCompatibleDC(Some(screen));
    let mut bi = BITMAPINFO::default();
    bi.bmiHeader = BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() };
    let mut bits = std::ptr::null_mut();
    st.bmp = CreateDIBSection(Some(screen), &bi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
    ReleaseDC(None, screen);
    SelectObject(st.dc, st.bmp.into());
    st.size = SIZE { cx: w, cy: h };
    let brush = CreateSolidBrush(rgb(30, 30, 36));
    FillRect(st.dc, &RECT { left: 0, top: 0, right: w, bottom: h }, brush);
    let _ = DeleteObject(brush.into());
    SetBkMode(st.dc, TRANSPARENT);
    bits
}

/// GDI leaves alpha at 0: make the card opaque with anti-aliased rounded corners and a hairline border.
unsafe fn card_edges(bits: *mut std::ffi::c_void, w: i32, h: i32, r: f32) {
    let _ = GdiFlush();
    let pixels = std::slice::from_raw_parts_mut(bits as *mut [u8; 4], (w * h) as usize);
    let border = rgb(56, 56, 66).0;
    let [br, bgc, bb] = [(border & 0xff) as f32, ((border >> 8) & 0xff) as f32, ((border >> 16) & 0xff) as f32];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let dx = (r - fx).max(fx - (w as f32 - r)).max(0.0);
            let dy = (r - fy).max(fy - (h as f32 - r)).max(0.0);
            let d = (dx * dx + dy * dy).sqrt() - r; // signed distance to the rounded edge (negative inside)
            let edge = if dx > 0.0 && dy > 0.0 { d } else { -(fx.min(fy).min(w as f32 - fx).min(h as f32 - fy)) };
            let a = (0.5 - edge).clamp(0.0, 1.0);
            let p = &mut pixels[(y * w + x) as usize];
            if edge > -1.5 {
                let t = (edge + 1.5).clamp(0.0, 1.0) * 0.9; // hairline border
                p[0] = (p[0] as f32 * (1.0 - t) + bb * t) as u8;
                p[1] = (p[1] as f32 * (1.0 - t) + bgc * t) as u8;
                p[2] = (p[2] as f32 * (1.0 - t) + br * t) as u8;
            }
            // Premultiplied alpha.
            for c in 0..3 {
                p[c] = (p[c] as f32 * a) as u8;
            }
            p[3] = (a * 255.0) as u8;
        }
    }
}

/// One line of text in a box, with "..." when it doesn't fit.
unsafe fn text(dc: HDC, s: &str, r: RECT, color: COLORREF, font: HFONT, flags: DRAW_TEXT_FORMAT) {
    if s.is_empty() {
        return; // DrawTextW with an empty buffer and an ellipsis flag crashes
    }
    SelectObject(dc, font.into());
    SetTextColor(dc, color);
    let mut r = r;
    let mut t: Vec<u16> = s.encode_utf16().collect();
    DrawTextW(dc, &mut t, &mut r, flags | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
}

/// The cheat sheet: a map of the panel, with a card for each control where it sits on the device
/// (Pro: two knobs, three knobs, four sliders; Mini and original: one row of four), centered on screen.
unsafe fn render_sheet(st: &mut State, info: &Info) {
    let k = GetDpiForSystem() as f32 / 96.0;
    let px = |v: f32| (v * k).round() as i32;
    let n = info.sheet.len();
    let rows: Vec<Vec<usize>> = if n > 4 { vec![vec![0, 1], vec![2, 3, 4], (5..n).collect()] } else { vec![(0..n).collect()] };
    // Cards are as tall as the busiest one needs: tag, name, then up to four lines.
    let most = info.sheet.iter().map(|it| it.lines.len().min(4)).max().unwrap_or(0).max(1) as f32;
    let (cw, ch, gap, pad, head, split) = (px(222.0), px(72.0 + 19.0 * most), px(12.0), px(20.0), px(56.0), px(14.0));
    let w = pad * 2 + cw * 4 + gap * 3;
    let h = head + rows.len() as i32 * (ch + gap) - gap + if n > 4 { split } else { 0 } + pad;
    let bits = canvas(st, w, h);
    let font = |size: f32, weight: i32| CreateFontW(px(size), 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
    let (title_f, tag_f, name_f, line_f) = (font(19.0, 600), font(14.0, 700), font(15.0, 600), font(13.0, 400));
    let old = SelectObject(st.dc, title_f.into());
    text(st.dc, &info.title, RECT { left: pad, top: px(12.0), right: w - pad - px(200.0), bottom: px(42.0) }, rgb(240, 240, 245), title_f, DT_LEFT);
    text(st.dc, &info.hint, RECT { left: w - pad - px(200.0), top: px(12.0), right: w - pad, bottom: px(42.0) }, rgb(140, 140, 152), line_f, DT_RIGHT);
    let card = CreateSolidBrush(rgb(38, 38, 47));
    let null_pen = GetStockObject(NULL_PEN);
    let mut y = head;
    for (r, row) in rows.iter().enumerate() {
        if r == 2 {
            y += split; // a little space between the knobs and the sliders, as on the panel
        }
        let row_w = row.len() as i32 * cw + (row.len() as i32 - 1) * gap;
        let mut x = (w - row_w) / 2;
        for &idx in row {
            let it = &info.sheet[idx];
            let color = rgb(it.color[0], it.color[1], it.color[2]);
            SelectObject(st.dc, null_pen);
            SelectObject(st.dc, card.into());
            let _ = RoundRect(st.dc, x, y, x + cw, y + ch, px(12.0), px(12.0));
            // The control's shape in its light color: a ring for a knob, a bar for a slider.
            let (ix, iy) = (x + px(12.0), y + px(12.0));
            if it.knob {
                let pen = CreatePen(PS_SOLID, px(3.0), color);
                SelectObject(st.dc, pen.into());
                SelectObject(st.dc, GetStockObject(NULL_BRUSH));
                let _ = Ellipse(st.dc, ix, iy, ix + px(16.0), iy + px(16.0));
                SelectObject(st.dc, null_pen);
                let _ = DeleteObject(pen.into());
            } else {
                let bar = CreateSolidBrush(color);
                SelectObject(st.dc, bar.into());
                let _ = RoundRect(st.dc, ix + px(5.0), iy - px(1.0), ix + px(11.0), iy + px(17.0), px(3.0), px(3.0));
                let _ = DeleteObject(bar.into());
            }
            // The tag in a lighter shade of the light's color, so dark colors stay readable.
            let light = |v: u8| (v as u32 + (255 - v as u32) * 2 / 5) as u8;
            let tag_c = rgb(light(it.color[0]), light(it.color[1]), light(it.color[2]));
            text(st.dc, &it.tag, RECT { left: x + px(36.0), top: y + px(8.0), right: x + cw - px(10.0), bottom: y + px(32.0) }, tag_c, tag_f, DT_LEFT);
            let unset = it.title.is_empty() && it.lines.is_empty();
            let (name, name_c) = if unset { ("Not used", rgb(110, 110, 120)) } else { (it.title.as_str(), rgb(240, 240, 245)) };
            text(st.dc, name, RECT { left: x + px(12.0), top: y + px(36.0), right: x + cw - px(10.0), bottom: y + px(60.0) }, name_c, name_f, DT_LEFT);
            for (l, line) in it.lines.iter().take(4).enumerate() {
                let top = y + px(62.0) + px(19.0) * l as i32;
                text(st.dc, line, RECT { left: x + px(12.0), top, right: x + cw - px(10.0), bottom: top + px(19.0) }, rgb(170, 170, 182), line_f, DT_LEFT);
            }
            x += cw + gap;
        }
        y += ch + gap;
    }
    SelectObject(st.dc, old);
    for f in [title_f, tag_f, name_f, line_f] {
        let _ = DeleteObject(f.into());
    }
    let _ = DeleteObject(card.into());
    card_edges(bits, w, h, px(14.0) as f32);
    let work = work_area();
    st.pos = POINT { x: work.left + (work.right - work.left - w) / 2, y: work.top + (work.bottom - work.top - h) / 2 };
}
