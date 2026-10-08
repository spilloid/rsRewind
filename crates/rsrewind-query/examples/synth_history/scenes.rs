//! The fictional desktops. Everything here is invented: Northwind Print Co. is a made-up company,
//! Kestrel a made-up printer line, Waypoint / Ledger / Parcel / Huddle made-up applications, every
//! domain ends in `.example` (reserved, RFC 2606), and nobody named exists.

use crate::draw::{Canvas, Face, Fonts, Rgb, hex};

pub const W: u32 = 1920;
pub const H: u32 = 1080;
const TASKBAR: i32 = 48;

/// A tiny deterministic generator (no extra dependency).
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

/// One drawn screen and what the recorder would have noted about it.
pub struct Scene {
    pub canvas: Canvas,
    pub process: &'static str,
    pub title: String,
}

/// Which kinds of screen a machine shows, and how often.
#[derive(Clone, Copy, Debug)]
pub enum Persona {
    /// The operations desk ("This machine"): terminal, spreadsheets, printer admin, mail.
    Ops,
    /// The front desk: printer pages, mail, chat, invoices.
    FrontDesk,
    /// The café kiosk: menu, orders, the odd chat.
    Kiosk,
}

impl Persona {
    pub fn wallpaper(self) -> (Rgb, Rgb, Rgb, (f32, f32)) {
        match self {
            Self::Ops => (hex(0x10131f), hex(0x1d1630), hex(0x3b4a8f), (0.78, 0.22)),
            Self::FrontDesk => (hex(0x0f1c1d), hex(0x14262a), hex(0x2f7f74), (0.2, 0.3)),
            Self::Kiosk => (hex(0x22140f), hex(0x2c1a14), hex(0xb8643a), (0.5, 0.15)),
        }
    }
}

pub struct Ctx<'a> {
    pub c: Canvas,
    pub f: &'a Fonts,
    pub r: &'a mut Rng,
}

impl Ctx<'_> {
    fn t(&mut self, face: Face, size: f32, x: i32, y: i32, text: &str, color: Rgb) -> i32 {
        self.c
            .text(self.f, face, size, x as f32, y as f32, text, color, true)
            .ceil() as i32
    }
    /// Text that is decoration, not something worth recognizing (icons, tiny labels).
    fn deco(&mut self, face: Face, size: f32, x: i32, y: i32, text: &str, color: Rgb) -> i32 {
        self.c
            .text(self.f, face, size, x as f32, y as f32, text, color, false)
            .ceil() as i32
    }
    fn width(&self, face: Face, size: f32, text: &str) -> i32 {
        self.c.measure(self.f, face, size, text).ceil() as i32
    }
}

fn clock(at_ms: i64) -> (String, String) {
    let local = rsrewind_core::Timestamp(at_ms).to_local();
    (
        local.format("%-I:%M %p").to_string(),
        local.format("%-m/%-d/%Y").to_string(),
    )
}

/// Desktop wallpaper and a taskbar with a few app tiles and a clock.
fn desktop(x: &mut Ctx, persona: Persona, at_ms: i64, active: usize) {
    let (top, bottom, glow, at) = persona.wallpaper();
    x.c.gradient(top, bottom, glow, at);
    let y = H as i32 - TASKBAR;
    x.c.rect_a(0, y, W as i32, TASKBAR, hex(0x16161c), 0.92);
    x.c.rect_a(0, y, W as i32, 1, hex(0x3a3a46), 0.8);
    let tiles = [
        hex(0x4f7dd9),
        hex(0x2e8b57),
        hex(0xd98c3a),
        hex(0x8a63d2),
        hex(0x3a3a46),
        hex(0x2aa198),
    ];
    let start = W as i32 / 2 - (tiles.len() as i32 * 44) / 2;
    for (i, c) in tiles.iter().enumerate() {
        let tx = start + i as i32 * 44;
        x.c.round(tx + 8, y + 10, 28, 28, 6.0, *c, 0.95);
        if i == active {
            x.c.round(tx + 16, y + 42, 12, 3, 1.5, hex(0x9fb4ff), 1.0);
        }
    }
    let (time, date) = clock(at_ms);
    let tw = x.width(Face::Ui, 13.0, &time);
    x.deco(
        Face::Ui,
        13.0,
        W as i32 - 24 - tw,
        y + 8,
        &time,
        hex(0xe8e6e3),
    );
    let dw = x.width(Face::Ui, 13.0, &date);
    x.deco(
        Face::Ui,
        13.0,
        W as i32 - 24 - dw,
        y + 26,
        &date,
        hex(0xe8e6e3),
    );
}

