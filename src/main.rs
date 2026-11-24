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

#[derive(Clone, Serialize, Deserialize)]
struct CustomServer { name: String, address: String, players_online: u32, players_max: u32, online: bool }

#[derive(Default, Serialize, Deserialize)]
struct Settings {
    accent_hue: f32,
    background_path: String,
    microsoft_client_id: String,
    ram_gb: u32,
    minecraft_version: String,
    #[serde(default = "default_offline_username")]
    offline_username: String,
    #[serde(default)]
    servers: Vec<CustomServer>,
}

fn default_offline_username() -> String { "RexiumPlayer".to_owned() }

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
    new_server_name: String,
    new_server_address: String,
    new_server_players: String,
    new_server_capacity: String,
}

impl RexiumApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals = egui::Visuals::dark();
        style.visuals.window_fill = Color32::BLACK;
        style.visuals.panel_fill = Color32::BLACK;
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
            account: Some(Account::offline("RexiumPlayer")),
            account_name: Some("RexiumPlayer (offline)".to_owned()),
            background_frames,
            background_started: Instant::now(),
            tx,
            rx,
            installed: false,
            new_server_name: String::new(),
            new_server_address: String::new(),
            new_server_players: "0".to_owned(),
            new_server_capacity: "20".to_owned(),
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
                offline_username: "RexiumPlayer".to_owned(),
                servers: Vec::new(),
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
                let ms = delay.0.checked_div(delay.1).unwrap_or(100).max(30);
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
        let account = self.account.clone().unwrap_or_else(|| Account::offline(&self.settings.offline_username));
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
            .exact_width(76.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(Color32::from_rgb(7, 13, 15)).inner_margin(9.0))
            .show(ctx, |ui| {
                ui.add_space(8.0);
                let (logo, _) = ui.allocate_exact_size(Vec2::splat(46.0), egui::Sense::hover());
                ui.painter().rect_filled(logo, 12.0, Color32::from_rgb(0, 205, 133));
                ui.painter().text(logo.center(), egui::Align2::CENTER_CENTER, "R", egui::FontId::proportional(29.0), Color32::from_rgb(3, 22, 17));
                ui.add_space(25.0);
                for page in [Page::Home, Page::Mods, Page::ResourcePacks, Page::Shaderpacks, Page::Settings] {
                    let selected = self.page == page;
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(56.0, 58.0), egui::Sense::click());
                    if response.clicked() {
                        self.page = page;
                        self.results.clear();
                    }
                    if selected {
                        ui.painter().rect_filled(rect, 9.0, Color32::from_rgb(0, 105, 73));
                        ui.painter().rect_filled(egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.left()+3.0, rect.bottom())), 2.0, Color32::from_rgb(0, 225, 145));
                    }
                    let color = if selected { Color32::WHITE } else { Color32::from_rgb(145, 160, 160) };
                    ui.painter().text(egui::pos2(rect.center().x, rect.center().y-7.0), egui::Align2::CENTER_CENTER, page.icon(), egui::FontId::proportional(22.0), color);
                    ui.painter().text(egui::pos2(rect.center().x, rect.bottom()-9.0), egui::Align2::CENTER_CENTER, page.label(), egui::FontId::proportional(7.5), color);
                    ui.add_space(5.0);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    if self.busy { ui.spinner(); }
                    else {
                        ui.painter().circle_filled(ui.cursor().center(), 0.0, Color32::TRANSPARENT);
                        ui.label(RichText::new("●").size(10.0).color(Color32::from_rgb(0, 210, 130)));
                    }
                });
            });
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        let accent = Color32::from_rgb(0, 211, 137);
        let available = ui.available_width();
        let rail_width = 258.0_f32.min((available * 0.32).max(220.0));
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new((available - rail_width - 18.0).max(280.0), ui.available_height()),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.add_space(3.0);
                    ui.label(RichText::new("TENSURA CLIENT").size(10.0).strong().color(accent));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Play").size(27.0).strong().color(Color32::WHITE));
                    ui.add_space(12.0);

                    let hero_height = (ui.available_height() * 0.57).clamp(300.0, 470.0);
                    let (hero, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), hero_height), egui::Sense::hover());
                    ui.painter().rect_filled(hero, 10.0, Color32::from_rgb(10, 17, 19));
                    ui.painter().rect_stroke(hero, 10.0, egui::Stroke::new(1.0_f32, Color32::from_rgb(40, 61, 59)), egui::StrokeKind::Inside);
                    if let Some((texture, _)) = self.background_frames.get(self.background_index()) {
                        ui.painter().image(texture.id(), hero, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0,1.0)), Color32::from_white_alpha(220));
                        ui.painter().rect_filled(hero, 10.0, Color32::from_black_alpha(95));
                    } else {
                        let center = hero.center();
                        for radius in [150.0, 105.0, 60.0] {
                            ui.painter().circle_stroke(center, radius, egui::Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(0, 210, 140, 24)));
                        }
                        ui.painter().circle_filled(egui::pos2(hero.right()-55.0, hero.top()+55.0), 2.0, accent);
                    }
                    ui.painter().text(hero.left_top()+egui::vec2(22.0,20.0), egui::Align2::LEFT_TOP, "TENSURA CLIENT", egui::FontId::proportional(11.0), accent);
                    ui.painter().text(egui::pos2(hero.left()+22.0, hero.bottom()-102.0), egui::Align2::LEFT_TOP, "Minecraft", egui::FontId::proportional(31.0), Color32::WHITE);
                    ui.painter().text(egui::pos2(hero.left()+22.0, hero.bottom()-64.0), egui::Align2::LEFT_TOP, format!("{}  ·  Fabric", self.settings.minecraft_version), egui::FontId::proportional(13.0), Color32::from_gray(210));
                    let play_rect = egui::Rect::from_min_size(egui::pos2(hero.left()+22.0, hero.bottom()-49.0), Vec2::new(205.0, 38.0));
                    ui.painter().rect_filled(play_rect, 5.0, accent);
                    ui.painter().text(play_rect.center(), egui::Align2::CENTER_CENTER, if self.busy { "INSTALLING..." } else { "▶   LAUNCH GAME" }, egui::FontId::proportional(14.0), Color32::from_rgb(2,25,17));
                    if ui.interact(play_rect, ui.id().with("hero_play"), egui::Sense::click()).clicked() {
                        self.start_game();
                    }
                    let profile_y = hero.bottom()+10.0;
                    let gap = 9.0;
                    let tile_w = (ui.available_width()-gap*2.0)/3.0;
                    for (idx, label) in ["TENSURA CLIENT", "MODS", "SHADERS"].iter().enumerate() {
                        let rect = egui::Rect::from_min_size(egui::pos2(hero.left()+idx as f32*(tile_w+gap), profile_y), Vec2::new(tile_w, 56.0));
                        ui.painter().rect_filled(rect, 6.0, Color32::from_rgb(17,24,26));
                        ui.painter().rect_stroke(rect, 6.0, egui::Stroke::new(1.0_f32, Color32::from_rgb(43,57,57)), egui::StrokeKind::Inside);
                        ui.painter().text(egui::pos2(rect.left()+12.0,rect.center().y), egui::Align2::LEFT_CENTER, format!("＋  {}",label), egui::FontId::proportional(11.0), Color32::from_gray(225));
                        if ui.interact(rect, ui.id().with(("home_tile",idx)), egui::Sense::click()).clicked() {
                            self.page = match idx { 1 => Page::Mods, 2 => Page::Shaderpacks, _ => Page::Home };
                        }
                    }
                    ui.add_space(68.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("RECENTLY PLAYED").size(11.0).strong().color(Color32::WHITE));
                        ui.label(RichText::new("Tensura Client").size(10.0).color(Color32::from_gray(140)));
                    });
                },
            );
            ui.add_space(18.0);
            ui.allocate_ui_with_layout(Vec2::new(rail_width, ui.available_height()), egui::Layout::top_down(egui::Align::Min), |ui| {
                let banner = |ui: &mut egui::Ui, title: &str, subtitle: &str, icon: &str, color: Color32, url: &str| {
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 78.0), egui::Sense::click());
                    ui.painter().rect_filled(rect, 8.0, color);
                    ui.painter().circle_filled(egui::pos2(rect.left()+30.0,rect.center().y), 17.0, Color32::from_white_alpha(32));
                    ui.painter().text(egui::pos2(rect.left()+30.0,rect.center().y), egui::Align2::CENTER_CENTER, icon, egui::FontId::proportional(18.0), Color32::WHITE);
                    ui.painter().text(egui::pos2(rect.left()+58.0,rect.top()+24.0), egui::Align2::LEFT_CENTER, title, egui::FontId::proportional(14.0), Color32::WHITE);
                    ui.painter().text(egui::pos2(rect.left()+58.0,rect.top()+47.0), egui::Align2::LEFT_CENTER, subtitle, egui::FontId::proportional(10.0), Color32::from_white_alpha(210));
                    ui.painter().text(egui::pos2(rect.right()-14.0,rect.center().y), egui::Align2::RIGHT_CENTER, "OPEN ↗", egui::FontId::proportional(9.0), Color32::WHITE);
                    if response.clicked() { let _ = open::that(url); }
                };
                banner(ui, "Discord", "Join the support server", "◉", Color32::from_rgb(48,68,143), "https://discord.gg/BCWaT9PVhh");
                ui.add_space(9.0);
                banner(ui, "Modrinth", "Mods, packs and shaders", "◆", Color32::from_rgb(0,105,62), "https://modrinth.com");
                ui.add_space(17.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SERVERS").size(10.0).strong().color(Color32::from_gray(175)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("{} added", self.settings.servers.len())).size(9.0).color(Color32::from_gray(125)));
                    });
                });
                ui.add_space(7.0);
                egui::Frame::new().fill(Color32::from_rgb(14,20,22)).corner_radius(7.0).inner_margin(10.0).show(ui, |ui| {
                    ui.label(RichText::new("ADD YOUR SERVER").size(9.0).strong().color(accent));
                    ui.add_space(5.0);
                    ui.add(egui::TextEdit::singleline(&mut self.new_server_name).hint_text("Server name").desired_width(f32::INFINITY));
                    ui.add(egui::TextEdit::singleline(&mut self.new_server_address).hint_text("play.example.net").desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.new_server_players).hint_text("Players").desired_width(62.0));
                        ui.add(egui::TextEdit::singleline(&mut self.new_server_capacity).hint_text("Max").desired_width(62.0));
                        if ui.button("＋ Add").clicked() {
                            let name = self.new_server_name.trim().to_owned();
                            let address = self.new_server_address.trim().to_owned();
                            let online = self.new_server_players.parse::<u32>().unwrap_or(0);
                            let max = self.new_server_capacity.parse::<u32>().unwrap_or(20);
                            if !name.is_empty() && !address.is_empty() && !address.contains(' ') {
                                self.settings.servers.push(CustomServer { name, address, players_online: online.min(max), players_max: max.max(1), online: true });
                                self.new_server_name.clear();
                                self.new_server_address.clear();
                                self.save_settings();
                                self.status("Server added.");
                            }
                        }
                    });
                    ui.label(RichText::new("Add a name, address and current player count.").size(9.0).color(Color32::from_gray(130)));
                });
                ui.add_space(8.0);
                let mut remove_server = None;
                for (index, server) in self.settings.servers.clone().iter().enumerate() {
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 58.0), egui::Sense::click());
                    ui.painter().rect_filled(rect, 7.0, Color32::from_rgb(17,23,26));
                    ui.painter().text(egui::pos2(rect.left()+12.0,rect.top()+20.0), egui::Align2::LEFT_CENTER, &server.name, egui::FontId::proportional(12.0), Color32::WHITE);
                    ui.painter().text(egui::pos2(rect.left()+12.0,rect.top()+41.0), egui::Align2::LEFT_CENTER, format!("{}  ·  {}/{} players",server.address,server.players_online,server.players_max), egui::FontId::proportional(9.0), Color32::from_gray(150));
                    let status_color = if server.online { accent } else { Color32::from_rgb(220,75,75) };
                    ui.painter().circle_filled(egui::pos2(rect.right()-18.0,rect.center().y),4.0,status_color);
                    if response.clicked() {
                        ui.ctx().copy_text(server.address.clone());
                        self.status(format!("Copied {}",server.address));
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(if server.online { "ONLINE" } else { "OFFLINE" }).size(8.0).color(status_color));
                        ui.label(RichText::new("Click card to copy").size(8.0).color(Color32::from_gray(105)));
                        if ui.small_button(if server.online { "Set offline" } else { "Set online" }).clicked() {
                            if let Some(item) = self.settings.servers.get_mut(index) { item.online = !item.online; }
                            self.save_settings();
                        }
                        if ui.small_button("Remove").clicked() { remove_server = Some(index); }
                    });
                    ui.add_space(5.0);
                }
                if let Some(index) = remove_server {
                    self.settings.servers.remove(index);
                    self.save_settings();
                }
                egui::Frame::new().fill(Color32::from_rgb(14,20,22)).corner_radius(7.0).inner_margin(12.0).show(ui, |ui| {
                    ui.label(RichText::new("ACCOUNT").size(10.0).strong().color(accent));
                    ui.add_space(5.0);
                    ui.label(RichText::new(self.account_name.as_deref().unwrap_or("Offline mode")).size(12.0).color(Color32::WHITE));
                    if ui.small_button("Microsoft sign-in  ↗").clicked() { self.start_login(); }
                });
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
            ui.label("Offline player name");
            ui.text_edit_singleline(&mut self.settings.offline_username);
            ui.label(RichText::new("Offline mode works without a Microsoft account. Online servers may require a paid Microsoft-owned Java account.").size(10.0).color(Color32::from_gray(145)));
            ui.add_space(10.0);
            ui.label("Microsoft app client ID (optional)");
            ui.text_edit_singleline(&mut self.settings.microsoft_client_id);
            ui.label(RichText::new("Only needed if you choose Microsoft sign-in.").size(10.0).color(Color32::from_gray(145)));
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
                let username = self.settings.offline_username.trim().to_owned();
                if !username.is_empty() {
                    self.settings.offline_username = username.chars().take(16).collect();
                    if self.account_name.as_deref().is_some_and(|name| name.ends_with("(offline)")) {
                        self.account = Some(Account::offline(&self.settings.offline_username));
                        self.account_name = Some(format!("{} (offline)", self.settings.offline_username));
                    }
                }
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
        egui::TopBottomPanel::top("topbar").exact_height(54.0)
            .frame(egui::Frame::new().fill(Color32::from_rgb(5, 11, 13)).inner_margin(egui::Margin::symmetric(18, 0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("REXIUM").size(16.0).strong().color(Color32::WHITE));
                    ui.label(RichText::new("LAUNCHER").size(9.0).strong().color(Color32::from_rgb(0, 211, 137)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let name = self.account_name.as_deref().unwrap_or("Offline player");
                        egui::ComboBox::from_id_salt("account_menu")
                            .selected_text(RichText::new(name).size(11.0).color(Color32::WHITE))
                            .show_ui(ui, |ui| {
                                if ui.button("Offline mode").clicked() {
                                    self.account = Some(Account::offline(&self.settings.offline_username));
                                    self.account_name = Some(format!("{} (offline)", self.settings.offline_username));
                                }
                                if ui.button("Sign in with Microsoft").clicked() { self.start_login(); }
                                if ui.button("Account settings").clicked() { self.page = Page::Settings; }
                            });
                    });
                });
            });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::from_rgb(8, 12, 14)).inner_margin(22.0)).show(ctx, |ui| {
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

fn java_major_version(path: &Path) -> Option<u32> {
    let output = Command::new(path).arg("-version").output().ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let marker = text.find("version \"").map(|i| i + 9)
        .or_else(|| text.find("openjdk ").map(|i| i + 8))?;
    let digits: String = text[marker..].chars().take_while(|ch| ch.is_ascii_digit() || *ch == '.').collect();
    let first = digits.split('.').next()?.parse::<u32>().ok()?;
    Some(if first == 1 {
        digits.split('.').nth(1)?.parse().ok()?
    } else {
        first
    })
}

fn find_java() -> Option<PathBuf> {
    if let Ok(path) = which::which("java") {
        if java_major_version(&path).is_some_and(|version| version >= 21) {
            return Some(path);
        }
    }
    #[cfg(target_os = "windows")]
    {
        let candidates = [
            RexiumApp::data_dir().join("runtime").join("bin").join("java.exe"),
            PathBuf::from(r"C:\Program Files\Eclipse Adoptium\jdk-21\bin\java.exe"),
            PathBuf::from(r"C:\Program Files\Java\jdk-21\bin\java.exe"),
        ];
        return candidates.into_iter().find(|path| {
            path.exists() && java_major_version(path).is_some_and(|version| version >= 21)
        });
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
                if path.file_name().is_some_and(|name| name == "java.exe") { return Some(path); }
                if path.is_dir() { if let Some(found) = locate(&path) { return Some(found); } }
            }
            None
        }
        return locate(&runtime_dir).ok_or_else(|| "Java 21 runtime installation did not produce java.exe.".to_owned());
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
    let shader = download_modrinth_project("complementary-reimagined", "shader", version, &RexiumApp::game_dir());
    let shader_status = if shader.is_ok() { "Complementary Reimagined installed" } else { "Complementary Reimagined unavailable for this Minecraft version" };
    let report = format!("Tensura Client installed: {}. Performance mods: {} installed, {} unavailable for Minecraft {}. {}. Profile: {}",
        install.version_id, installed.len(), skipped.len(), version, shader_status, RexiumApp::instance_dir().display());
    Ok(report)
}

fn download_modrinth_project(slug: &str, kind: &str, mc_version: &str, game_dir: &Path) -> std::result::Result<PathBuf, String> {
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
