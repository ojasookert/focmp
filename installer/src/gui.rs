use crate::{
    Result, art, install,
    manifest::{self, Manifest},
    steam,
};
use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Vec2};
use std::{path::PathBuf, sync::mpsc};
pub fn run() -> Result<()> {
    eframe::run_native(
        "FOCMP Installer",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("FOCMP Installer")
                .with_inner_size([720.0, 680.0])
                .with_min_inner_size([720.0, 600.0])
                .with_icon(icon()),
            ..Default::default()
        },
        Box::new(|cc| {
            style(&cc.egui_ctx);
            Ok(Box::new(App::new(&cc.egui_ctx)))
        }),
    )
    .map_err(|e| e.to_string())
}
enum Event {
    Done(Result<String>),
    Latest(Result<Manifest>),
    Folder(Option<String>),
}
struct App {
    manifest: Manifest,
    game: String,
    steam: Option<steam::Steam>,
    configure: bool,
    installed: String,
    message: String,
    rx: Option<mpsc::Receiver<Event>>,
    confirm_uninstall: bool,
    /// The banner and icon, loaded in the background (art.rs); without them the plain heading stays.
    art_rx: Option<mpsc::Receiver<art::Art>>,
    banner: Option<(egui::TextureHandle, art::Banner)>,
    show_license: bool,
    /// The game version check: the folder last checked, its exe and data.win hashes (None while checking or unreadable).
    checked_for: String,
    checking: bool,
    hashes: Option<(String, String)>,
    check_rx: Option<mpsc::Receiver<(String, Option<(String, String)>)>>,
}
impl App {
    fn new(ctx: &egui::Context) -> Self {
        let steam = steam::Steam::find(None);
        let (tx, art_rx) = mpsc::channel();
        let (art_steam, art_ctx) = (steam.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(art::load(art_steam.as_ref()));
            art_ctx.request_repaint();
        });
        let game = steam
            .as_ref()
            .and_then(|s| s.game())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut app = Self {
            manifest: manifest::parse(manifest::EMBEDDED_MANIFEST).expect("embedded manifest"),
            game,
            steam,
            configure: !cfg!(windows),
            installed: String::new(),
            message: "The bundled version is ready to install offline.".into(),
            rx: None,
            confirm_uninstall: false,
            art_rx: Some(art_rx),
            banner: None,
            show_license: false,
            checked_for: String::new(),
            checking: false,
            hashes: None,
            check_rx: None,
        };
        app.refresh();
        app
    }
    fn refresh(&mut self) {
        self.installed = match install::record(&PathBuf::from(&self.game)) {
            Ok(Some(r)) => r.version,
            Ok(None) => "Not installed by this installer".into(),
            Err(e) => format!("Needs attention: {e}"),
        };
    }
    fn job(&mut self, ctx: &egui::Context, f: impl FnOnce() -> Event + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
    }
}
impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(event) => {
                    self.rx = None;
                    match event {
                        Event::Done(r) => {
                            self.message = r.unwrap_or_else(|e| format!("Needs attention: {e}"))
                        }
                        Event::Latest(r) => {
                            match r {
                                Ok(m) => {
                                    if manifest::version(&m.version)
                                        > manifest::version(&self.manifest.version)
                                    {
                                        self.message = format!(
                                            "Version {} is available. Select Update to install it.",
                                            m.version
                                        );
                                        self.manifest = m;
                                    } else {
                                        self.message="You already have the latest available installer payload.".into()
                                    }
                                }
                                Err(e) => self.message = e,
                            }
                        }
                        Event::Folder(Some(p)) => self.game = p,
                        Event::Folder(None) => {}
                    }
                    self.refresh();
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.rx = None;
                    self.message =
                        "The operation stopped unexpectedly. No further changes will be made."
                            .into();
                }
                Err(_) => {}
            }
        }
        if let Some(rx) = &self.art_rx
            && let Ok(a) = rx.try_recv()
        {
            self.art_rx = None;
            if let Some(icon) = a.icon {
                ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(std::sync::Arc::new(icon))));
            }
            if let Some(b) = a.banner {
                let tex = ctx.load_texture("banner", b.image.clone(), egui::TextureOptions::LINEAR);
                self.banner = Some((tex, b));
            }
        }
        // identify the installed game version whenever the folder changes (hashing data.win takes a moment)
        if self.checked_for != self.game {
            self.checked_for = self.game.clone();
            self.hashes = None;
            self.checking = PathBuf::from(&self.game).join(steam::EXE).is_file();
            if self.checking {
                let (tx, rx) = mpsc::channel();
                let (key, c) = (self.game.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let h = manifest::game_hashes(&PathBuf::from(&key)).ok();
                    let _ = tx.send((key, h));
                    c.request_repaint();
                });
                self.check_rx = Some(rx);
            }
        }
        if let Some(rx) = &self.check_rx
            && let Ok((key, h)) = rx.try_recv()
        {
            self.check_rx = None;
            if key == self.game {
                self.hashes = h;
                self.checking = false;
            }
        }
        egui::Panel::top("banner")
            .frame(egui::Frame::NONE.fill(BG))
            .show(ui, |ui| match &self.banner {
                Some((tex, b)) => banner_ui(ui, tex, b),
                None => banner_fallback(ui),
            });
        egui::Panel::bottom("footer")
            .frame(egui::Frame::NONE.fill(BG).inner_margin(Margin::symmetric(20, 10)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("FOCMP {}", self.manifest.version)).color(DIM));
                    ui.label(RichText::new("·").color(DIM));
                    ui.label(RichText::new("Unofficial community mod").color(DIM));
                    ui.label(RichText::new("·").color(DIM));
                    ui.hyperlink_to("GitHub", manifest::REPO);
                    ui.label(RichText::new("·").color(DIM));
                    ui.hyperlink_to("Mod support", format!("{}/blob/main/README.md", manifest::REPO));
                    ui.label(RichText::new("·").color(DIM));
                    if ui.link("License").clicked() {
                        self.show_license = true;
                    }
                });
            });
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::NONE.fill(BG).inner_margin(Margin::symmetric(20, 14)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.game_card(ui, &ctx);
                    ui.add_space(10.0);
                    self.mod_card(ui, &ctx);
                });
            });
        egui::Window::new("License")
            .open(&mut self.show_license)
            .collapsible(false)
            .default_size([560.0, 420.0])
            .show(&ctx, |ui| {
                ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
                egui::ScrollArea::vertical()
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .show(ui, |ui| {
                    ui.label(
                        "FOCMP is an unofficial, free community mod. Brainburn Studio doesn't support or maintain it. \
                         Flashes of Chaos, its logo and art belong to Brainburn Studio. \
                         The MULTIPLAYER lettering and the mod's code are covered by the license below.",
                    );
                    ui.separator();
                    ui.label(include_str!("../../LICENSE"));
                });
            });
    }
}

