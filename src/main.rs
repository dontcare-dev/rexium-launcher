use eframe::egui;
use egui::{Color32, RichText, Vec2};
use mc_launcher_core::prelude::*;
use mc_launcher_core::{account::Account, auth::microsoft_account};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

const MC_VERSION: &str = "1.21.1";
const PERF_MODS: &[&str] = &[
    "fabric-api", "sodium", "lithium", "ferrite-core", "immediatelyfast",
    "entityculling", "modernfix", "iris", "dynamic-fps", "moreculling", "noisium",
];

#[derive(Clone, Copy, PartialEq)]
enum Page { Home, Mods, ResourcePacks, Shaderpacks, Settings }

impl Page {
    fn label(self) -> &'static str {
        match self {
            Self::Home => "PLAY",
            Self::Mods => "MODS",
            Self::ResourcePacks => "RESOURCE PACKS",
            Self::Shaderpacks => "SHADERS",
            Self::Settings => "SETTINGS",
        }
    }
    fn project_type(self) -> &'static str {
        match self {
            Self::Mods => "mod",
            Self::ResourcePacks => "resourcepack",
            Self::Shaderpacks => "shader",
            _ => "",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Self::Home => "▶",
            Self::Mods => "▦",
            Self::ResourcePacks => "▧",
            Self::Shaderpacks => "◈",
            Self::Settings => "⚙",
        }
    }
}

#[derive(Clone, Deserialize)]
struct Project {
    title: String,
    description: String,
    downloads: u64,
    slug: String,
}

#[derive(Deserialize)]
struct SearchResponse { hits: Vec<Project> }

#[derive(Default, Serialize, Deserialize)]
struct Settings {
    accent_hue: f32,
    background_path: String,
    microsoft_client_id: String,
    ram_gb: u32,
    minecraft_version: String,
}

enum WorkerMessage {
    Status(String),
    Installed(String),
    LoggedIn { name: String, uuid: String, token: String },
    SearchResults(Vec<Project>),
    Error(String),
    Launched,
}

struct RexiumApp {
    page: Page,
    query: String,
    results: Vec<Project>,
    status: String,
    busy: bool,
    settings: Settings,
    account: Option<Account>,
    account_name: Option<String>,
    background_frames: Vec<(egui::TextureHandle, Duration)>,
    background_started: Instant,
    tx: Sender<WorkerMessage>,
    rx: Receiver<WorkerMessage>,
    installed: bool,
}