/// A window frame with a title bar; returns the content rectangle.
fn window(
    x: &mut Ctx,
    rect: (i32, i32, i32, i32),
    title: &str,
    accent: Rgb,
    dark: bool,
) -> (i32, i32, i32, i32) {
    let (wx, wy, ww, wh) = rect;
    x.c.shadow(wx, wy, ww, wh);
    let body = if dark { hex(0x1b1b22) } else { hex(0xfbfaf8) };
    x.c.round(wx, wy, ww, wh, 8.0, body, 1.0);
    let bar = if dark { hex(0x24242d) } else { hex(0xeeece8) };
    x.c.round(wx, wy, ww, 40, 8.0, bar, 1.0);
    x.c.rect(wx, wy + 32, ww, 8, bar);
    x.c.round(wx + 14, wy + 12, 16, 16, 4.0, accent, 1.0);
    let ink = if dark { hex(0xd8d6d2) } else { hex(0x2b2a30) };
    x.t(Face::Ui, 14.0, wx + 40, wy + 10, title, ink);
    // Caption buttons: minimise, maximise, close.
    let bx = wx + ww - 138;
    x.c.rect(bx + 18, wy + 20, 10, 1, ink);
    x.c.outline(bx + 64, wy + 15, 10, 10, 0.0, 1, ink);
    for k in 0..10 {
        x.c.rect(bx + 110 + k, wy + 15 + k, 1, 1, ink);
        x.c.rect(bx + 119 - k, wy + 15 + k, 1, 1, ink);
    }
    (wx, wy + 40, ww, wh - 40)
}

const PRINTERS: [(&str, &str, &str); 4] = [
    ("Kestrel LX 360", "kestrel-3f", "3rd floor, copy room"),
    ("Kestrel LX 520", "kestrel-2f", "2nd floor, east wing"),
    ("Kestrel MF 44", "kestrel-fd", "Front desk"),
    ("Kestrel LX 360", "kestrel-4f", "4th floor, studio"),
];

/// The Waypoint browser showing the Northwind intranet's printer page.
fn browser_printer(x: &mut Ctx) -> (String, &'static str) {
    let (model, queue, place) = *x.r.pick(&PRINTERS);
    let black = 4 + x.r.below(30);
    let cyan = 20 + x.r.below(75);
    let pages = 8_000 + x.r.below(30_000);
    let title = format!("{model} · Device settings — Waypoint");
    let content = browser_frame(
        x,
        &title,
        &format!("intranet.northwind.example/printers/{queue}"),
        &[
            &format!("{model} · Device settings"),
            "Northwind intranet",
            "Supply portal",
        ],
    );
    let (cx, cy, cw, ch) = content;
    // Left navigation.
    x.c.rect(cx, cy, 230, ch - 8, hex(0xf3f1ed));
    for (i, item) in ["Devices", "Print queues", "Supplies", "Reports", "Settings"]
        .iter()
        .enumerate()
    {
        let selected = i == 0;
        if selected {
            x.c.round(
                cx + 12,
                cy + 18 + i as i32 * 44,
                206,
                36,
                6.0,
                hex(0xdfe6fb),
                1.0,
            );
        }
        x.t(
            Face::Ui,
            16.0,
            cx + 28,
            cy + 26 + i as i32 * 44,
            item,
            if selected {
                hex(0x2d4aa8)
            } else {
                hex(0x46444c)
            },
        );
    }
    let px = cx + 270;
    x.t(
        Face::Ui,
        13.0,
        px,
        cy + 26,
        "Northwind Print Co.  ›  Devices",
        hex(0x77737c),
    );
    x.t(
        Face::Bold,
        30.0,
        px,
        cy + 50,
        &format!("{model} — {place}"),
        hex(0x1d1c22),
    );
    let (serial, patch) = (10_000 + x.r.below(89_999), x.r.below(9));
    x.t(
        Face::Ui,
        15.0,
        px,
        cy + 96,
        &format!("Queue {queue}  ·  Serial NW-{serial:05}  ·  Firmware 4.12.{patch}"),
        hex(0x66636b),
    );
    // Status cards.
    let jam = x.r.chance(35);
    let cards: [(&str, String, Rgb); 3] = [
        (
            "Status",
            if jam {
                "Paper jam in tray 2".into()
            } else {
                "Ready".into()
            },
            if jam { hex(0xc2410c) } else { hex(0x15803d) },
        ),
        ("Pages this month", format!("{pages}"), hex(0x1d1c22)),
        (
            "Last service",
            format!("{} days ago", 3 + x.r.below(60)),
            hex(0x1d1c22),
        ),
    ];
    for (i, (label, value, color)) in cards.iter().enumerate() {
        let kx = px + i as i32 * 300;
        x.c.round(kx, cy + 140, 280, 110, 10.0, hex(0xffffff), 1.0);
        x.c.outline(kx, cy + 140, 280, 110, 10.0, 1, hex(0xe3e0da));
        x.t(Face::Ui, 14.0, kx + 20, cy + 160, label, hex(0x77737c));
        x.t(Face::Bold, 24.0, kx + 20, cy + 188, value, *color);
    }
    // Toner levels.
    x.t(Face::Bold, 20.0, px, cy + 290, "Toner", hex(0x1d1c22));
    let toners = [
        ("Toner (black)", black, hex(0x26252b)),
        ("Toner (cyan)", cyan, hex(0x0891b2)),
        ("Toner (magenta)", 15 + x.r.below(80), hex(0xc026d3)),
        ("Toner (yellow)", 15 + x.r.below(80), hex(0xeab308)),
    ];
    for (i, (label, level, color)) in toners.iter().enumerate() {
        let ty = cy + 330 + i as i32 * 56;
        x.t(Face::Ui, 16.0, px, ty, label, hex(0x2b2a30));
        x.c.round(px + 200, ty + 4, 520, 14, 7.0, hex(0xe8e5df), 1.0);
        x.c.round(
            px + 200,
            ty + 4,
            (520 * *level as i32) / 100,
            14,
            7.0,
            *color,
            1.0,
        );
        let note = if *level < 15 {
            format!("{level}%  low — order soon")
        } else {
            format!("{level}%")
        };
        x.t(
            Face::Ui,
            15.0,
            px + 740,
            ty,
            &note,
            if *level < 15 {
                hex(0xc2410c)
            } else {
                hex(0x46444c)
            },
        );
    }
    // Recent events.
    x.t(
        Face::Bold,
        20.0,
        px,
        cy + 570,
        "Recent events",
        hex(0x1d1c22),
    );
    let events = [
        format!(
            "09:{:02}  Tray 2 paper jam cleared by front desk",
            10 + x.r.below(49)
        ),
        format!(
            "08:{:02}  Toner (black) below 20%, supply request SR-{} opened",
            10 + x.r.below(49),
            2000 + x.r.below(999)
        ),
        "Yesterday  Firmware check: up to date".into(),
        format!(
            "Yesterday  {} pages printed, 3 jobs held for release",
            200 + x.r.below(900)
        ),
    ];
    for (i, e) in events.iter().enumerate() {
        x.t(
            Face::Ui,
            15.0,
            px,
            cy + 610 + i as i32 * 32,
            e,
            hex(0x46444c),
        );
    }
    x.c.round(px + cw - 560, cy + 290, 190, 44, 8.0, hex(0x2d4aa8), 1.0);
    x.t(
        Face::Bold,
        16.0,
        px + cw - 535,
        cy + 301,
        "Order toner",
        hex(0xffffff),
    );
    (title, "Waypoint.exe")
}