impl App {
    /// The selected folder holds a game version this manifest supports (the Install button needs it).
    fn game_supported(&self) -> bool {
        matches!(&self.hashes, Some((exe, data)) if manifest::supports(&self.manifest, exe, data))
    }

    fn game_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.rx.is_some();
        card(ui, "Game", |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.game)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("Flashes of Chaos folder")
                        .desired_width(ui.available_width()),
                )
                .changed()
                .then(|| self.refresh());
                ui.horizontal(|ui| {
                    if ui.button("Change…").clicked() {
                        self.job(ctx, || Event::Folder(browse()));
                    }
                    if ui.button("Detect from Steam").clicked() {
                        self.steam = steam::Steam::find(None);
                        if let Some(p) = self.steam.as_ref().and_then(|s| s.game()) {
                            self.game = p.to_string_lossy().into_owned();
                            self.refresh();
                        } else {
                            self.message = "Steam game installation not found. Choose its folder manually.".into();
                        }
                    }
                });
            });
            ui.add_space(4.0);
            if self.game.is_empty() {
                status_line(ui, RED, "Flashes of Chaos wasn't found. Choose its folder.");
            } else if PathBuf::from(&self.game).join(steam::EXE).is_file() {
                let build = steam::Steam::build_id(&PathBuf::from(&self.game)).map(|b| format!(" (Steam build {b})")).unwrap_or_default();
                match &self.hashes {
                    _ if self.checking => status_line(ui, DIM, "Checking the game version…"),
                    None => status_line(ui, AMBER, "Couldn't read the game files to check their version."),
                    Some((exe, data)) => match manifest::identify(&self.manifest, exe, data) {
                        Some(v) => status_line(ui, CYAN, &format!("Flashes of Chaos {}{build}.", v.name)),
                        None if manifest::supports(&self.manifest, exe, data) => {
                            status_line(ui, CYAN, &format!("Flashes of Chaos found{build}."))
                        }
                        None => status_line(ui, RED, &format!(
                            "Unsupported game version{build}. FOCMP {} is for {}.",
                            self.manifest.version,
                            manifest::supported_names(&self.manifest)
                        )),
                    },
                }
            } else {
                status_line(ui, AMBER, "This folder doesn't look like the game (Flashes_of_Chaos.exe is missing).");
            }
            #[cfg(not(windows))]
            {
                ui.add_space(4.0);
                ui.add_enabled(!busy, egui::Checkbox::new(&mut self.configure, "Set up Proton to load the mod"));
                ui.label(RichText::new("Start the game once first so Proton creates its prefix. Your launch options aren't touched.").small().color(DIM));
            }
        });
    }

    fn mod_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.rx.is_some();
        card(ui, "FOCMP", |ui| {
            let available = self.manifest.version.clone();
            if self.installed.starts_with("Needs attention") {
                status_line(ui, RED, &self.installed);
            } else if self.installed.starts_with("Not installed") {
                status_line(ui, DIM, &format!("Not installed. Available: {available}."));
            } else if manifest::version(&available) > manifest::version(&self.installed) {
                status_line(ui, AMBER, &format!("Version {} is installed. {available} is available.", self.installed));
            } else {
                status_line(ui, CYAN, &format!("Version {} is installed.", self.installed));
            }
            status_line(ui, DIM, "Your focmp.ini settings and all saves are kept. Close the game first.");
            ui.add_space(6.0);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if self.installed.starts_with("Not installed") { "Install" } else { "Update" };
                let primary = egui::Button::new(RichText::new(label).strong().color(Color32::BLACK).size(16.0))
                    .fill(PINK)
                    .min_size(Vec2::new(150.0, 36.0));
                if ui.add_enabled(!busy && self.game_supported(), primary).clicked() {
                    let game = PathBuf::from(&self.game);
                    let m = self.manifest.clone();
                    let steam = self.steam.clone();
                    let configure = self.configure;
                    self.message = "Checking game compatibility and installing…".into();
                    self.job(ctx, move || {
                        Event::Done((|| {
                            let game = game.canonicalize().map_err(|e| e.to_string())?;
                            if configure {
                                steam::prepare(steam.as_ref(), &game)?;
                            }
                            let message = install::install(&game, &m, None)?;
                            if configure {
                                steam::configure(steam.as_ref(), &game)
                                    .map_err(|e| format!("Mod files installed; the Proton setup needs attention: {e}"))?;
                            }
                            Ok(message)
                        })())
                    });
                }
                let big = |t: &str, w: f32| egui::Button::new(RichText::new(t).size(16.0)).min_size(Vec2::new(w, 36.0));
                if ui.add_enabled(!busy, big("Launch game", 130.0)).clicked() {
                    self.message = steam::launch().map(|_| "Opening Steam…".into()).unwrap_or_else(|e| e);
                }
                if ui.add_enabled(!busy, big("Uninstall", 110.0)).clicked() {
                    self.confirm_uninstall = true;
                }
            });
            if self.confirm_uninstall && !busy {
                ui.add_space(8.0);
                ui.label("Remove FOCMP and restore backed-up files and Proton settings? Saves and edited settings are kept.");
                ui.horizontal(|ui| {
                    if ui.button("Remove FOCMP").clicked() {
                        self.confirm_uninstall = false;
                        let game = PathBuf::from(&self.game);
                        self.job(ctx, move || Event::Done(install::uninstall(&game)));
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_uninstall = false;
                    }
                });
            }
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if busy {
                    ui.add(egui::Spinner::new().color(DIM));
                }
                ui.label(RichText::new(&self.message).color(DIM));
                if !busy && ui.link("Check for updates").clicked() {
                    self.message = "Checking public GitHub releases…".into();
                    self.job(ctx, || Event::Latest(manifest::latest()));
                }
            });
        });
    }
}