impl RexiumApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals = egui::Visuals::dark();
        style.visuals.window_fill = Color32::from_rgb(8, 12, 11);
        style.visuals.panel_fill = Color32::from_rgb(10, 15, 13);
        style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(26, 34, 29);
        style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(39, 62, 46);
        style.visuals.widgets.active.bg_fill = Color32::from_rgb(46, 92, 58);
        style.spacing.item_spacing = Vec2::new(10.0, 10.0);
        cc.egui_ctx.set_style(style);
        let settings = Self::load_settings();
        let background_frames = Self::load_background(&cc.egui_ctx, &settings.background_path);
        let (tx, rx) = mpsc::channel();
        let mut app = Self {
            page: Page::Home,
            query: String::new(),
            results: Vec::new(),
            status: "Ready".to_owned(),
            busy: false,
            settings,
            account: None,
            account_name: None,
            background_frames,
            background_started: Instant::now(),
            tx,
            rx,
            installed: false,
        };
        app.installed = app.profile_exists();
        if !app.installed { app.start_install(); }
        app
    }

    fn data_dir() -> PathBuf {
        dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("RexiumLauncher")
    }
    fn instance_dir() -> PathBuf { Self::data_dir().join("instances").join("Tensura Client") }
    fn game_dir() -> PathBuf { Self::instance_dir().join("game") }
    fn minecraft_dir() -> PathBuf { Self::instance_dir().join("minecraft") }
    fn settings_path() -> PathBuf { Self::data_dir().join("settings.json") }

    fn load_settings() -> Settings {
        fs::read(Self::settings_path()).ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| Settings {
                accent_hue: 0.36,
                background_path: String::new(),
                microsoft_client_id: String::new(),
                ram_gb: 4,
                minecraft_version: MC_VERSION.to_owned(),
            })
    }

    fn save_settings(&self) {
        let _ = fs::create_dir_all(Self::data_dir());
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.settings) {
            let _ = fs::write(Self::settings_path(), bytes);
        }
    }

    fn load_background(ctx: &egui::Context, path: &str) -> Vec<(egui::TextureHandle, Duration)> {
        if path.is_empty() { return Vec::new(); }
        let path_buf = PathBuf::from(path);
        let is_gif = path_buf.extension().and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gif"));
        let decoded: Vec<(image::RgbaImage, Duration)> = if is_gif {
            let Ok(file) = fs::File::open(&path_buf) else { return Vec::new(); };
            let Ok(decoder) = image::codecs::gif::GifDecoder::new(BufReader::new(file)) else { return Vec::new(); };
            let Ok(frames) = image::AnimationDecoder::into_frames(decoder).collect_frames() else { return Vec::new(); };
            frames.into_iter().map(|frame| {
                let delay = frame.delay().numer_denom_ms();
                let ms = if delay.1 == 0 { 100 } else { (delay.0 / delay.1).max(30) };
                (frame.into_buffer(), Duration::from_millis(ms as u64))
            }).collect()
        } else {
            let Ok(image) = image::open(&path_buf) else { return Vec::new(); };
            vec![(image.to_rgba8(), Duration::from_secs(1))]
        };
        decoded.into_iter().enumerate().map(|(index, (image, delay))| {
            let size = [image.width() as usize, image.height() as usize];
            let texture = ctx.load_texture(
                format!("rexium-user-background-{index}"),
                egui::ColorImage::from_rgba_unmultiplied(size, &image.into_raw()),
                egui::TextureOptions::LINEAR,
            );
            (texture, delay)
        }).collect()
    }

    fn background_index(&self) -> usize {
        if self.background_frames.len() <= 1 { return 0; }
        let total: u128 = self.background_frames.iter().map(|(_, delay)| delay.as_millis()).sum();
        if total == 0 { return 0; }
        let elapsed = self.background_started.elapsed().as_millis() % total;
        let mut position = 0_u128;
        for (index, (_, delay)) in self.background_frames.iter().enumerate() {
            position += delay.as_millis();
            if elapsed < position { return index; }
        }
        0
    }

    fn accent(&self) -> Color32 {
        Color32::from(egui::ecolor::Hsva::new(self.settings.accent_hue, 0.72, 0.92, 1.0))
    }

    fn profile_exists(&self) -> bool {
        find_fabric_version_id(&Self::minecraft_dir(), &self.settings.minecraft_version).is_some()
    }

    fn status(&mut self, text: impl Into<String>) {
        self.status = text.into();
    }

    fn start_install(&mut self) {
        if self.busy { return; }
        self.busy = true;
        self.status("Installing Tensura Client...");
        let tx = self.tx.clone();
        let version = self.settings.minecraft_version.clone();
        thread::spawn(move || {
            let result = install_tensura(&version, &tx);
            match result {
                Ok(message) => { let _ = tx.send(WorkerMessage::Installed(message)); }
                Err(error) => { let _ = tx.send(WorkerMessage::Error(error)); }
            }
        });
    }

    fn start_login(&mut self) {
        if self.busy { return; }
        let client_id = self.settings.microsoft_client_id.trim().to_owned();
        if client_id.is_empty() {
            self.status("Add your Microsoft app client ID in Settings to enable sign-in.");
            self.page = Page::Settings;
            return;
        }
        self.busy = true;
        self.status("Starting Microsoft sign-in...");
        let tx = self.tx.clone();
        thread::spawn(move || {
            match microsoft_device_login(&client_id, &tx) {
                Ok((name, uuid, token)) => {
                    let _ = tx.send(WorkerMessage::LoggedIn { name, uuid, token });
                }
                Err(error) => { let _ = tx.send(WorkerMessage::Error(error)); }
            }
        });
    }

    fn start_game(&mut self) {
        if self.busy { return; }
        let Some(account) = self.account.clone() else {
            self.status("Sign in with Microsoft before playing.");
            self.start_login();
            return;
        };
        if !self.installed {
            self.start_install();
            return;
        }
        self.busy = true;
        self.status("Starting Minecraft...");
        let tx = self.tx.clone();
        let version_id = find_fabric_version_id(&Self::minecraft_dir(), &self.settings.minecraft_version);
        let Some(version_id) = version_id else {
            self.busy = false;
            self.installed = false;
            self.status("Tensura Client needs to be installed.");
            return;
        };
        let ram = self.settings.ram_gb.clamp(2, 16);
        thread::spawn(move || {
            let result = (|| -> std::result::Result<(), String> {
                let launcher = Launcher::new(RexiumApp::minecraft_dir());
                let version = launcher.load_version(&version_id).map_err(|e| e.to_string())?;
                let java = ensure_java()?;
                let options = LaunchOptions {
                    account,
                    java_executable: Some(java),
                    game_directory: Some(RexiumApp::game_dir()),
                    launcher_name: "Rexium".to_owned(),
                    launcher_version: env!("CARGO_PKG_VERSION").to_owned(),
                    ..Default::default()
                };
                let launch = launcher.build_launch_command_from_version(&version, options).map_err(|e| e.to_string())?;
                let mut command = Command::new(&launch.executable);
                command.arg("-Xms1G").arg(format!("-Xmx{}G", ram))
                    .args(&launch.args).current_dir(&launch.working_dir).envs(launch.env);
                command.spawn().map_err(|e| format!("Minecraft could not start: {e}"))?;
                Ok(())
            })();
            match result {
                Ok(()) => { let _ = tx.send(WorkerMessage::Launched); }
                Err(error) => { let _ = tx.send(WorkerMessage::Error(error)); }
            }
        });
    }

    fn search(&mut self) {
        let kind = self.page.project_type().to_owned();
        if kind.is_empty() || self.busy { return; }
        self.busy = true;
        self.status("Searching Modrinth...");
        let query = self.query.trim().to_owned();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(20))
                .user_agent("RexiumLauncher/0.2")
                .build();
            let result = client.map_err(|e| e.to_string()).and_then(|client| {
                client.get("https://api.modrinth.com/v2/search")
                    .query(&[("query", query.as_str()), ("facets", &format!("[[\"project_type:{}\"]]", kind)), ("limit", "30")])
                    .send().map_err(|e| e.to_string())?
                    .error_for_status().map_err(|e| e.to_string())?
                    .json::<SearchResponse>().map_err(|e| e.to_string())
            });
            match result {
                Ok(response) => { let _ = tx.send(WorkerMessage::SearchResults(response.hits)); }
                Err(error) => { let _ = tx.send(WorkerMessage::Error(error)); }
            }
        });
    }

    fn install_project(&mut self, project: Project) {
        if self.busy { return; }
        let kind = self.page.project_type().to_owned();
        let version = self.settings.minecraft_version.clone();
        self.busy = true;
        self.status(format!("Installing {}...", project.title));
        let tx = self.tx.clone();
        thread::spawn(move || {
            match download_modrinth_project(&project.slug, &kind, &version, &RexiumApp::game_dir()) {
                Ok(path) => { let _ = tx.send(WorkerMessage::Installed(format!("Installed to {}", path.display()))); }
                Err(error) => { let _ = tx.send(WorkerMessage::Error(error)); }
            }
        });
    }

    fn handle_messages(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                WorkerMessage::Status(text) => self.status(text),
                WorkerMessage::Installed(text) => {
                    self.busy = false;
                    self.installed = true;
                    self.status(text);
                }
                WorkerMessage::LoggedIn { name, uuid, token } => {
                    self.busy = false;
                    self.account = Some(Account::Microsoft { username: name.clone(), uuid, access_token: token });
                    self.account_name = Some(name.clone());
                    self.status(format!("Signed in as {}", name));
                }
                WorkerMessage::SearchResults(results) => {
                    self.busy = false;
                    self.results = results;
                    self.status(format!("{} results", self.results.len()));
                }
                WorkerMessage::Error(error) => {
                    self.busy = false;
                    self.status(error);
                }
                WorkerMessage::Launched => {
                    self.busy = false;
                    self.status("Minecraft started.");
                }
            }
        }
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("rexium_sidebar")
            .exact_width(190.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(Color32::from_rgb(8, 13, 11)).inner_margin(14.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(34.0), egui::Sense::hover());
                    let p = ui.painter();
                    p.rect_filled(rect, 9.0, self.accent());
                    p.text(rect.center(), egui::Align2::CENTER_CENTER, "R", egui::FontId::proportional(23.0), Color32::BLACK);
                    ui.label(RichText::new("REXIUM").size(17.0).strong().color(Color32::WHITE));
                });
                ui.add_space(30.0);
                for page in [Page::Home, Page::Mods, Page::ResourcePacks, Page::Shaderpacks, Page::Settings] {
                    let selected = self.page == page;
                    let text = format!("{}   {}", page.icon(), page.label());
                    let button = egui::Button::new(RichText::new(text).size(12.0).strong()
                        .color(if selected { Color32::WHITE } else { Color32::from_gray(160) }))
                        .fill(if selected { Color32::from_rgba_unmultiplied(self.accent().r(), self.accent().g(), self.accent().b(), 42) } else { Color32::TRANSPARENT })
                        .corner_radius(7.0);
                    if ui.add_sized([ui.available_width(), 38.0], button).clicked() {
                        self.page = page;
                        self.results.clear();
                    }
                }
                ui.add_space(22.0);
                ui.separator();
                ui.add_space(12.0);
                ui.label(RichText::new("TENSURA CLIENT").size(10.0).strong().color(self.accent()));
                ui.label(RichText::new(format!("Minecraft {}", self.settings.minecraft_version)).size(11.0).color(Color32::from_gray(150)));
                ui.label(RichText::new("Fabric loader").size(11.0).color(Color32::from_gray(150)));
                ui.add_space(14.0);
                if self.busy {
                    ui.spinner();
                } else {
                    ui.label(RichText::new(&self.status).size(10.0).color(Color32::from_gray(145)));
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    let account = self.account_name.as_deref().unwrap_or("Not signed in");
                    ui.label(RichText::new(account).size(11.0).strong().color(Color32::WHITE));
                });
            });
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("TENSURA CLIENT").size(12.0).strong().color(self.accent()));
                ui.label(RichText::new("Enter your world.").size(31.0).strong().color(Color32::WHITE));
                ui.label(RichText::new("A performance-focused Minecraft profile.").size(13.0).color(Color32::from_gray(170)));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some((bg, _)) = self.background_frames.get(self.background_index()) {
                    ui.add(egui::Image::new((bg.id(), Vec2::new(330.0, 155.0))).corner_radius(8.0));
                } else {
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(330.0, 155.0), egui::Sense::hover());
                    let p = ui.painter();
                    p.rect_filled(rect, 8.0, Color32::from_rgb(22, 36, 28));
                    p.circle_filled(rect.center() + Vec2::new(70.0, -20.0), 58.0, Color32::from_rgb(36, 102, 67));
                    p.circle_filled(rect.center() + Vec2::new(70.0, -20.0), 42.0, Color32::from_rgb(76, 178, 118));
                    p.circle_filled(rect.center() + Vec2::new(56.0, -30.0), 7.0, Color32::WHITE);
                    p.circle_filled(rect.center() + Vec2::new(83.0, -30.0), 7.0, Color32::WHITE);
                    p.circle_filled(rect.center() + Vec2::new(56.0, -30.0), 3.0, Color32::BLACK);
                    p.circle_filled(rect.center() + Vec2::new(83.0, -30.0), 3.0, Color32::BLACK);
                    p.text(rect.left_bottom() + Vec2::new(14.0, -12.0), egui::Align2::LEFT_BOTTOM, "TENSURA", egui::FontId::proportional(12.0), Color32::WHITE);
                }
            });
        });
        ui.add_space(18.0);
        egui::Frame::new().fill(Color32::from_rgb(14, 22, 18))
            .stroke(egui::Stroke::new(1.0_f32, Color32::from_rgb(37, 63, 44)))
            .corner_radius(9.0).inner_margin(18.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Tensura Client").size(21.0).strong().color(Color32::WHITE));
                        ui.label(RichText::new("Fabric  •  Sodium  •  Iris  •  Performance stack").size(11.0).color(Color32::from_gray(155)));
                        ui.add_space(10.0);
                        if self.busy {
                            ui.add(egui::ProgressBar::new(0.45).desired_width(240.0).animate(true));
                        } else {
                            ui.label(RichText::new(if self.installed { "INSTALLED" } else { "NOT INSTALLED" }).size(10.0).strong().color(self.accent()));
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_sized([180.0, 48.0], egui::Button::new(RichText::new("▶  PLAY").size(17.0).strong().color(Color32::BLACK)).fill(self.accent()).corner_radius(6.0)).clicked() {
                            self.start_game();
                        }
                        if !self.installed && !self.busy && ui.button("Install client").clicked() {
                            self.start_install();
                        }
                    });
                });
            });
        ui.add_space(14.0);
        ui.columns(2, |columns| {
            egui::Frame::new().fill(Color32::from_rgb(13, 20, 17)).corner_radius(8.0).inner_margin(14.0).show(&mut columns[0], |ui| {
                ui.label(RichText::new("PERFORMANCE").size(11.0).strong().color(self.accent()));
                ui.add_space(8.0);
                for name in ["Sodium", "Lithium", "FerriteCore", "ImmediatelyFast", "Entity Culling", "ModernFix", "Iris"] {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("✓").color(self.accent()));
                        ui.label(RichText::new(name).size(12.0).color(Color32::from_gray(205)));
                    });
                }
            });
            egui::Frame::new().fill(Color32::from_rgb(13, 20, 17)).corner_radius(8.0).inner_margin(14.0).show(&mut columns[1], |ui| {
                ui.label(RichText::new("ACCOUNT").size(11.0).strong().color(self.accent()));
                ui.add_space(8.0);
                ui.label(self.account_name.as_deref().unwrap_or("Microsoft account not connected"));
                if self.account.is_none() && ui.button("Sign in with Microsoft").clicked() {
                    self.start_login();
                }
                ui.add_space(8.0);
                ui.label(RichText::new("Shaders are managed separately from performance mods.").size(11.0).color(Color32::from_gray(145)));
            });
        });
    }

    fn browser(&mut self, ui: &mut egui::Ui) {
        let title = match self.page { Page::Mods => "Mods", Page::ResourcePacks => "Resource packs", _ => "Shaders" };
        ui.label(RichText::new(title).size(25.0).strong().color(Color32::WHITE));
        ui.label(RichText::new("Modrinth library").size(12.0).color(Color32::from_gray(150)));
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            let width = (ui.available_width() - 110.0).max(120.0);
            let field = ui.add_sized([width, 38.0], egui::TextEdit::singleline(&mut self.query).hint_text("Search..."));
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) { self.search(); }
            if ui.add_sized([100.0, 38.0], egui::Button::new("Search").fill(self.accent())).clicked() { self.search(); }
        });
        ui.add_space(12.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            for project in self.results.clone() {
                egui::Frame::new().fill(Color32::from_rgb(15, 23, 19)).corner_radius(7.0).inner_margin(12.0).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&project.title).size(14.0).strong());
                            ui.label(RichText::new(&project.description).size(11.0).color(Color32::from_gray(150)));
                            ui.label(RichText::new(format!("{} downloads", project.downloads)).size(10.0).color(Color32::from_gray(125)));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Install").clicked() { self.install_project(project.clone()); }
                        });
                    });
                });
                ui.add_space(6.0);
            }
        });
    }

    fn settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label(RichText::new("Settings").size(25.0).strong());
        ui.add_space(14.0);
        egui::Frame::new().fill(Color32::from_rgb(14, 22, 18)).corner_radius(8.0).inner_margin(16.0).show(ui, |ui| {
            ui.label(RichText::new("Tensura Client").size(16.0).strong());
            ui.horizontal(|ui| {
                ui.label("Minecraft version");
                egui::ComboBox::from_id_salt("mc_version").selected_text(&self.settings.minecraft_version).show_ui(ui, |ui| {
                    for version in ["1.21.1", "1.21.4", "1.21.5", "1.21.8"] {
                        ui.selectable_value(&mut self.settings.minecraft_version, version.to_owned(), version);
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.label("Memory");
                ui.add(egui::Slider::new(&mut self.settings.ram_gb, 2..=16).suffix(" GB"));
            });
            ui.add_space(10.0);
            ui.label("Microsoft app client ID");
            ui.text_edit_singleline(&mut self.settings.microsoft_client_id);
            ui.label(RichText::new("Required for Microsoft sign-in. Tokens are kept in memory for this session.").size(10.0).color(Color32::from_gray(145)));
            ui.add_space(10.0);
            if ui.button("Choose background image").clicked() {
                if let Some(path) = rfd::FileDialog::new().add_filter("Images and GIFs", &["png", "jpg", "jpeg", "webp", "gif"]).pick_file() {
                    self.settings.background_path = path.display().to_string();
                    self.background_frames = Self::load_background(ctx, &self.settings.background_path);
                    self.background_started = Instant::now();
                }
            }
            if ui.button("Clear background").clicked() {
                self.settings.background_path.clear();
                self.background_frames.clear();
            }
            ui.label("Accent");
            ui.add(egui::Slider::new(&mut self.settings.accent_hue, 0.0..=1.0).show_value(false));
            if ui.button("Save settings").clicked() {
                self.save_settings();
                self.status("Settings saved.");
            }
        });
        ui.add_space(10.0);
        ui.label(RichText::new(format!("Instance: {}", Self::instance_dir().display())).size(10.0).color(Color32::from_gray(130)));
        ui.label(RichText::new(format!("Mods: {}", Self::game_dir().join("mods").display())).size(10.0).color(Color32::from_gray(130)));
        ui.label(RichText::new(format!("Resource packs: {}", Self::game_dir().join("resourcepacks").display())).size(10.0).color(Color32::from_gray(130)));
        ui.label(RichText::new(format!("Shaders: {}", Self::game_dir().join("shaderpacks").display())).size(10.0).color(Color32::from_gray(130)));
    }
}

impl eframe::App for RexiumApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages();
        if self.busy { ctx.request_repaint_after(Duration::from_millis(120)); }
        if self.background_frames.len() > 1 { ctx.request_repaint_after(self.background_frames[self.background_index()].1); }
        self.sidebar(ctx);
        egui::TopBottomPanel::top("topbar").exact_height(48.0).frame(egui::Frame::new().fill(Color32::from_rgb(9, 14, 12))).show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new("REXIUM LAUNCHER").size(10.0).strong().color(Color32::from_gray(150)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = self.account_name.as_deref().unwrap_or("Sign in");
                    if ui.button(label).clicked() { self.start_login(); }
                });
            });
        });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::from_rgb(10, 15, 13)).inner_margin(22.0)).show(ctx, |ui| {
            match self.page {
                Page::Home => self.home(ui),
                Page::Mods | Page::ResourcePacks | Page::Shaderpacks => self.browser(ui),
                Page::Settings => self.settings(ui, ctx),
            }
        });
    }
}