/// The browser chrome: tab strip, address bar; returns the page rectangle.
fn browser_frame(x: &mut Ctx, title: &str, url: &str, tabs: &[&str]) -> (i32, i32, i32, i32) {
    let (cx, cy, cw, ch) = window(x, (90, 40, 1740, 950), title, hex(0x4f7dd9), false);
    x.c.rect(cx, cy, cw, 44, hex(0xe7e4df));
    for (i, tab) in tabs.iter().enumerate() {
        let tx = cx + 10 + i as i32 * 270;
        if i == 0 {
            x.c.round(tx, cy + 6, 260, 38, 8.0, hex(0xfbfaf8), 1.0);
        }
        x.t(Face::Ui, 13.0, tx + 16, cy + 17, tab, hex(0x3a3940));
    }
    x.c.rect(cx, cy + 44, cw, 46, hex(0xfbfaf8));
    x.c.round(cx + 110, cy + 52, cw - 220, 30, 15.0, hex(0xefede9), 1.0);
    x.deco(Face::Ui, 18.0, cx + 20, cy + 55, "←  →  ⟳", hex(0x77737c));
    x.t(
        Face::Ui,
        14.0,
        cx + 130,
        cy + 58,
        &format!("https://{url}"),
        hex(0x3a3940),
    );
    x.c.rect(cx, cy + 90, cw, 1, hex(0xe3e0da));
    (cx, cy + 91, cw, ch - 91)
}