const BG: Color32 = Color32::from_rgb(14, 11, 20);
const CARD: Color32 = Color32::from_rgb(28, 21, 38);
const TEXT: Color32 = Color32::from_rgb(236, 230, 245);
const DIM: Color32 = Color32::from_rgb(160, 150, 178);
const PINK: Color32 = Color32::from_rgb(255, 45, 205);
const CYAN: Color32 = Color32::from_rgb(0, 230, 230);
const AMBER: Color32 = Color32::from_rgb(255, 196, 84);
const RED: Color32 = Color32::from_rgb(255, 110, 110);

fn card(ui: &mut egui::Ui, title: &str, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(18.0).strong().color(TEXT));
            ui.add_space(6.0);
            contents(ui);
        });
}

fn status_line(ui: &mut egui::Ui, color: Color32, text: &str) {
    ui.horizontal_wrapped(|ui| {
        let size = ui.text_style_height(&egui::TextStyle::Body);
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), size * 0.22, color);
        ui.label(RichText::new(text).color(if color == DIM { DIM } else { TEXT }));
    });
}

/// Without Steam's art: a plain banner with the name.
fn banner_fallback(ui: &mut egui::Ui) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, w * art::H as f32 / art::W as f32), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, Color32::from_rgb(24, 14, 34));
    ui.painter().text(
        rect.left_center() + Vec2::new(28.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "FOCMP · MULTIPLAYER",
        egui::FontId::proportional(34.0),
        PINK,
    );
}