fn find_fabric_version_id(root: &Path, mc_version: &str) -> Option<String> {
    let entries = fs::read_dir(root.join("versions")).ok()?;
    entries.flatten().find_map(|entry| {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("fabric-loader-") && name.ends_with(&format!("-{}", mc_version)) { Some(name) } else { None }
    })
}

fn find_java() -> Option<PathBuf> {
    if let Ok(path) = which::which("java") { return Some(path); }
    #[cfg(target_os = "windows")]
    {
        let candidates = [
            PathBuf::from(r"C:\Program Files\Eclipse Adoptium\jdk-21\bin\javaw.exe"),
            PathBuf::from(r"C:\Program Files\Java\jdk-21\bin\javaw.exe"),
            RexiumApp::data_dir().join("runtime").join("bin").join("javaw.exe"),
        ];
        return candidates.into_iter().find(|p| p.exists());
    }
    #[allow(unreachable_code)]
    None
}

fn ensure_java() -> std::result::Result<PathBuf, String> {
    if let Some(java) = find_java() { return Ok(java); }
    #[cfg(target_os = "windows")]
    {
        let runtime_dir = RexiumApp::data_dir().join("runtime");
        fs::create_dir_all(&runtime_dir).map_err(|e| e.to_string())?;
        let bytes = reqwest::blocking::get("https://api.adoptium.net/v3/binary/latest/21/ga/windows/x64/jre/hotspot/normal/eclipse")
            .map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?
            .bytes().map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
        archive.extract(&runtime_dir).map_err(|e| e.to_string())?;
        fn locate(dir: &std::path::Path) -> Option<PathBuf> {
            for entry in fs::read_dir(dir).ok()?.flatten() {
                let path = entry.path();
                if path.file_name().is_some_and(|n| n == "javaw.exe") { return Some(path); }
                if path.is_dir() { if let Some(found) = locate(&path) { return Some(found); } }
            }
            None
        }
        return locate(&runtime_dir).ok_or_else(|| "Java runtime installation did not produce javaw.exe.".to_owned());
    }
    #[cfg(not(target_os = "windows"))]
    Err("Install Java 21 or newer and make java available on PATH.".to_owned())
}