fn supply_order(x: &mut Ctx) -> (String, &'static str) {
    let number = 2_200 + x.r.below(700);
    let title = format!("Supply order SO-{number} — Waypoint");
    let (cx, cy, cw, _) = browser_frame(
        x,
        &title,
        &format!("supplies.northwind.example/orders/SO-{number}"),
        &[
            &format!("Supply order SO-{number}"),
            "Kestrel LX 360 · Device settings",
            "Northwind intranet",
        ],
    );
    let px = cx + 80;
    x.t(
        Face::Bold,
        30.0,
        px,
        cy + 40,
        &format!("Supply order SO-{number}"),
        hex(0x1d1c22),
    );
    x.t(
        Face::Ui,
        15.0,
        px,
        cy + 86,
        "Vendor: Kestrel Supplies  ·  Ship to: Northwind Print Co., receiving dock",
        hex(0x66636b),
    );
    let header = ["Item", "Part", "Qty", "Unit", "Total"];
    let cols = [0, 520, 760, 880, 1040];
    x.c.rect(px, cy + 140, 1200, 40, hex(0xf3f1ed));
    for (h, c) in header.iter().zip(cols) {
        x.t(Face::Bold, 15.0, px + 16 + c, cy + 150, h, hex(0x46444c));
    }
    let items = [
        ("Toner cartridge, black", "TK-36K", 41.0),
        ("Toner cartridge, cyan", "TK-36C", 58.5),
        ("Toner cartridge, magenta", "TK-36M", 58.5),
        ("Toner cartridge, yellow", "TK-36Y", 58.5),
        ("Waste toner box", "WT-8", 19.0),
        ("Copy paper A4, 5 reams", "PA-5", 24.9),
        ("Staple refill", "SR-3", 12.0),
        ("Drum unit", "DR-36", 129.0),
    ];
    let mut sum = 0.0;
    for row in 0..6 {
        let (name, part, unit) = *x.r.pick(&items);
        let qty = 1 + x.r.below(6);
        let total = unit * qty as f64;
        sum += total;
        let ry = cy + 190 + row * 44;
        if row % 2 == 1 {
            x.c.rect(px, ry - 6, 1200, 44, hex(0xf8f7f4));
        }
        let cells = [
            name.to_owned(),
            part.to_owned(),
            qty.to_string(),
            format!("${unit:.2}"),
            format!("${total:.2}"),
        ];
        for (text, c) in cells.iter().zip(cols) {
            x.t(Face::Ui, 15.0, px + 16 + c, ry + 6, text, hex(0x2b2a30));
        }
    }
    x.t(
        Face::Bold,
        20.0,
        px + 880,
        cy + 470,
        &format!("Total ${sum:.2}"),
        hex(0x1d1c22),
    );
    x.c.round(px + cw - 520, cy + 40, 200, 44, 8.0, hex(0x15803d), 1.0);
    x.t(
        Face::Bold,
        16.0,
        px + cw - 492,
        cy + 51,
        "Submit order",
        hex(0xffffff),
    );
    x.t(
        Face::Ui,
        15.0,
        px,
        cy + 540,
        "Approver: Facilities lead  ·  Budget line: Q3 office supplies",
        hex(0x66636b),
    );
    (title, "Waypoint.exe")
}

fn terminal(x: &mut Ctx) -> (String, &'static str) {
    let title = "Terminal — ops@northwind".to_owned();
    let (cx, cy, _, ch) = window(x, (160, 70, 1500, 880), &title, hex(0x3a3a46), true);
    let mut y = cy + 18;
    let prompt = hex(0x7cc6ee);
    let out = hex(0xd6d3cf);
    let dim = hex(0x8f8b86);
    let ok = hex(0x74d4b5);
    let blocks: Vec<Vec<(String, Rgb)>> = vec![
        vec![
            (
                "PS C:\\ops> Get-PrintQueue -Printer kestrel-3f".into(),
                prompt,
            ),
            ("Name         Status    Jobs   Toner(black)".into(), dim),
            (
                format!(
                    "kestrel-3f   Ready     {}      {}%",
                    x.r.below(5),
                    5 + x.r.below(40)
                ),
                out,
            ),
        ],
        vec![
            (
                "PS C:\\ops> Test-NetConnection print01.northwind.example -Port 9100".into(),
                prompt,
            ),
            ("ComputerName     : print01.northwind.example".into(), out),
            ("RemotePort       : 9100".into(), out),
            ("TcpTestSucceeded : True".into(), ok),
        ],
        vec![
            ("PS C:\\ops> git log --oneline -4".into(), prompt),
            (
                format!(
                    "{:07x} Fix invoice export rounding for partial refunds",
                    x.r.next() & 0xfff_ffff
                ),
                out,
            ),
            (
                format!(
                    "{:07x} Add toner usage report per floor",
                    x.r.next() & 0xfff_ffff
                ),
                out,
            ),
            (
                format!(
                    "{:07x} Kiosk: show order number on receipt",
                    x.r.next() & 0xfff_ffff
                ),
                out,
            ),
            (
                format!(
                    "{:07x} Bump print spooler retry to 3",
                    x.r.next() & 0xfff_ffff
                ),
                out,
            ),
        ],
        vec![
            ("PS C:\\ops> .\\Export-Invoices.ps1 -Month 9".into(), prompt),
            (
                format!(
                    "Exported {} invoices to \\\\files.northwind.example\\finance\\2026-09",
                    40 + x.r.below(80)
                ),
                out,
            ),
            (
                "Warning: invoice 4471 is overdue (due 2026-09-30)".into(),
                hex(0xe8bd72),
            ),
        ],
        vec![
            ("PS C:\\ops> cargo test --workspace".into(), prompt),
            (
                format!(
                    "test result: ok. {} passed; 0 failed; 0 ignored",
                    120 + x.r.below(80)
                ),
                ok,
            ),
        ],
        vec![
            (
                "PS C:\\ops> Get-Content .\\toner-alerts.log -Tail 3".into(),
                prompt,
            ),
            (
                format!(
                    "{:02}:{:02} kestrel-2f toner (black) 9% low",
                    7 + x.r.below(10),
                    x.r.below(60)
                ),
                out,
            ),
            (
                format!(
                    "{:02}:{:02} kestrel-fd tray 2 jam",
                    7 + x.r.below(10),
                    x.r.below(60)
                ),
                out,
            ),
            (
                format!(
                    "{:02}:{:02} kestrel-3f toner order SR-{} placed",
                    7 + x.r.below(10),
                    x.r.below(60),
                    2000 + x.r.below(999)
                ),
                out,
            ),
        ],
    ];
    let mut order: Vec<usize> = (0..blocks.len()).collect();
    for i in (1..order.len()).rev() {
        let j = x.r.below(i as u64 + 1) as usize;
        order.swap(i, j);
    }
    for &b in order.iter().take(4) {
        for (line, color) in &blocks[b] {
            if y > cy + ch - 60 {
                break;
            }
            x.t(Face::Mono, 17.0, cx + 22, y, line, *color);
            y += 27;
        }
        y += 12;
    }
    x.deco(Face::Mono, 17.0, cx + 22, y, "PS C:\\ops> █", prompt);
    (title, "Terminal.exe")
}

