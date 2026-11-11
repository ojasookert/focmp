use crate::{
    Result, art, install,
    manifest::{self, Manifest},
    steam,
};
use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Vec2};
use std::{path::PathBuf, sync::mpsc};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
pub fn run() -> Result<()> {
    const SIZE: [f32; 2] = [720.0, 690.0];
    let viewport = egui::ViewportBuilder::default()
        .with_title("FOCMP Installer")
        .with_inner_size(SIZE)
        .with_min_inner_size([720.0, 600.0])
        .with_window_type(egui::X11WindowType::Dialog)
        .with_icon(icon());
    // Wayland has no standalone dialog-type hint. Equal size constraints let
    // Sway recognize this installer as a floating window without user rules.
    // The minimum must match too: eframe re-applies min_inner_size after
    // with_resizable(false), which otherwise leaves a range that can only shrink.
    #[cfg(target_os = "linux")]
    let viewport = if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) {
        viewport.with_resizable(false).with_min_inner_size(SIZE).with_max_inner_size(SIZE)
    } else {
        viewport
    };
    eframe::run_native(
        "FOCMP Installer",
        eframe::NativeOptions {
            viewport,
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
    /// Mod files changed outside the installer; installing waits for the user to allow overwriting them.
    Changed(Vec<String>),
    Folder(Option<String>),
}
struct Readiness {
    game_present: bool,
    build: Option<u64>,
    running: Result<bool>,
    #[cfg(target_os = "linux")]
    prefix_missing: bool,
}
struct App {
    bundled: Manifest,
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
    // Discovery never occupies the install/folder job slot or selects a payload.
    update_rx: Option<mpsc::Receiver<Result<Manifest>>>,
    latest: Option<Manifest>,
    update_error: Option<String>,
    update_started: std::time::Instant,
    readiness: Option<(String, Readiness)>,
    readiness_rx: Option<mpsc::Receiver<(String, Readiness)>>,
    readiness_checked: Option<std::time::Instant>,
    /// The user chose a version themselves; discovery never changes it afterwards.
    picked: bool,
    editing_path: bool,
    changed: Vec<String>,
}
/// One installable FOCMP version, with why it can't be installed here (if it can't).
struct Choice {
    m: Manifest,
    bundled: bool,
    latest: bool,
    older: bool,
    problem: Option<String>,
}
/// Debug builds only: FOCMP_INSTALLER_LATEST=<manifest.json>|offline stands in for the GitHub
/// check, so every version state can be reviewed without publishing a release.
#[cfg(debug_assertions)]
fn test_latest() -> Option<Result<Manifest>> {
    let v = std::env::var("FOCMP_INSTALLER_LATEST").ok()?;
    std::thread::sleep(std::time::Duration::from_millis(800));
    Some(if v == "offline" {
        Err("offline (test)".into())
    } else {
        std::fs::read(&v).map_err(|e| e.to_string()).and_then(|d| manifest::parse(&d))
    })
}
#[cfg(not(debug_assertions))]
fn test_latest() -> Option<Result<Manifest>> {
    None
}
impl App {
    fn new(ctx: &egui::Context) -> Self {
        let steam = steam::Steam::find(None);
        let (tx, art_rx) = mpsc::channel();
        let art_ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(art::load());
            art_ctx.request_repaint();
        });
        let game = steam
            .as_ref()
            .and_then(|s| s.game())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        #[cfg(debug_assertions)]
        let game = std::env::var("FOCMP_INSTALLER_GAME").unwrap_or(game);
        let bundled = manifest::parse(manifest::EMBEDDED_MANIFEST).expect("embedded manifest");
        let mut app = Self {
            manifest: bundled.clone(),
            bundled,
            game,
            steam,
            configure: !cfg!(windows),
            installed: String::new(),
            message: String::new(),
            rx: None,
            confirm_uninstall: false,
            art_rx: Some(art_rx),
            banner: None,
            show_license: false,
            checked_for: String::new(),
            checking: false,
            hashes: None,
            check_rx: None,
            update_rx: None,
            latest: None,
            update_error: None,
            update_started: std::time::Instant::now(),
            readiness: None,
            readiness_rx: None,
            readiness_checked: None,
            picked: false,
            editing_path: false,
            changed: Vec::new(),
        };
        app.refresh();
        app.check_updates(ctx);
        app
    }
    fn check_updates(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        self.update_rx = Some(rx);
        self.update_error = None;
        self.update_started = std::time::Instant::now();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(test_latest().unwrap_or_else(manifest::latest));
            ctx.request_repaint();
        });
    }
    fn refresh(&mut self) {
        self.readiness_checked = None;
        self.installed = match install::record(&PathBuf::from(&self.game)) {
            Ok(Some(r)) => r.version,
            Ok(None) => "Not installed by this installer".into(),
            Err(e) => format!("Needs attention: {e}"),
        };
    }
    fn readiness(&self) -> Option<&Readiness> {
        self.readiness.as_ref().filter(|(key, _)| key == &self.game).map(|(_, r)| r)
    }
    fn poll_readiness(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.readiness_rx {
            match rx.try_recv() {
                Ok((key, r)) => {
                    self.readiness_rx = None;
                    if key == self.game { self.readiness = Some((key, r)); }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.readiness_rx = None;
                    self.readiness = Some((self.game.clone(), Readiness {
                        game_present: !self.game.is_empty(),
                        build: None,
                        running: Err("Game status check stopped unexpectedly.".into()),
                        #[cfg(target_os = "linux")]
                        prefix_missing: false,
                    }));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        // Process discovery (tasklist on Windows) must not stall rendering or
        // run once per animation frame. Refresh automatically after game exit.
        if self.readiness_rx.is_none() && (self.readiness().is_none()
            || self.readiness_checked.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(1))) {
            self.readiness_checked = Some(std::time::Instant::now());
            let (tx, rx) = mpsc::channel();
            self.readiness_rx = Some(rx);
            let (key, c) = (self.game.clone(), ctx.clone());
            #[cfg(target_os = "linux")]
            let steam = self.steam.clone();
            std::thread::spawn(move || {
                let game = PathBuf::from(&key);
                let game_present = game.join(steam::EXE).is_file();
                let running = if game_present { steam::game_running(&game) } else { Ok(false) };
                let r = Readiness {
                    game_present, running,
                    build: if game_present { steam::Steam::build_id(&game) } else { None },
                    #[cfg(target_os = "linux")]
                    prefix_missing: game_present && steam::prefix_registry(steam.as_ref(), &game).is_none(),
                };
                let _ = tx.send((key, r));
                c.request_repaint();
            });
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
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
        self.poll_readiness(&ctx);
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(event) => {
                    self.rx = None;
                    match event {
                        Event::Done(r) => {
                            self.message = r.unwrap_or_else(|e| format!("Needs attention: {e}"))
                        }
                        Event::Changed(files) => {
                            self.changed = files;
                            self.message.clear();
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
        if let Some(rx) = &self.update_rx
            && self.update_started.elapsed() >= std::time::Duration::from_millis(500) {
            match rx.try_recv() {
                Ok(result) => {
                    self.update_rx = None;
                    match result {
                        Ok(m) => self.latest = Some(m),
                        Err(e) => self.update_error = Some(e),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.update_rx = None;
                    self.update_error = Some("Update check stopped unexpectedly.".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
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
            self.changed.clear();
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
            .frame(egui::Frame::NONE.fill(BG).inner_margin(Margin::symmetric(20, 10)))
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
                         Flashes of Chaos, its logo and art belong to Brainburn Studio and are used with permission. \
                         The MULTIPLAYER lettering and the mod's code are covered by the license below.",
                    );
                    ui.separator();
                    ui.label(include_str!("../../LICENSE"));
                });
            });
    }
}

impl App {
    fn first_run_needed(&self) -> bool {
        #[cfg(target_os = "linux")]
        { self.configure && self.readiness().is_some_and(|r| r.prefix_missing) }
        #[cfg(not(target_os = "linux"))]
        { false }
    }

    fn supports_game(&self, m: &Manifest) -> bool {
        matches!(&self.hashes, Some((exe, data)) if manifest::supports(m, exe, data))
    }

    /// The selected folder holds a game version this manifest supports (the Install button needs it).
    fn game_supported(&self) -> bool {
        self.supports_game(&self.manifest)
    }

    fn problem(&self, m: &Manifest) -> Option<String> {
        if self.hashes.is_some() && !self.supports_game(m) {
            Some(format!("needs game {}", manifest::supported_names(m)))
        } else {
            None
        }
    }

    /// Every version this installer can install, newest first: the online release and the bundled one.
    fn choices(&self) -> Vec<Choice> {
        let mut list = vec![self.bundled.clone()];
        if let Some(l) = &self.latest && l.version != self.bundled.version {
            list.push(l.clone());
        }
        list.sort_by_key(|m| std::cmp::Reverse(manifest::version(&m.version)));
        list.into_iter()
            .map(|m| Choice {
                bundled: m.version == self.bundled.version,
                latest: self.latest.as_ref().is_some_and(|l| l.version == m.version),
                older: manifest::version(&m.version) < manifest::version(&self.installed),
                problem: self.problem(&m),
                m,
            })
            .collect()
    }

    /// The newest version that installs on this game, once the game files are known.
    fn best(&self) -> Option<Manifest> {
        self.hashes.as_ref()?;
        self.choices().into_iter().find(|c| c.problem.is_none()).map(|c| c.m)
    }

    /// Until the user picks a version, keep the newest compatible one selected.
    fn auto_select(&mut self) {
        if self.picked || self.rx.is_some() { return; }
        if let Some(best) = self.best() && best.version != self.manifest.version {
            self.manifest = best;
        }
    }

    fn choice_label(c: &Choice) -> String {
        let mut tags = vec![c.m.version.clone()];
        if c.latest { tags.push("latest".into()); }
        if c.bundled {
            tags.push("bundled, works offline".into());
        } else {
            tags.push(format!("download {:.1} MB", c.m.package.size as f64 / 1_000_000.0));
        }
        if c.older { tags.push("older than installed".into()); }
        if let Some(p) = &c.problem { tags.push(p.clone()); }
        tags.join(" · ")
    }

    /// One plain-language line: where the installation stands.
    fn headline(&self) -> (Color32, String, String) {
        if self.installed.starts_with("Needs attention") {
            return (RED, "Needs attention".into(), self.installed.trim_start_matches("Needs attention: ").into());
        }
        let installed = manifest::version(&self.installed);
        let newest_offered = self.choices().iter().filter_map(|c| manifest::version(&c.m.version)).max();
        if installed.is_some() && installed > newest_offered {
            return (CYAN, format!("Installed {}", self.installed), "newer than this installer".into());
        }
        let title = if installed.is_some() { format!("Installed {}", self.installed) } else { "Not installed".into() };
        if self.checking || self.hashes.is_none() {
            let detail = if self.checking { "checking the game version…" } else { "" };
            return (DIM, title, detail.into());
        }
        match (installed, self.best()) {
            (_, None) => (AMBER, "No compatible version".into(),
                "No FOCMP version here supports this game version yet".into()),
            (None, Some(b)) => (PINK, "Not installed".into(), format!("FOCMP {} is ready to install", b.version)),
            (Some(_), Some(b)) if manifest::version(&b.version) > installed =>
                (PINK, "Update available".into(), format!("{} to {}", self.installed, b.version)),
            (Some(_), _) => (CYAN, "Up to date".into(), format!("FOCMP {}", self.installed)),
        }
    }

    /// The online check, always one row so nothing below moves when it finishes.
    fn check_line(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.set_min_height(22.0);
            if self.update_rx.is_some() {
                ui.add(egui::Spinner::new().size(14.0).color(DIM));
                ui.label(RichText::new("Checking GitHub for updates…").color(DIM));
            } else if self.update_error.is_some() {
                ui.label(RichText::new(format!("Couldn't reach GitHub. The bundled {} still installs offline.", self.bundled.version)).color(DIM))
                    .on_hover_text(self.update_error.clone().unwrap_or_default());
                if ui.link("Retry").clicked() { self.check_updates(ctx); }
            } else if let Some(latest) = self.latest.clone() {
                let blocked = self.hashes.is_some() && !self.supports_game(&latest)
                    && manifest::version(&latest.version) > manifest::version(&self.installed)
                    && self.best().is_none_or(|b| manifest::version(&latest.version) > manifest::version(&b.version));
                if blocked {
                    ui.label(RichText::new(format!(
                        "FOCMP {} is out, but needs game {}. Update Flashes of Chaos in Steam first.",
                        latest.version, manifest::supported_names(&latest))).color(AMBER));
                } else {
                    ui.label(RichText::new(format!("Checked GitHub · {} is the latest release", latest.version)).color(DIM));
                }
            }
        });
    }

    fn version_row(&mut self, ui: &mut egui::Ui) {
        let choices = self.choices();
        let busy = self.rx.is_some();
        ui.horizontal(|ui| {
            ui.set_min_height(30.0);
            ui.label("Version");
            let current = choices.iter().find(|c| c.m.version == self.manifest.version);
            let text = current.map(Self::choice_label).unwrap_or_else(|| self.manifest.version.clone());
            if choices.len() > 1 {
                ui.add_enabled_ui(!busy, |ui| {
                    egui::ComboBox::from_id_salt("version")
                        .selected_text(text)
                        .width(420.0)
                        .show_ui(ui, |ui| {
                            for c in &choices {
                                let selected = c.m.version == self.manifest.version;
                                let r = ui.add_enabled(c.problem.is_none(), egui::Button::selectable(selected, Self::choice_label(c)));
                                if r.clicked() {
                                    self.manifest = c.m.clone();
                                    self.picked = true;
                                    self.message.clear();
                                }
                            }
                        });
                });
            } else {
                ui.label(RichText::new(text).color(DIM));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.hyperlink_to("What's new", format!("{}/releases/tag/v{}", manifest::REPO, self.manifest.version));
            });
        });
    }

    fn game_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.rx.is_some();
        card(ui, "Game", |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                if self.editing_path || self.game.is_empty() {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.game)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("Flashes of Chaos folder")
                            .desired_width(ui.available_width()),
                    );
                    if self.editing_path && !r.has_focus() && !r.lost_focus() { r.request_focus(); }
                    if r.changed() { self.refresh(); }
                    if r.lost_focus() { self.editing_path = false; }
                } else {
                    let font = egui::TextStyle::Monospace.resolve(ui.style());
                    let width = ui.available_width() - 16.0;
                    let shown = elide_middle(ui, &self.game, &font, width);
                    let r = egui::Frame::new()
                        .fill(ui.visuals().extreme_bg_color)
                        .corner_radius(CornerRadius::same(4))
                        .inner_margin(Margin::symmetric(8, 4))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(shown).font(font));
                        })
                        .response
                        .interact(egui::Sense::click())
                        .on_hover_text(format!("{}\nClick to edit", self.game));
                    if r.clicked() { self.editing_path = true; }
                }
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
                status_line(ui, RED, "Flashes of Chaos wasn't found. Choose its folder.", "");
            } else if PathBuf::from(&self.game).join(steam::EXE).is_file() {
                let build = self.readiness().and_then(|r| r.build).map(|b| format!("Steam build {b}")).unwrap_or_default();
                match &self.hashes {
                    _ if self.checking => status_line(ui, DIM, "Checking the game version…", ""),
                    None => status_line(ui, AMBER, "Couldn't read the game files to check their version.", ""),
                    Some((exe, data)) => match manifest::identify(&self.bundled, exe, data)
                        .or_else(|| self.latest.as_ref().and_then(|m| manifest::identify(m, exe, data))) {
                        Some(v) => status_line(ui, CYAN, &format!("Flashes of Chaos {}", v.name), &build),
                        None if manifest::supports(&self.manifest, exe, data) => {
                            status_line(ui, CYAN, "Flashes of Chaos found", &build)
                        }
                        None => status_line(ui, RED, "Unrecognized game version", &build),
                    },
                }
            } else {
                status_line(ui, AMBER, "This folder doesn't look like the game (Flashes_of_Chaos.exe is missing).", "");
            }
            #[cfg(target_os = "linux")]
            {
                ui.add_space(4.0);
                if ui.add_enabled(!busy, egui::Checkbox::new(&mut self.configure, "Set up Proton to load the mod"))
                    .on_hover_text("Your Steam launch options aren't changed.").changed() {
                    self.readiness_checked = None;
                }
            }
        });
    }

    /// Why the game can't be changed right now, if it can't.
    fn install_blocker(&self) -> Option<String> {
        let game = PathBuf::from(&self.game);
        if self.game.is_empty() || !game.join(steam::EXE).is_file() {
            return Some("Choose the game folder first.".into());
        }
        if self.checking { return Some("Checking the game files…".into()); }
        if self.hashes.is_none() { return Some("Couldn't read the game files.".into()); }
        if !self.game_supported() {
            return Some(format!("FOCMP {} doesn't support this game version.", self.manifest.version));
        }
        match self.readiness() {
            None => Some("Checking whether the game is running…".into()),
            Some(r) => match &r.running {
                Ok(true) => Some("Close Flashes of Chaos first.".into()),
                Err(_) => Some("Couldn't check whether the game is running.".into()),
                Ok(false) if self.first_run_needed() =>
                    Some("Start the game once through Steam and quit it, so Proton creates its files.".into()),
                Ok(false) => None,
            },
        }
    }

    fn start_install(&mut self, ctx: &egui::Context, overwrite: bool) {
        let game = PathBuf::from(&self.game);
        let m = self.manifest.clone();
        let steam = self.steam.clone();
        let configure = self.configure;
        self.changed.clear();
        self.message = format!("Installing FOCMP {}…", m.version);
        self.job(ctx, move || {
            let game = match game.canonicalize() {
                Ok(g) => g,
                Err(e) => return Event::Done(Err(e.to_string())),
            };
            if !overwrite {
                match install::changed(&game) {
                    Ok(files) if !files.is_empty() => return Event::Changed(files),
                    Err(e) => return Event::Done(Err(e)),
                    Ok(_) => {}
                }
            }
            Event::Done((|| {
                if configure {
                    steam::prepare(steam.as_ref(), &game)?;
                }
                let message = install::install(&game, &m, None, overwrite)?;
                if configure {
                    steam::configure(steam.as_ref(), &game)
                        .map_err(|e| format!("Mod files installed; the Proton setup needs attention: {e}"))?;
                }
                Ok(message)
            })())
        });
    }

    fn mod_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.auto_select();
        let busy = self.rx.is_some();
        let game_present = self.readiness().is_some_and(|r| r.game_present);
        let running = self.readiness().is_some_and(|r| matches!(r.running, Ok(true)));
        let idle = self.readiness().is_some_and(|r| matches!(r.running, Ok(false)));
        let installed = manifest::version(&self.installed);
        let selected = manifest::version(&self.manifest.version);
        let older = installed.is_some() && selected < installed;
        // An older version becomes the main action only when the user picked it; otherwise it's a quiet option.
        let current = installed.is_some() && selected <= installed && !(older && self.picked);
        let blocker = self.install_blocker();
        let can_install = !busy && blocker.is_none();
        let can_launch = !busy && idle && game_present;
        let can_uninstall = installed.is_some() && idle && !busy;
        if !can_uninstall { self.confirm_uninstall = false; }
        card(ui, "FOCMP", |ui| {
            let (color, title, detail) = self.headline();
            ui.horizontal(|ui| {
                dot(ui, color);
                ui.label(RichText::new(title).strong().size(17.0));
                if !detail.is_empty() {
                    ui.label(RichText::new(format!("· {detail}")).color(DIM));
                }
            });
            self.check_line(ui, ctx);
            ui.add_space(2.0);
            self.version_row(ui);
            ui.add_space(8.0);
            if self.confirm_uninstall && !busy {
                // Same height as the button row it replaces, so the card never grows past the window.
                ui.horizontal(|ui| {
                    ui.set_min_height(36.0);
                    ui.label("Remove FOCMP and restore the original game files?");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new(RichText::new("Remove").strong().color(Color32::BLACK))
                            .fill(SOFT_RED).min_size(Vec2::new(90.0, 30.0))).clicked() {
                            self.confirm_uninstall = false;
                            let game = PathBuf::from(&self.game);
                            self.message = "Removing FOCMP…".into();
                            self.job(ctx, move || Event::Done(install::uninstall(&game)));
                        }
                        if ui.add(egui::Button::new("Cancel").min_size(Vec2::new(80.0, 30.0))).clicked() {
                            self.confirm_uninstall = false;
                        }
                    });
                });
            } else {
                ui.horizontal(|ui| {
                    let pink = |t: String| egui::Button::new(RichText::new(t).strong().color(Color32::BLACK).size(16.0))
                        .fill(PINK).min_size(Vec2::new(180.0, 36.0));
                    let big = |t: &str| egui::Button::new(RichText::new(t).size(16.0)).min_size(Vec2::new(130.0, 36.0));
                    if current {
                        if ui.add_enabled(can_launch, pink("Launch game".into())).clicked() {
                            self.message = steam::launch().map(|_| "Opening Steam…".into()).unwrap_or_else(|e| e);
                        }
                    } else {
                        let action = if installed.is_none() { "Install" } else if older { "Downgrade to" } else { "Update to" };
                        if ui.add_enabled(can_install, pink(format!("{action} {}", self.manifest.version))).clicked() {
                            self.start_install(ctx, false);
                        }
                        if ui.add_enabled(can_launch, big("Launch game")).clicked() {
                            self.message = steam::launch().map(|_| "Opening Steam…".into()).unwrap_or_else(|e| e);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let quiet = |t: &str, c: Color32| egui::Button::new(RichText::new(t).color(c)).frame(false);
                        if installed.is_some() && ui.add_enabled(can_uninstall, quiet("Uninstall", SOFT_RED)).clicked() {
                            self.confirm_uninstall = true;
                        }
                        if current { ui.label(RichText::new("·").color(DIM)); }
                        if current && older
                            && ui.add_enabled(can_install, quiet(&format!("Downgrade to {}", self.manifest.version), DIM))
                                .on_hover_text(format!("Replace FOCMP {} with the older {}.", self.installed, self.manifest.version))
                                .clicked() {
                            self.start_install(ctx, false);
                        }
                        if current && selected == installed
                            && ui.add_enabled(can_install, quiet("Reinstall", DIM))
                                .on_hover_text(format!("Install FOCMP {} again over the current files.", self.manifest.version))
                                .clicked() {
                            self.start_install(ctx, false);
                        }
                    });
                });
            }
            // One status line under the buttons, only when there's something to say: a pending job, files that
            // need permission to overwrite, what blocks the main action, or the last result.
            let reason = if current { running.then(|| "Flashes of Chaos is running.".to_string()) } else { blocker.clone() };
            if self.confirm_uninstall && !busy {
                ui.add_space(4.0);
                ui.label(RichText::new("Proton settings are restored too. Your saves and focmp.ini edits are kept.").color(DIM));
            } else if !self.changed.is_empty() && !busy {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    dot(ui, AMBER);
                    let (files, verb) = if self.changed.len() == 1 {
                        (self.changed[0].clone(), "was")
                    } else {
                        (self.changed.join(", "), "were")
                    };
                    ui.label(format!("{files} {verb} changed outside the installer."));
                    if ui.add_enabled(can_install, egui::Link::new("Overwrite anyway"))
                        .on_hover_text(format!("Replace it with FOCMP {}'s files. Your saves and focmp.ini are kept.", self.manifest.version))
                        .clicked() {
                        self.start_install(ctx, true);
                    }
                });
            } else if busy || (reason.is_none() && !self.message.is_empty()) {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if busy {
                        ui.add(egui::Spinner::new().color(DIM));
                    }
                    ui.label(RichText::new(&self.message).color(DIM));
                });
            } else if let Some(r) = reason {
                ui.add_space(4.0);
                let pending = r.starts_with("Checking");
                ui.label(RichText::new(r).color(if pending { DIM } else { AMBER }));
            }
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
const SOFT_RED: Color32 = Color32::from_rgb(235, 130, 140);