fn install_tensura(version: &str, tx: &Sender<WorkerMessage>) -> std::result::Result<String, String> {
    fs::create_dir_all(RexiumApp::game_dir()).map_err(|e| e.to_string())?;
    for folder in ["mods", "resourcepacks", "shaderpacks", "screenshots", "saves", "config"] {
        fs::create_dir_all(RexiumApp::game_dir().join(folder)).map_err(|e| e.to_string())?;
    }
    let _ = tx.send(WorkerMessage::Status(format!("Installing Minecraft {} + Fabric...", version)));
    let launcher = Launcher::new(RexiumApp::minecraft_dir());
    let install = launcher.install(InstallRequest {
        minecraft_version: version.to_owned(),
        loader: Some(LoaderSpec::Fabric { version: LoaderVersion::LatestStable }),
        java: JavaInstallPolicy::Auto,
    }).map_err(|e| e.to_string())?;
    let _ = tx.send(WorkerMessage::Status("Installing performance mods...".to_owned()));
    let mut installed = Vec::new();
    let mut skipped = Vec::new();
    for slug in PERF_MODS {
        match download_modrinth_project(slug, "mod", version, &RexiumApp::game_dir()) {
            Ok(_) => installed.push(*slug),
            Err(_) => skipped.push(*slug),
        }
    }
    let _ = download_modrinth_project("complementary-reimagined", "shader", version, &RexiumApp::game_dir());
    let report = format!("Tensura Client installed: {}. Performance mods: {} installed, {} unavailable for Minecraft {}. Profile: {}",
        install.version_id, installed.len(), skipped.len(), version, RexiumApp::instance_dir().display());
    Ok(report)
}