fn spreadsheet(x: &mut Ctx) -> (String, &'static str) {
    let sheet = *x.r.pick(&[
        "Q3 supplies.ledger",
        "Invoices 2026.ledger",
        "Toner usage.ledger",
    ]);
    let title = format!("{sheet} — Ledger");
    let (cx, cy, cw, ch) = window(x, (110, 50, 1700, 930), &title, hex(0x2e8b57), false);
    x.c.rect(cx, cy, cw, 52, hex(0x2e8b57));
    for (i, m) in ["File", "Home", "Insert", "Data", "Review"]
        .iter()
        .enumerate()
    {
        x.t(
            Face::Ui,
            15.0,
            cx + 20 + i as i32 * 90,
            cy + 16,
            m,
            hex(0xffffff),
        );
    }
    x.c.rect(cx, cy + 52, cw, 40, hex(0xf6f5f2));
    x.t(Face::Mono, 15.0, cx + 20, cy + 62, "F7", hex(0x46444c));
    x.t(
        Face::Mono,
        15.0,
        cx + 90,
        cy + 62,
        "=SUMIF(F2:F14,\"Overdue\",E2:E14)",
        hex(0x2b2a30),
    );
    let gy = cy + 92;
    let colw = [60, 190, 300, 260, 110, 150, 150, 200];
    let heads = ["", "A", "B", "C", "D", "E", "F", "G"];
    let mut gx = cx;
    for (wdt, h) in colw.iter().zip(heads) {
        x.c.rect(gx, gy, *wdt, 30, hex(0xeceae6));
        x.deco(Face::Ui, 14.0, gx + wdt / 2 - 4, gy + 6, h, hex(0x66636b));
        gx += wdt;
    }
    let vendors = [
        "Kestrel Supplies",
        "Harbor Paper Co.",
        "Brightline Office",
        "Northwind Café",
    ];
    let things = [
        "Toner cartridges",
        "Copy paper",
        "Drum unit",
        "Service visit",
        "Coffee beans",
        "Staples",
    ];
    let status = ["Paid", "Paid", "Due", "Overdue", "Paid"];
    let rows: Vec<[String; 7]> = std::iter::once([
        "Invoice".into(),
        "Vendor".into(),
        "Item".into(),
        "Amount".into(),
        "Due".into(),
        "Status".into(),
        "Notes".into(),
    ])
    .chain((0..18).map(|i| {
        let inv = if i == 3 { 4471 } else { 4400 + x.r.below(140) };
        let st = if i == 3 {
            "Overdue"
        } else {
            *x.r.pick(&status)
        };
        [
            format!("INV-{inv}"),
            (*x.r.pick(&vendors)).to_owned(),
            (*x.r.pick(&things)).to_owned(),
            format!("{:.2}", 20.0 + x.r.below(90_000) as f64 / 100.0),
            format!("2026-{:02}-{:02}", 7 + x.r.below(3), 1 + x.r.below(28)),
            st.to_owned(),
            if st == "Overdue" {
                "chase with accounts".into()
            } else {
                String::new()
            },
        ]
    }))
    .collect();
    for (r, row) in rows.iter().enumerate() {
        let ry = gy + 30 + r as i32 * 30;
        if ry > cy + ch - 40 {
            break;
        }
        let mut gx = cx;
        x.c.rect(gx, ry, colw[0], 30, hex(0xeceae6));
        x.deco(
            Face::Ui,
            13.0,
            gx + 20,
            ry + 7,
            &(r + 1).to_string(),
            hex(0x66636b),
        );
        gx += colw[0];
        for (k, cell) in row.iter().enumerate() {
            let wdt = colw[k + 1];
            x.c.rect(gx, ry + 29, wdt, 1, hex(0xe3e0da));
            x.c.rect(gx + wdt - 1, ry, 1, 30, hex(0xe3e0da));
            let face = if r == 0 { Face::Bold } else { Face::Ui };
            let color = if cell == "Overdue" {
                hex(0xc2410c)
            } else {
                hex(0x2b2a30)
            };
            if r == 4 && k == 5 {
                x.c.outline(gx, ry, wdt, 30, 0.0, 2, hex(0x2e8b57));
            }
            if !cell.is_empty() {
                x.t(face, 14.0, gx + 8, ry + 6, cell, color);
            }
            gx += wdt;
        }
    }
    (title, "Ledger.exe")
}