fn style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = CYAN;
    v.selection.bg_fill = PINK.linear_multiply(0.5);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(46, 34, 62);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(64, 46, 86);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(80, 58, 108);
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        for (text_style, size) in [(egui::TextStyle::Body, 15.0), (egui::TextStyle::Button, 15.0), (egui::TextStyle::Heading, 20.0)] {
            if let Some(font) = s.text_styles.get_mut(&text_style) {
                font.size = size;
            }
        }
    });
}

/// The banner across the window's width, with the blood drips on MULTIPLAYER animated (art::drip_pixels).
fn banner_ui(ui: &mut egui::Ui, tex: &egui::TextureHandle, b: &art::Banner) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, w * art::H as f32 / art::W as f32), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    painter.image(tex.id(), rect, uv, Color32::WHITE);
    let k = w / art::W as f32;
    let cell = egui::vec2(b.px * k + 0.5, b.px * k + 0.5);
    let t = ui.input(|i| i.time) as f32;
    art::drip_pixels(&b.drips, b.word_h, t, |x, y, c| {
        let p = egui::pos2(rect.min.x + (b.word_x + x as f32 * b.px) * k, rect.min.y + (b.word_y + y as f32 * b.px) * k);
        painter.rect_filled(egui::Rect::from_min_size(p, cell), 0.0, Color32::from_rgb(c[0], c[1], c[2]));
    });
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
}

fn browse() -> Option<String> {
    #[cfg(windows)] let output=std::process::Command::new("powershell.exe").args(["-NoProfile","-STA","-Command","[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(); Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; if ($d.ShowDialog() -eq 'OK') { [Console]::Write($d.SelectedPath) }"]).output().ok()?;
    #[cfg(not(windows))]
    let output = std::process::Command::new("zenity")
        .args([
            "--file-selection",
            "--directory",
            "--title=Choose Flashes of Chaos installation",
        ])
        .output()
        .or_else(|_| {
            std::process::Command::new("kdialog")
                .args(["--getexistingdirectory", "."])
                .output()
        })
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn icon() -> egui::IconData {
    let mut rgba = Vec::new();
    for y in 0..128 {
        for x in 0..128 {
            let f = (30..48).contains(&x) && (25..105).contains(&y)
                || (30..99).contains(&x) && (25..43).contains(&y)
                || (30..85).contains(&x) && (58..76).contains(&y);
            rgba.extend_from_slice(if f {
                &[190, 100, 255, 255]
            } else {
                &[20, 17, 32, 255]
            });
        }
    }
    egui::IconData {
        rgba,
        width: 128,
        height: 128,
    }
}