fn download_modrinth_project(slug: &str, kind: &str, mc_version: &str, game_dir: &PathBuf) -> std::result::Result<PathBuf, String> {
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(90))
        .user_agent("RexiumLauncher/0.2").build().map_err(|e| e.to_string())?;
    let mut request = client.get(format!("https://api.modrinth.com/v2/project/{}/version", slug))
        .query(&[("game_versions", format!("[\"{}\"]", mc_version))]);
    if kind == "mod" { request = request.query(&[("loaders", "[\"fabric\"]")]); }
    let versions: Vec<serde_json::Value> = request.send().map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?
        .json().map_err(|e| e.to_string())?;
    let file = versions.iter().find_map(|v| {
        v.get("files")?.as_array()?.iter()
            .find(|f| f.get("primary").and_then(|x| x.as_bool()) == Some(true))
            .or_else(|| v.get("files")?.as_array()?.first())
    }).ok_or_else(|| format!("No compatible {} release for {}", slug, mc_version))?;
    let url = file.get("url").and_then(|x| x.as_str()).ok_or("Missing file URL")?;
    let filename = file.get("filename").and_then(|x| x.as_str()).ok_or("Missing filename")?
        .replace(['/', '\\'], "_");
    let folder = match kind {
        "shader" => "shaderpacks",
        "resourcepack" => "resourcepacks",
        _ => "mods",
    };
    let destination_dir = game_dir.join(folder);
    fs::create_dir_all(&destination_dir).map_err(|e| e.to_string())?;
    let bytes = client.get(url).send().map_err(|e| e.to_string())?
        .error_for_status().map_err(|e| e.to_string())?.bytes().map_err(|e| e.to_string())?;
    let destination = destination_dir.join(filename);
    fs::write(&destination, &bytes).map_err(|e| e.to_string())?;
    Ok(destination)
}