fn mail(x: &mut Ctx) -> (String, &'static str) {
    let title = "Inbox — frontdesk@northwind.example — Parcel".to_owned();
    let (cx, cy, cw, ch) = window(x, (130, 60, 1660, 910), &title, hex(0x0f6cbd), false);
    x.c.rect(cx, cy, 220, ch, hex(0xf3f1ed));
    for (i, (f, n)) in [
        ("Inbox", "4"),
        ("Drafts", ""),
        ("Sent", ""),
        ("Supplies", "1"),
        ("Archive", ""),
    ]
    .iter()
    .enumerate()
    {
        x.t(
            Face::Ui,
            16.0,
            cx + 24,
            cy + 24 + i as i32 * 40,
            f,
            hex(0x2b2a30),
        );
        if !n.is_empty() {
            x.deco(
                Face::Bold,
                14.0,
                cx + 180,
                cy + 25 + i as i32 * 40,
                n,
                hex(0x0f6cbd),
            );
        }
    }
    let list_x = cx + 220;
    let messages = [
        (
            "Accounts",
            "Invoice 4471 is overdue",
            "Kestrel Supplies flagged invoice 4471 as 30 days overdue.",
        ),
        (
            "Kestrel Supplies",
            "Your toner order has shipped",
            "Order SR-2291: 4 x toner cartridge (black), arriving Thursday.",
        ),
        (
            "Facilities",
            "Fire drill on Thursday at 10:00",
            "Please leave the printers paused during the drill.",
        ),
        (
            "Northwind Café",
            "Lunch menu for Friday",
            "Tomato soup, grilled cheese, and a lemon tart.",
        ),
        (
            "IT help",
            "Tray 2 on the 3rd floor printer",
            "We replaced the pickup roller; jams should stop now.",
        ),
        (
            "Accounts",
            "Re: invoice export for September",
            "The export is in the finance share as usual.",
        ),
    ];
    let selected = x.r.below(messages.len() as u64) as usize;
    for (i, (from, subject, _)) in messages.iter().enumerate() {
        let my = cy + i as i32 * 84;
        if i == selected {
            x.c.rect(list_x, my, 520, 84, hex(0xe3ecfa));
        }
        x.c.rect(list_x, my + 83, 520, 1, hex(0xe3e0da));
        x.t(Face::Bold, 16.0, list_x + 20, my + 14, from, hex(0x1d1c22));
        x.t(Face::Ui, 15.0, list_x + 20, my + 44, subject, hex(0x46444c));
        let when = format!("{}:{:02}", 8 + x.r.below(9), x.r.below(60));
        x.deco(Face::Ui, 13.0, list_x + 450, my + 16, &when, hex(0x77737c));
    }
    let (from, subject, body) = messages[selected];
    let rx = list_x + 540;
    x.t(Face::Bold, 26.0, rx + 20, cy + 26, subject, hex(0x1d1c22));
    x.t(
        Face::Ui,
        15.0,
        rx + 20,
        cy + 70,
        &format!(
            "{from} <{}@northwind.example>",
            from.to_lowercase().replace(' ', ".")
        ),
        hex(0x66636b),
    );
    x.c.rect(rx + 20, cy + 104, cw - 800, 1, hex(0xe3e0da));
    x.t(
        Face::Ui,
        16.0,
        rx + 20,
        cy + 128,
        "Hi front desk,",
        hex(0x2b2a30),
    );
    x.t(Face::Ui, 16.0, rx + 20, cy + 164, body, hex(0x2b2a30));
    x.t(
        Face::Ui,
        16.0,
        rx + 20,
        cy + 200,
        "Let us know if anything looks off.",
        hex(0x2b2a30),
    );
    x.t(Face::Ui, 16.0, rx + 20, cy + 250, "Thanks,", hex(0x2b2a30));
    x.t(Face::Ui, 16.0, rx + 20, cy + 276, from, hex(0x2b2a30));
    (format!("{subject} — Parcel"), "Parcel.exe")
}