/// Shortens a path in the middle so its start and the game folder name both stay visible.
fn elide_middle(ui: &egui::Ui, text: &str, font: &egui::FontId, width: f32) -> String {
    let fits = |s: &str| ui.painter().layout_no_wrap(s.to_owned(), font.clone(), TEXT).size().x <= width;
    if fits(text) { return text.to_owned(); }
    let chars: Vec<char> = text.chars().collect();
    for keep in (8..chars.len()).rev() {
        let head = keep / 3;
        let tail = keep - head;
        let s: String = chars[..head].iter().chain(['…'].iter()).chain(chars[chars.len() - tail..].iter()).collect();
        if fits(&s) { return s; }
    }
    text.to_owned()
}

fn dot(ui: &mut egui::Ui, color: Color32) {
    let size = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), size * 0.25, color);
}

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

fn status_line(ui: &mut egui::Ui, color: Color32, text: &str, detail: &str) {
    ui.horizontal_wrapped(|ui| {
        dot(ui, color);
        ui.label(RichText::new(text).color(if color == DIM { DIM } else { TEXT }));
        if !detail.is_empty() {
            ui.label(RichText::new(format!("· {detail}")).color(DIM));
        }
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
        for (text_style, size) in [(egui::TextStyle::Body, 15.0), (egui::TextStyle::Button, 15.0), (egui::TextStyle::Monospace, 15.0), (egui::TextStyle::Small, 15.0), (egui::TextStyle::Heading, 20.0)] {
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
    #[cfg(windows)] let output=std::process::Command::new("powershell.exe").creation_flags(0x08000000).args(["-NoProfile","-STA","-Command","[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(); Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; if ($d.ShowDialog() -eq 'OK') { [Console]::Write($d.SelectedPath) }"]).output().ok()?;
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