fn microsoft_device_login(client_id: &str, tx: &Sender<WorkerMessage>) -> std::result::Result<(String, String, String), String> {
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30))
        .user_agent("RexiumLauncher/0.2").build().map_err(|e| e.to_string())?;
    let device: serde_json::Value = client.post("https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode")
        .form(&[("client_id", client_id), ("scope", "XboxLive.signin offline_access")])
        .send().map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?
        .json().map_err(|e| e.to_string())?;
    let device_code = device["device_code"].as_str().ok_or("Microsoft did not return a device code")?.to_owned();
    let user_code = device["user_code"].as_str().ok_or("Microsoft did not return a user code")?.to_owned();
    let verification_uri = device["verification_uri"].as_str().unwrap_or("https://microsoft.com/link");
    let interval = device["interval"].as_u64().unwrap_or(5);
    let expires = device["expires_in"].as_u64().unwrap_or(900);
    let _ = open::that(verification_uri);
    let _ = tx.send(WorkerMessage::Status(format!("Go to {} and enter code {}", verification_uri, user_code)));
    let started = Instant::now();
    let token: serde_json::Value = loop {
        if started.elapsed().as_secs() > expires { return Err("Microsoft sign-in expired. Try again.".to_owned()); }
        thread::sleep(Duration::from_secs(interval));
        let response = client.post("https://login.microsoftonline.com/consumers/oauth2/v2.0/token")
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", device_code.as_str()),
            ]).send().map_err(|e| e.to_string())?;
        let value: serde_json::Value = response.json().map_err(|e| e.to_string())?;
        if value.get("access_token").is_some() { break value; }
        match value["error"].as_str().unwrap_or_default() {
            "authorization_pending" => continue,
            "slow_down" => thread::sleep(Duration::from_secs(5)),
            "authorization_declined" => return Err("Microsoft sign-in was declined.".to_owned()),
            "expired_token" => return Err("Microsoft sign-in expired. Try again.".to_owned()),
            _ => return Err(value["error_description"].as_str().unwrap_or("Microsoft sign-in failed.").to_owned()),
        }
    };
    let ms_token = token["access_token"].as_str().ok_or("Missing Microsoft access token")?;
    let xbl = microsoft_account::authenticate_with_xbl(ms_token).map_err(|e| e.to_string())?;
    let userhash = xbl.display_claims.xui.first().ok_or("Xbox account did not return a user identity")?.uhs.clone();
    let xsts = microsoft_account::authenticate_with_xsts(&xbl.token).map_err(|e| e.to_string())?;
    let mc = microsoft_account::authenticate_with_minecraft(&userhash, &xsts.token).map_err(|e| e.to_string())?;
    let profile = microsoft_account::get_profile(&mc.access_token).map_err(|e| e.to_string())?;
    if profile.id.is_empty() || profile.name.is_empty() {
        return Err("This Microsoft account does not have a Minecraft Java profile.".to_owned());
    }
    Ok((profile.name, profile.id, mc.access_token))
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0])
            .with_min_inner_size([1000.0, 650.0]).with_title("Rexium Launcher"),
        ..Default::default()
    };
    eframe::run_native("Rexium Launcher", options, Box::new(|cc| Ok(Box::new(RexiumApp::new(cc)))))
}