fn chat(x: &mut Ctx) -> (String, &'static str) {
    let channel = *x.r.pick(&["#facilities", "#front-desk", "#it-help"]);
    let title = format!("{channel} — Huddle");
    let (cx, cy, cw, ch) = window(x, (220, 90, 1480, 860), &title, hex(0x8a63d2), true);
    x.c.rect(cx, cy, 260, ch, hex(0x17161d));
    x.t(
        Face::Bold,
        17.0,
        cx + 22,
        cy + 22,
        "Northwind Print Co.",
        hex(0xe8e6e3),
    );
    for (i, c) in [
        "#facilities",
        "#front-desk",
        "#it-help",
        "#kitchen",
        "#random",
    ]
    .iter()
    .enumerate()
    {
        if *c == channel {
            x.c.round(
                cx + 12,
                cy + 62 + i as i32 * 38,
                236,
                32,
                6.0,
                hex(0x3b3357),
                1.0,
            );
        }
        x.t(
            Face::Ui,
            15.0,
            cx + 26,
            cy + 68 + i as i32 * 38,
            c,
            hex(0xc8c1b7),
        );
    }
    let lines = [
        ("Front desk", "the printer on 3 is jamming again, tray 2"),
        (
            "IT help",
            "on it. swapping the toner and checking the roller",
        ),
        ("Front desk", "toner light is still blinking on the Kestrel"),
        ("IT help", "new cartridge is in, try a test page now"),
        ("Front desk", "works! thanks"),
        ("Kitchen", "lunch menu for Friday is up on the kiosk"),
        ("Accounts", "reminder: invoice 4471 needs a sign-off today"),
    ];
    let start = x.r.below(3) as usize;
    let colors = [hex(0x7cc6ee), hex(0x74d4b5), hex(0xef98b1), hex(0xe8bd72)];
    for (i, (who, what)) in lines.iter().skip(start).take(5).enumerate() {
        let my = cy + 30 + i as i32 * 92;
        let c = colors[(who.len()) % colors.len()];
        x.c.round(cx + 290, my, 40, 40, 20.0, c, 1.0);
        x.t(Face::Bold, 16.0, cx + 346, my, who, hex(0xe8e6e3));
        let when = format!("{}:{:02}", 8 + x.r.below(9), x.r.below(60));
        let ww = x.width(Face::Bold, 16.0, who);
        x.deco(Face::Ui, 13.0, cx + 356 + ww, my + 3, &when, hex(0x8f8b86));
        x.t(Face::Ui, 16.0, cx + 346, my + 28, what, hex(0xc8c1b7));
    }
    x.c.round(
        cx + 290,
        cy + ch - 76,
        cw - 320,
        50,
        10.0,
        hex(0x26252e),
        1.0,
    );
    x.deco(
        Face::Ui,
        15.0,
        cx + 310,
        cy + ch - 61,
        &format!("Message {channel}"),
        hex(0x8f8b86),
    );
    (title, "Huddle.exe")
}

fn kiosk_menu(x: &mut Ctx) -> (String, &'static str) {
    let (w, h) = (W as i32, H as i32 - TASKBAR);
    x.c.rect(0, 0, w, h, hex(0x1f1712));
    x.c.rect(0, 0, w, 120, hex(0x2a1f18));
    x.t(Face::Bold, 44.0, 70, 32, "Northwind Café", hex(0xf8f3eb));
    let day =
        *x.r.pick(&["Monday", "Tuesday", "Wednesday", "Thursday", "Friday"]);
    let dw = x.width(Face::Ui, 26.0, &format!("Lunch menu · {day}"));
    x.t(
        Face::Ui,
        26.0,
        w - 70 - dw,
        46,
        &format!("Lunch menu · {day}"),
        hex(0xe8bd72),
    );
    let dishes = [
        ("Tomato soup", "Basil, sourdough croutons", 4.5),
        ("Grilled cheese", "Three cheeses on rye", 6.0),
        ("Harvest bowl", "Farro, squash, tahini", 8.5),
        ("Lemon tart", "Shortcrust, crème fraîche", 3.75),
        ("Chili of the day", "Black bean, cornbread", 7.25),
        ("Cold brew", "Oat or whole milk", 3.0),
        ("Dinner menu", "Served from 17:00", 0.0),
    ];
    for i in 0..6 {
        let (name, note, price) = dishes[(i + x.r.below(2) as usize) % dishes.len()];
        let col = (i % 2) as i32;
        let row = (i / 2) as i32;
        let (bx, by) = (70 + col * 900, 180 + row * 250);
        x.c.round(bx, by, 860, 220, 18.0, hex(0x2c221b), 1.0);
        x.c.round(
            bx + 24,
            by + 24,
            172,
            172,
            14.0,
            *x.r.pick(&[hex(0xb8643a), hex(0x7a8f3e), hex(0xd4a24c), hex(0x8b4a3c)]),
            1.0,
        );
        x.t(Face::Bold, 34.0, bx + 230, by + 40, name, hex(0xf8f3eb));
        x.t(Face::Ui, 22.0, bx + 230, by + 96, note, hex(0xc8c1b7));
        if price > 0.0 {
            x.t(
                Face::Bold,
                30.0,
                bx + 230,
                by + 146,
                &format!("{price:.2}"),
                hex(0xe8bd72),
            );
        }
    }
    ("Northwind Café — Kiosk".into(), "Kiosk.exe")
}

fn kiosk_orders(x: &mut Ctx) -> (String, &'static str) {
    let (w, h) = (W as i32, H as i32 - TASKBAR);
    x.c.rect(0, 0, w, h, hex(0x15130f));
    x.t(Face::Bold, 40.0, 70, 40, "Orders", hex(0xf8f3eb));
    x.t(
        Face::Ui,
        22.0,
        260,
        54,
        "Northwind Café · counter",
        hex(0xc8c1b7),
    );
    let states = [
        ("Ready", hex(0x74d4b5)),
        ("Preparing", hex(0xe8bd72)),
        ("Received", hex(0x7cc6ee)),
    ];
    for (col, (state, color)) in states.iter().enumerate() {
        let bx = 70 + col as i32 * 600;
        x.t(Face::Bold, 28.0, bx, 140, state, *color);
        for k in 0..4 {
            let n = 8800 + x.r.below(60);
            let by = 200 + k * 190;
            x.c.round(bx, by, 560, 170, 16.0, hex(0x24201a), 1.0);
            x.c.round(bx, by, 8, 170, 4.0, *color, 1.0);
            x.t(
                Face::Bold,
                34.0,
                bx + 30,
                by + 24,
                &format!("Order {n}"),
                hex(0xf8f3eb),
            );
            let items = *x.r.pick(&[
                "Tomato soup, cold brew",
                "Harvest bowl",
                "Grilled cheese x2",
                "Lemon tart, cold brew",
                "Chili of the day",
            ]);
            x.t(Face::Ui, 22.0, bx + 30, by + 82, items, hex(0xc8c1b7));
            let ago = 1 + x.r.below(15);
            x.t(
                Face::Ui,
                18.0,
                bx + 30,
                by + 122,
                &format!("{ago} min ago"),
                hex(0x8f8b86),
            );
        }
    }
    if x.r.chance(50) {
        x.t(
            Face::Bold,
            30.0,
            70,
            h - 80,
            "Order 8812 ready at the counter",
            hex(0x74d4b5),
        );
    }
    ("Orders — Kiosk".into(), "Kiosk.exe")
}

/// Draws one screen for `persona` at `at_ms`.
pub fn draw(fonts: &Fonts, rng: &mut Rng, persona: Persona, at_ms: i64) -> Scene {
    let mut x = Ctx {
        c: Canvas::new(W, H),
        f: fonts,
        r: rng,
    };
    type Make = fn(&mut Ctx) -> (String, &'static str);
    let menu: &[(Make, u64, usize)] = match persona {
        Persona::Ops => &[
            (terminal, 30, 4),
            (spreadsheet, 20, 1),
            (browser_printer, 20, 0),
            (mail, 15, 3),
            (supply_order, 15, 0),
        ],
        Persona::FrontDesk => &[
            (browser_printer, 25, 0),
            (mail, 25, 3),
            (chat, 20, 2),
            (spreadsheet, 15, 1),
            (supply_order, 15, 0),
        ],
        Persona::Kiosk => &[(kiosk_menu, 50, 5), (kiosk_orders, 35, 5), (chat, 15, 2)],
    };
    let roll = x.r.below(100);
    let mut acc = 0;
    let (make, tile) = menu
        .iter()
        .find(|(_, weight, _)| {
            acc += weight;
            roll < acc
        })
        .map_or((menu[0].0, menu[0].2), |(m, _, t)| (*m, *t));
    desktop(&mut x, persona, at_ms, tile);
    // The desktop's text is not part of the window; only the window's lines are "recognized".
    x.c.blocks.clear();
    let (title, process) = make(&mut x);
    Scene {
        canvas: x.c,
        process,
        title,
    }
}
