use eframe::egui;
use egui::{Color32, RichText, Vec2};
use mc_launcher_core::prelude::*;
use mc_launcher_core::{account::Account, auth::microsoft_account};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
            Self::Home => "HOME",
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
}

#[derive(Clone, Deserialize)]
struct Project {
    title: String,
    description: String,
    downloads: u64,
    slug: String,
    #[serde(default)]
    icon_url: String,
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
    #[serde(default = "default_loader")]
    loader: String,
    #[serde(default = "default_instance_name")]
    instance_name: String,
    #[serde(default = "default_offline_username")]
    offline_username: String,
    #[serde(default)]
    servers: Vec<CustomServer>,
}

fn default_offline_username() -> String { "TensuraPlayer".to_owned() }
fn default_loader() -> String { "fabric".to_owned() }
fn default_instance_name() -> String { "Tensura Client".to_owned() }

enum WorkerMessage {
    Status(String),
    Installed(String),
    LoggedIn { name: String, uuid: String, token: String },
    SearchResults(Vec<Project>),
    Error(String),
    Launched,
    GameExited(Option<i32>),
    ServerStatus { address: String, online: bool, players_online: u32, players_max: u32 },
    UpdateAvailable { version: String, url: String },
    Versions(Vec<String>),
}

struct TensuraApp {
    page: Page,
    query: String,
    results: Vec<Project>,
    show_installed: bool,
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
    game_running: bool,
    new_server_name: String,
    new_server_address: String,
    new_instance_name: String,
    update_available: Option<(String, String)>,
    available_versions: Vec<String>,
}

impl TensuraApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
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
        let initial_offline_name = settings.offline_username.clone();
        let background_frames = Self::load_background(&cc.egui_ctx, &settings.background_path);
        let (tx, rx) = mpsc::channel();
        let mut app = Self {
            page: Page::Home,
            query: String::new(),
            results: Vec::new(),
            show_installed: false,
            status: "Ready".to_owned(),
            busy: false,
            settings,
            account: Some(Account::offline(&initial_offline_name)),
            account_name: Some(format!("{initial_offline_name} (offline)")),
            background_frames,
            background_started: Instant::now(),
            tx,
            rx,
            installed: false,
            game_running: false,
            new_server_name: String::new(),
            new_server_address: String::new(),
            new_instance_name: String::new(),
            update_available: None,
            available_versions: vec!["1.21.8".to_owned(), "1.21.5".to_owned(), "1.21.4".to_owned(), "1.21.1".to_owned()],
        };
        app.installed = app.profile_exists();
        if !app.installed { app.start_install(); }
        for server in app.settings.servers.clone() { request_server_status(app.tx.clone(), server.address); }
        let update_tx = app.tx.clone();
        thread::spawn(move || check_for_updates(update_tx));
        let versions_tx = app.tx.clone();
        thread::spawn(move || fetch_release_versions(versions_tx));
        app
    }

    fn data_dir() -> PathBuf {
        dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("TensuraLauncher")
    }
    fn instance_dir_for(name: &str) -> PathBuf {
        Self::data_dir().join("instances").join(sanitize_instance_name(name))
    }
    fn current_instance_dir(&self) -> PathBuf { Self::instance_dir_for(&self.settings.instance_name) }
    fn current_game_dir(&self) -> PathBuf { self.current_instance_dir().join("game") }
    fn current_minecraft_dir(&self) -> PathBuf { self.current_instance_dir().join("minecraft") }
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
                loader: default_loader(),
                instance_name: default_instance_name(),
                offline_username: "TensuraPlayer".to_owned(),
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
                format!("tensura-user-background-{index}"),
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
        find_profile_version_id(&self.current_minecraft_dir(), &self.settings.minecraft_version, &self.settings.loader).is_some()
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
        let loader = self.settings.loader.clone();
        let instance_dir = self.current_instance_dir();
        thread::spawn(move || {
            let result = install_tensura(&version, &loader, &instance_dir, &tx);
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
        if self.busy || self.game_running { return; }
        let account = self.account.clone().unwrap_or_else(|| Account::offline(&self.settings.offline_username));
        if !self.installed {
            self.start_install();
            return;
        }
        self.busy = true;
        self.status("Starting Minecraft...");
        let tx = self.tx.clone();
        let version_id = find_profile_version_id(&self.current_minecraft_dir(), &self.settings.minecraft_version, &self.settings.loader);
        let Some(version_id) = version_id else {
            self.busy = false;
            self.installed = false;
            self.status("Tensura Client needs to be installed.");
            return;
        };
        let ram = self.settings.ram_gb.clamp(2, 16);
        let minecraft_dir = self.current_minecraft_dir();
        let game_dir = self.current_game_dir();
        thread::spawn(move || {
            let result = (|| -> std::result::Result<(), String> {
                let launcher = Launcher::new(minecraft_dir);
                let version = launcher.load_version(&version_id).map_err(|e| e.to_string())?;
                let java = ensure_java()?;
                let options = LaunchOptions {
                    account,
                    java_executable: Some(java),
                    game_directory: Some(game_dir.clone()),
                    launcher_name: "Tensura".to_owned(),
                    launcher_version: env!("CARGO_PKG_VERSION").to_owned(),
                    ..Default::default()
                };
                let launch = launcher.build_launch_command_from_version(&version, options).map_err(|e| e.to_string())?;
                let mut command = Command::new(&launch.executable);
                command.arg("-Xms1G").arg(format!("-Xmx{}G", ram))
                    .args(&launch.args).current_dir(&launch.working_dir).envs(launch.env);
                let log_dir = game_dir.join("logs");
                fs::create_dir_all(&log_dir).map_err(|e| e.to_string())?;
                let log = fs::File::create(log_dir.join("tensura-launcher.log")).map_err(|e| e.to_string())?;
                command.stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
                    .stderr(Stdio::from(log));
                let mut child = command.spawn().map_err(|e| format!("Minecraft could not start: {e}"))?;
                let _ = tx.send(WorkerMessage::Launched);
                let exit = child.wait().map_err(|e| format!("Could not monitor Minecraft process: {e}"))?;
                let _ = tx.send(WorkerMessage::GameExited(exit.code()));
                Ok(())
            })();
            match result {
                Ok(()) => {}
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
        let version = self.settings.minecraft_version.clone();
        let loader = self.settings.loader.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut facets = vec![
                vec![format!("project_type:{kind}")],
                vec![format!("versions:{version}")],
            ];
            if kind == "mod" { facets.push(vec![format!("categories:{loader}")]); }
            let facets = serde_json::to_string(&facets).unwrap_or_default();
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(20))
                .user_agent("TensuraLauncher/0.2")
                .build();
            let result = client.map_err(|e| e.to_string()).and_then(|client| {
                client.get("https://api.modrinth.com/v2/search")
                    .query(&[("query", query.as_str()), ("facets", facets.as_str()), ("limit", "30")])
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
        let loader = self.settings.loader.clone();
        let game_dir = self.current_game_dir();
        self.busy = true;
        self.status(format!("Installing {}...", project.title));
        let tx = self.tx.clone();
        thread::spawn(move || {
            match download_modrinth_project(&project.slug, &kind, &version, &loader, &game_dir) {
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
                    self.game_running = true;
                    self.status("Minecraft is running. Launch log: game/logs/tensura-launcher.log");
                }
                WorkerMessage::GameExited(code) => {
                    self.game_running = false;
                    match code {
                        Some(0) => self.status("Minecraft closed normally."),
                        Some(code) => self.status(format!("Minecraft exited with code {code}. Check the launch log.")),
                        None => self.status("Minecraft closed. Check the launch log for details."),
                    }
                }
                WorkerMessage::UpdateAvailable { version, url } => {
                    self.update_available = Some((version.clone(), url));
                    self.status(format!("Tensura Launcher update {version} is available."));
                }
                WorkerMessage::Versions(versions) => {
                    if !versions.is_empty() { self.available_versions = versions; }
                }
                WorkerMessage::ServerStatus { address, online, players_online, players_max } => {
                    if let Some(server) = self.settings.servers.iter_mut().find(|server| server.address == address) {
                        server.online = online;
                        server.players_online = players_online;
                        server.players_max = players_max.max(1);
                        self.save_settings();
                    }
                }
            }
        }
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("tensura_sidebar")
            .exact_width(76.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(Color32::from_rgb(7, 12, 22)).inner_margin(egui::Margin::symmetric(12, 16)))
            .show(ctx, |ui| {
                let mark = egui::Image::from_bytes("bytes://tensura-mark.svg", include_bytes!("../assets/tensura-mark.svg"));
                ui.add_sized(Vec2::splat(44.0), mark);
                ui.add_space(30.0);
                for page in [Page::Home, Page::Mods, Page::ResourcePacks, Page::Shaderpacks, Page::Settings] {
                    let selected = self.page == page;
                    let icon_bytes: &'static [u8] = match page {
                        Page::Home => include_bytes!("../assets/nav-home.svg"),
                        Page::Mods => include_bytes!("../assets/nav-mods.svg"),
                        Page::ResourcePacks => include_bytes!("../assets/nav-packs.svg"),
                        Page::Shaderpacks => include_bytes!("../assets/nav-shaders.svg"),
                        Page::Settings => include_bytes!("../assets/nav-settings.svg"),
                    };
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(52.0, 64.0), egui::Sense::click());
                    if response.clicked() {
                        self.page = page;
                        self.results.clear();
                    }
                    if selected {
                        ui.painter().line_segment(
                            [rect.left_top() + egui::vec2(1.0, 9.0), rect.left_bottom() - egui::vec2(-1.0, 9.0)],
                            egui::Stroke::new(3.0_f32, Color32::from_rgb(80, 239, 204)),
                        );
                    }
                    let icon_rect = egui::Rect::from_center_size(egui::pos2(rect.center().x, rect.center().y - 7.0), Vec2::splat(30.0));
                    ui.put(icon_rect, egui::Image::from_bytes(
                        format!("bytes://nav-{}.svg", page.label()),
                        icon_bytes,
                    ).fit_to_exact_size(icon_rect.size()));
                    ui.painter().text(
                        egui::pos2(rect.center().x, rect.bottom() - 8.0),
                        egui::Align2::CENTER_CENTER,
                        page.label(),
                        egui::FontId::proportional(7.5),
                        if selected { Color32::WHITE } else { Color32::from_rgb(139, 154, 174) },
                    );
                    ui.add_space(8.0);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    if self.busy { ui.spinner(); }
                    else { ui.label(RichText::new("●").size(10.0).color(Color32::from_rgb(78, 235, 174))); }
                });
            });
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        let accent = Color32::from_rgb(47, 224, 177);
        let muted = Color32::from_rgb(145, 163, 180);
        let panel = Color32::from_rgb(13, 22, 35);
        let border = Color32::from_rgb(31, 48, 68);
        let width = ui.available_width();
        let rail = (width * 0.29).clamp(250.0, 330.0);
        let gap = 22.0;
        let main = (width - rail - gap).max(360.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(Vec2::new(main, ui.available_height()), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("WELCOME BACK").size(10.0).strong().color(accent));
                        ui.label(RichText::new("Your Minecraft").size(27.0).strong().color(Color32::WHITE));
                        ui.label(RichText::new("Everything you need to play, in one place.").size(12.0).color(muted));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let status = if self.game_running { "GAME RUNNING" } else if self.busy { "SETTING UP" } else if self.installed { "READY TO PLAY" } else { "NOT INSTALLED" };
                        let c = if self.game_running || self.installed { accent } else { Color32::from_rgb(238, 185, 92) };
                        egui::Frame::new().fill(Color32::from_rgb(19, 39, 48)).corner_radius(20.0).inner_margin(egui::Margin::symmetric(12, 7)).show(ui, |ui| {
                            ui.label(RichText::new(format!("●  {status}")).size(9.0).strong().color(c));
                        });
                    });
                });
                ui.add_space(18.0);
                let hero_height = (ui.available_height() * 0.56).clamp(275.0, 390.0);
                let (hero, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), hero_height), egui::Sense::hover());
                egui::Frame::new().fill(Color32::from_rgb(17, 35, 71)).stroke(egui::Stroke::new(1.0, border)).corner_radius(18.0).show(ui, |ui| {
                    ui.set_min_size(hero.size());
                    let rect = ui.min_rect();
                    if !self.background_frames.is_empty() {
                        let texture = &self.background_frames[self.background_index()].0;
                        ui.painter().image(texture.id(), rect, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0,1.0)), Color32::WHITE);
                    } else {
                        ui.put(rect, egui::Image::from_bytes("bytes://tensura-hero-art.svg", include_bytes!("../assets/tensura-hero-art.svg")).fit_to_exact_size(rect.size()));
                    }
                    let painter = ui.painter().with_clip_rect(rect);
                    painter.rect_filled(rect, 18.0, Color32::from_black_alpha(74));
                    painter.text(rect.left_top()+egui::vec2(28.0,25.0), egui::Align2::LEFT_TOP, "TENSURA CLIENT", egui::FontId::proportional(13.0), Color32::WHITE);
                    painter.text(rect.left_top()+egui::vec2(28.0,48.0), egui::Align2::LEFT_TOP, format!("{}  •  {} PROFILE", self.settings.loader.to_uppercase(), "PERFORMANCE"), egui::FontId::proportional(9.0), Color32::from_rgb(173,224,219));
                    painter.text(rect.left_top()+egui::vec2(28.0,rect.height()-105.0), egui::Align2::LEFT_TOP, "Minecraft", egui::FontId::proportional(39.0), Color32::WHITE);
                    painter.text(rect.left_top()+egui::vec2(31.0,rect.height()-58.0), egui::Align2::LEFT_TOP, format!("{}  /  {}", self.settings.minecraft_version, self.settings.loader.to_uppercase()), egui::FontId::proportional(12.0), Color32::from_rgb(219,235,243));
                    let play = egui::Rect::from_min_size(rect.left_bottom()-egui::vec2(-28.0,64.0), Vec2::new(196.0,42.0));
                    let label = if self.game_running { "MINECRAFT IS RUNNING" } else if self.busy { "PREPARING CLIENT..." } else if self.installed { "▶    PLAY NOW" } else { "INSTALL CLIENT" };
                    painter.rect_filled(play, 8.0, if self.busy { Color32::from_rgb(87,115,126) } else { accent });
                    painter.text(play.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(12.0).strong(), Color32::from_rgb(5,24,28));
                    if ui.interact(play, ui.id().with("tensura_play"), egui::Sense::click()).clicked() && !self.busy && !self.game_running {
                        if self.installed { self.start_game(); } else { self.start_install(); }
                    }
                });
                ui.add_space(16.0);
                ui.label(RichText::new("QUICK ACCESS").size(10.0).strong().color(muted));
                ui.add_space(8.0);
                let tile_gap=10.0;
                let tile_width=(ui.available_width()-tile_gap*3.0)/4.0;
                ui.horizontal(|ui| {
                    for (name, sub, page, glyph) in [
                        ("MODS","Enhance gameplay",Page::Mods,"✧"),
                        ("RESOURCE PACKS","Change visuals",Page::ResourcePacks,"▧"),
                        ("SHADERS","Lighting & effects",Page::Shaderpacks,"✦"),
                        ("SETTINGS","Tune your client",Page::Settings,"⚙"),
                    ] {
                        egui::Frame::new().fill(panel).stroke(egui::Stroke::new(1.0,border)).corner_radius(11.0).inner_margin(10.0).show(ui,|ui|{
                            ui.set_min_width(tile_width-20.0);
                            if ui.add(egui::Button::new(RichText::new(glyph).size(20.0).color(accent)).frame(false)).clicked(){self.page=page;self.results.clear();}
                            ui.label(RichText::new(name).size(9.0).strong().color(Color32::WHITE));
                            ui.label(RichText::new(sub).size(8.0).color(muted));
                        });
                    }
                });
                ui.add_space(14.0);
                egui::Frame::new().fill(panel).stroke(egui::Stroke::new(1.0,border)).corner_radius(10.0).inner_margin(12.0).show(ui,|ui|{
                    ui.horizontal(|ui|{
                        ui.label(RichText::new("ACTIVE INSTANCE").size(9.0).strong().color(muted));
                        ui.add_space(8.0);
                        ui.label(RichText::new(&self.settings.instance_name).size(12.0).strong().color(Color32::WHITE));
                        ui.label(RichText::new(format!("Minecraft {}",self.settings.minecraft_version)).size(10.0).color(muted));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{
                            if ui.small_button("Open folder ↗").clicked(){let _=open::that(self.current_instance_dir());}
                        });
                    });
                });
            });
            ui.add_space(gap);
            ui.allocate_ui_with_layout(Vec2::new(rail,ui.available_height()),egui::Layout::top_down(egui::Align::Min),|ui|{
                egui::Frame::new().fill(panel).stroke(egui::Stroke::new(1.0,border)).corner_radius(14.0).inner_margin(15.0).show(ui,|ui|{
                    ui.label(RichText::new("COMMUNITY").size(10.0).strong().color(muted));
                    ui.add_space(10.0);
                    if ui.add(egui::Button::new("◈   Discord support     ↗").fill(Color32::from_rgb(24,36,57)).stroke(egui::Stroke::new(1.0,border)).min_size(Vec2::new(ui.available_width(),38.0))).clicked(){let _=open::that("https://discord.gg/BCWaT9PVhh");}
                    ui.add_space(7.0);
                    if ui.add(egui::Button::new("◇   Browse Modrinth     ↗").fill(Color32::from_rgb(24,36,57)).stroke(egui::Stroke::new(1.0,border)).min_size(Vec2::new(ui.available_width(),38.0))).clicked(){let _=open::that("https://modrinth.com");}
                    ui.add_space(20.0);
                    ui.horizontal(|ui|{
                        ui.label(RichText::new("YOUR SERVERS").size(10.0).strong().color(muted));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{ui.label(RichText::new(self.settings.servers.len().to_string()).size(10.0).color(muted));});
                    });
                    ui.add_space(8.0);
                    if ui.small_button("↻  Refresh all").clicked(){for server in self.settings.servers.clone(){request_server_status(self.tx.clone(),server.address);}self.status("Refreshing saved server statuses...");}
                    ui.add(egui::TextEdit::singleline(&mut self.new_server_name).hint_text("Server name").desired_width(f32::INFINITY));
                    ui.add(egui::TextEdit::singleline(&mut self.new_server_address).hint_text("play.example.net").desired_width(f32::INFINITY));
                    if ui.add_sized([ui.available_width(),30.0],egui::Button::new("＋  ADD SERVER").fill(Color32::from_rgb(23,73,69))).clicked(){
                        let name=self.new_server_name.trim().to_owned();let address=self.new_server_address.trim().to_owned();
                        if !name.is_empty()&&!address.is_empty()&&!address.contains(' '){
                            self.settings.servers.push(CustomServer{name,address:address.clone(),players_online:0,players_max:0,online:false});
                            request_server_status(self.tx.clone(),address);self.new_server_name.clear();self.new_server_address.clear();self.save_settings();
                        }
                    }
                    ui.add_space(6.0);
                    let mut remove=None;
                    for (i,s) in self.settings.servers.clone().iter().enumerate(){
                        ui.horizontal(|ui|{
                            let c=if s.online{accent}else{Color32::from_rgb(226,104,112)};
                            ui.label(RichText::new("●").color(c).size(9.0));
                            ui.vertical(|ui|{
                                ui.label(RichText::new(&s.name).size(11.0).strong().color(Color32::WHITE));
                                ui.label(RichText::new(if s.online{format!("ONLINE  ·  {} players",s.players_online)}else{"OFFLINE".to_owned()}).size(9.0).color(muted));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{if ui.small_button("×").clicked(){remove=Some(i);}});
                        });
                    }
                    if let Some(i)=remove{self.settings.servers.remove(i);self.save_settings();}
                    ui.add_space(18.0);
                    ui.separator();
                    ui.add_space(12.0);
                    ui.label(RichText::new("ACCOUNT").size(10.0).strong().color(muted));
                    ui.add_space(6.0);
                    ui.label(RichText::new(self.account_name.as_deref().unwrap_or("Offline player")).size(12.0).strong().color(Color32::WHITE));
                    if ui.add(egui::Button::new("Microsoft account sign-in  ↗").frame(false).min_size(Vec2::new(ui.available_width(),28.0))).clicked(){self.start_login();}
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(10.0);
                    ui.label(RichText::new("LAUNCHER STATUS").size(9.0).strong().color(muted));
                    let status_text=if self.status.len()>180{format!("{}…",&self.status[..self.status.floor_char_boundary(180)])}else{self.status.clone()};
                    ui.label(RichText::new(status_text).size(10.0).color(if self.status.to_lowercase().contains("error")||self.status.to_lowercase().contains("failed"){Color32::from_rgb(255,151,151)}else{Color32::from_rgb(181,203,214)}));
                });
            });
        });
    }

    fn browser(&mut self, ui: &mut egui::Ui) {
        let title = match self.page { Page::Mods => "Mods", Page::ResourcePacks => "Resource packs", _ => "Shaders" };
        let folder = match self.page {
            Page::Mods => "mods",
            Page::ResourcePacks => "resourcepacks",
            Page::Shaderpacks => "shaderpacks",
            _ => return,
        };
        let directory = self.current_game_dir().join(folder);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(title).size(25.0).strong().color(Color32::WHITE));
                ui.label(RichText::new("Discover and manage your Tensura installation").size(12.0).color(Color32::from_gray(150)));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Open folder ↗").clicked() {
                    let _ = open::that(&directory);
                }
                if ui.selectable_label(self.show_installed, "Installed").clicked() {
                    self.show_installed = true;
                    self.results.clear();
                }
                if ui.selectable_label(!self.show_installed, "Discover").clicked() {
                    self.show_installed = false;
                }
            });
        });
        ui.add_space(14.0);
        if self.show_installed {
            let mut files: Vec<PathBuf> = fs::read_dir(&directory).ok()
                .into_iter().flat_map(|entries| entries.flatten())
                .map(|entry| entry.path()).filter(|path| path.is_file()).collect();
            files.sort_by_key(|path| path.file_name().map(|name| name.to_string_lossy().to_lowercase()).unwrap_or_default());
            ui.label(RichText::new(format!("{} installed files", files.len())).size(11.0).color(Color32::from_rgb(112, 211, 184)));
            ui.add_space(8.0);
            if files.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(45.0);
                    ui.label(RichText::new("Nothing installed here yet").size(17.0).strong().color(Color32::WHITE));
                    ui.label(RichText::new("Browse Modrinth to add something to this folder.").size(12.0).color(Color32::from_gray(150)));
                });
            }
            let mut remove_file: Option<PathBuf> = None;
            egui::ScrollArea::vertical().show(ui, |ui| {
                for path in &files {
                    egui::Frame::new().fill(Color32::from_rgb(13, 22, 31))
                        .stroke(egui::Stroke::new(1.0_f32, Color32::from_rgb(30, 48, 61)))
                        .corner_radius(9.0).inner_margin(12.0).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("▣").size(23.0).color(Color32::from_rgb(84, 210, 177)));
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(path.file_name().unwrap_or_default().to_string_lossy()).size(13.0).strong().color(Color32::WHITE));
                                    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                                    ui.label(RichText::new(format!("{:.1} MB", size as f64 / 1_048_576.0)).size(10.0).color(Color32::from_gray(145)));
                                });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add(egui::Button::new(RichText::new("REMOVE").size(10.0).strong()).fill(Color32::from_rgb(104, 39, 51))).clicked() {
                                        remove_file = Some(path.clone());
                                    }
                                });
                            });
                        });
                    ui.add_space(6.0);
                }
            });
            if let Some(path) = remove_file {
                match fs::remove_file(&path) {
                    Ok(()) => self.status(format!("Removed {}", path.file_name().unwrap_or_default().to_string_lossy())),
                    Err(error) => self.status(format!("Could not remove file: {error}")),
                }
            }
            return;
        }
        ui.horizontal(|ui| {
            let width = (ui.available_width() - 110.0).max(120.0);
            let field = ui.add_sized([width, 38.0], egui::TextEdit::singleline(&mut self.query).hint_text("Search Modrinth..."));
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) { self.search(); }
            if ui.add_sized([100.0, 38.0], egui::Button::new("Search").fill(self.accent())).clicked() { self.search(); }
        });
        ui.add_space(12.0);
        if self.results.is_empty() && !self.busy {
            ui.vertical_centered(|ui| {
                ui.add_space(35.0);
                ui.label(RichText::new("Find your next upgrade").size(18.0).strong().color(Color32::WHITE));
                ui.label(RichText::new("Search Modrinth for compatible projects and install them directly.").size(12.0).color(Color32::from_gray(150)));
            });
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for project in self.results.clone() {
                egui::Frame::new().fill(Color32::from_rgb(13, 22, 31)).stroke(egui::Stroke::new(1.0_f32, Color32::from_rgb(30, 48, 61))).corner_radius(10.0).inner_margin(12.0).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let artwork = if project.icon_url.is_empty() {
                            egui::Image::new(egui::include_image!("../assets/tensura-mark.svg"))
                        } else {
                            egui::Image::from_uri(project.icon_url.clone())
                        };
                        ui.add(artwork.fit_to_exact_size(Vec2::splat(76.0)));
                        ui.add_space(12.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&project.title).size(15.0).strong().color(Color32::WHITE));
                            ui.label(RichText::new(&project.description).size(11.0).color(Color32::from_rgb(164,181,194)));
                            ui.add_space(5.0);
                            ui.label(RichText::new(format!("{} downloads  ·  Modrinth", project.downloads)).size(10.0).color(Color32::from_rgb(104,215,190)));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new(RichText::new("＋ INSTALL").strong()).fill(Color32::from_rgb(44,190,145))).clicked() { self.install_project(project.clone()); }
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
            ui.label(RichText::new("Instances").size(13.0).strong());
            let mut available_instances: Vec<String> = fs::read_dir(Self::data_dir().join("instances")).ok()
                .into_iter().flat_map(|entries| entries.flatten())
                .filter(|entry| entry.path().is_dir())
                .map(|entry| entry.file_name().to_string_lossy().to_string()).collect();
            available_instances.sort();
            let mut selected_instance = self.settings.instance_name.clone();
            egui::ComboBox::from_id_salt("instance_select")
                .selected_text(&selected_instance)
                .show_ui(ui, |ui| {
                    for name in &available_instances {
                        ui.selectable_value(&mut selected_instance, name.clone(), name);
                    }
                });
            let mut delete_instance: Option<String> = None;
            for name in &available_instances {
                if name != &self.settings.instance_name {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(name).size(11.0).color(Color32::from_gray(170)));
                        if ui.small_button("Delete").clicked() { delete_instance = Some(name.clone()); }
                    });
                }
            }
            if let Some(name) = delete_instance {
                match fs::remove_dir_all(Self::instance_dir_for(&name)) {
                    Ok(()) => self.status(format!("Deleted instance {name} and its local game data.")),
                    Err(error) => self.status(format!("Could not delete instance {name}: {error}")),
                }
            }
            if selected_instance != self.settings.instance_name {
                self.settings.instance_name = selected_instance;
                self.save_settings();
                self.installed = self.profile_exists();
                if !self.installed { self.start_install(); }
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_instance_name).hint_text("New instance name"));
                if ui.button("Create & switch").clicked() && !self.new_instance_name.trim().is_empty() {
                    let name = sanitize_instance_name(&self.new_instance_name);
                    self.settings.instance_name = name.clone();
                    self.new_instance_name.clear();
                    let _ = fs::create_dir_all(Self::instance_dir_for(&name));
                    self.save_settings();
                    self.installed = self.profile_exists();
                    if !self.installed { self.start_install(); }
                }
            });
            ui.add_space(8.0);
            let previous_version = self.settings.minecraft_version.clone();
            ui.horizontal(|ui| {
                ui.label("Minecraft version");
                egui::ComboBox::from_id_salt("mc_version").selected_text(&self.settings.minecraft_version).show_ui(ui, |ui| {
                    for version in &self.available_versions {
                        ui.selectable_value(&mut self.settings.minecraft_version, version.clone(), version);
                    }
                });
            });
            if self.settings.minecraft_version != previous_version {
                self.installed = self.profile_exists();
                self.save_settings();
                if !self.installed { self.start_install(); }
            }
            let previous_loader = self.settings.loader.clone();
            ui.horizontal(|ui| {
                ui.label("Mod loader");
                egui::ComboBox::from_id_salt("mod_loader").selected_text(self.settings.loader.to_uppercase()).show_ui(ui, |ui| {
                    for loader in ["fabric", "quilt", "forge", "neoforge"] {
                        ui.selectable_value(&mut self.settings.loader, loader.to_owned(), loader.to_uppercase());
                    }
                });
            });
            if self.settings.loader != previous_loader {
                self.installed = self.profile_exists();
                self.save_settings();
                if !self.installed { self.start_install(); }
            }
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
            ui.horizontal_wrapped(|ui| {
                if ui.button("Repair / reinstall client").clicked() {
                    self.installed = false;
                    self.start_install();
                }
                if ui.button("Open game folder ↗").clicked() { let _ = open::that(self.current_game_dir()); }
                if ui.button("Open instance folder ↗").clicked() { let _ = open::that(self.current_instance_dir()); }
                if ui.button("Open launch logs ↗").clicked() { let _ = open::that(self.current_game_dir().join("logs")); }
            });
            if ui.button("Save settings").clicked() {
                let username = self.settings.offline_username.trim().to_owned();
                if !username.is_empty() {
                    self.settings.offline_username = username.chars().filter(|ch| ch.is_ascii_alphanumeric() || *ch == char::from(95)).take(16).collect();
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
        ui.label(RichText::new(format!("Instance: {}", self.current_instance_dir().display())).size(10.0).color(Color32::from_gray(130)));
        ui.label(RichText::new(format!("Mods: {}", self.current_game_dir().join("mods").display())).size(10.0).color(Color32::from_gray(130)));
        ui.label(RichText::new(format!("Resource packs: {}", self.current_game_dir().join("resourcepacks").display())).size(10.0).color(Color32::from_gray(130)));
        ui.label(RichText::new(format!("Shaders: {}", self.current_game_dir().join("shaderpacks").display())).size(10.0).color(Color32::from_gray(130)));
    }
}

impl eframe::App for TensuraApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages();
        if self.busy { ctx.request_repaint_after(Duration::from_millis(120)); }
        if self.background_frames.len() > 1 { ctx.request_repaint_after(self.background_frames[self.background_index()].1); }
        self.sidebar(ctx);
        egui::TopBottomPanel::top("topbar").exact_height(54.0)
            .frame(egui::Frame::new().fill(Color32::from_rgb(5, 11, 13)).inner_margin(egui::Margin::symmetric(18, 0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("TENSURA").size(16.0).strong().color(Color32::WHITE));
                    ui.label(RichText::new("LAUNCHER").size(9.0).strong().color(Color32::from_rgb(0, 211, 137)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some((version, url)) = self.update_available.clone() {
                            if ui.button(format!("UPDATE {version}")).clicked() { let _ = open::that(url); }
                        }
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


fn version_is_newer(latest: &str, current: &str) -> bool {
    let parse = |value: &str| -> Vec<u64> {
        value.trim_start_matches('v').split('.')
            .map(|part| part.split('-').next().unwrap_or("0").parse::<u64>().unwrap_or(0))
            .collect()
    };
    let latest = parse(latest);
    let current = parse(current);
    for index in 0..latest.len().max(current.len()) {
        let left = *latest.get(index).unwrap_or(&0);
        let right = *current.get(index).unwrap_or(&0);
        if left != right { return left > right; }
    }
    false
}

fn request_server_status(tx: Sender<WorkerMessage>, address: String) {
    thread::spawn(move || {
        let result = reqwest::blocking::Client::builder().timeout(Duration::from_secs(12)).user_agent("TensuraLauncher").build()
            .and_then(|client| client.get(format!("https://api.mcsrvstat.us/3/{address}")).send())
            .and_then(|response| response.error_for_status()).and_then(|response| response.json::<serde_json::Value>());
        let (online, players_online, players_max) = match result {
            Ok(data) => (data["online"].as_bool().unwrap_or(false), data["players"]["online"].as_u64().unwrap_or(0) as u32, data["players"]["max"].as_u64().unwrap_or(0) as u32),
            Err(_) => (false, 0, 0),
        };
        let _ = tx.send(WorkerMessage::ServerStatus { address, online, players_online, players_max });
    });
}

fn fetch_release_versions(tx: Sender<WorkerMessage>) {
    let result = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("TensuraLauncher")
        .build()
        .and_then(|client| client.get("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json").send())
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json::<serde_json::Value>());
    if let Ok(manifest) = result {
        let versions = manifest["versions"].as_array().map(|items| items.iter()
            .filter(|item| item["type"].as_str() == Some("release"))
            .filter_map(|item| item["id"].as_str().map(str::to_owned))
            .collect::<Vec<_>>()).unwrap_or_default();
        let _ = tx.send(WorkerMessage::Versions(versions));
    }
}

fn check_for_updates(tx: Sender<WorkerMessage>) {
    let result = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("TensuraLauncher")
        .build()
        .and_then(|client| client.get("https://api.github.com/repos/dontcare-dev/tensura-launcher/releases/latest").send())
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json::<serde_json::Value>());
    if let Ok(release) = result {
        let version = release["tag_name"].as_str().unwrap_or_default().to_owned();
        let url = release["html_url"].as_str().unwrap_or_default().to_owned();
        if !version.is_empty() && !url.is_empty() && version_is_newer(&version, env!("CARGO_PKG_VERSION")) {
            let _ = tx.send(WorkerMessage::UpdateAvailable { version, url });
        }
    }
}

fn sanitize_instance_name(name: &str) -> String {
    let cleaned: String = name.chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == ' ' || *ch == '-' || *ch == '_')
        .take(32).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() { default_instance_name() } else { cleaned.to_owned() }
}

fn find_profile_version_id(root: &Path, mc_version: &str, loader: &str) -> Option<String> {
    let entries = fs::read_dir(root.join("versions")).ok()?;
    entries.flatten().find_map(|entry| {
        let name = entry.file_name().to_string_lossy().to_string();
        let matches = match loader {
            "fabric" => name.starts_with("fabric-loader-") && name.ends_with(&format!("-{mc_version}")),
            "quilt" => name.starts_with("quilt-loader-") && name.ends_with(&format!("-{mc_version}")),
            "forge" => name.starts_with(&format!("{mc_version}-forge-")),
            "neoforge" => name.starts_with(&format!("{mc_version}-neoforge-")),
            _ => false,
        };
        matches.then_some(name)
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
            TensuraApp::data_dir().join("runtime").join("bin").join("java.exe"),
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
        let runtime_dir = TensuraApp::data_dir().join("runtime");
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
        locate(&runtime_dir).ok_or_else(|| "Java 21 runtime installation did not produce java.exe.".to_owned())
    }
    #[cfg(not(target_os = "windows"))]
    Err("Install Java 21 or newer and make java available on PATH.".to_owned())
}

fn install_tensura(version: &str, loader: &str, instance_dir: &Path, tx: &Sender<WorkerMessage>) -> std::result::Result<String, String> {
    let game_dir = instance_dir.join("game");
    let minecraft_dir = instance_dir.join("minecraft");
    fs::create_dir_all(&game_dir).map_err(|e| e.to_string())?;
    for folder in ["mods", "resourcepacks", "shaderpacks", "screenshots", "saves", "config"] {
        fs::create_dir_all(game_dir.join(folder)).map_err(|e| e.to_string())?;
    }
    let _ = tx.send(WorkerMessage::Status(format!("Installing Minecraft {version} + {loader}...")));
    let launcher = Launcher::new(minecraft_dir);
    let loader_spec = match loader {
        "fabric" => Some(LoaderSpec::Fabric { version: LoaderVersion::LatestStable }),
        "quilt" => Some(LoaderSpec::Quilt { version: LoaderVersion::Latest }),
        "forge" => Some(LoaderSpec::Forge { version: LoaderVersion::Latest }),
        "neoforge" => Some(LoaderSpec::NeoForge { version: LoaderVersion::Latest }),
        _ => return Err(format!("Unsupported mod loader: {loader}")),
    };
    if matches!(loader, "forge" | "neoforge") {
        let java = ensure_java()?;
        if let Some(parent) = java.parent() {
            let mut paths = vec![parent.to_path_buf()];
            paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
            if let Ok(path) = std::env::join_paths(paths) { std::env::set_var("PATH", path); }
        }
    }
    let install_request = InstallRequest {
        minecraft_version: version.to_owned(),
        loader: loader_spec,
        java: JavaInstallPolicy::Auto,
    };
    let progress_tx = tx.clone();
    let mut reporter = |event: mc_launcher_core::progress::ProgressEvent| {
        let _ = progress_tx.send(WorkerMessage::Status("Downloading and preparing Minecraft files…".to_owned()));
    };
    let install = launcher.install_with_progress(install_request, &mut reporter)
        .map_err(|e| e.to_string())?;
    let _ = tx.send(WorkerMessage::Status("Installing compatible performance mods...".to_owned()));
    let mut installed = Vec::new();
    let mut skipped = Vec::new();
    for slug in if loader == "fabric" { PERF_MODS } else { &[] } {
        match download_modrinth_project(slug, "mod", version, loader, &game_dir) {
            Ok(_) => installed.push(*slug),
            Err(_) => skipped.push(*slug),
        }
    }
    let shader = download_modrinth_project("complementary-reimagined", "shader", version, loader, &game_dir);
    let shader_status = if shader.is_ok() { "Complementary Reimagined installed" } else { "Complementary Reimagined unavailable for this Minecraft version" };
    let report = format!("Instance installed: {}. Performance mods: {} installed, {} unavailable for Minecraft {}. {}. Profile: {}",
        install.version_id, installed.len(), skipped.len(), version, shader_status, instance_dir.display());
    Ok(report)
}

fn download_modrinth_project(slug: &str, kind: &str, mc_version: &str, loader: &str, game_dir: &Path) -> std::result::Result<PathBuf, String> {
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(90))
        .user_agent("TensuraLauncher/0.2").build().map_err(|e| e.to_string())?;
    let mut request = client.get(format!("https://api.modrinth.com/v2/project/{}/version", slug))
        .query(&[("game_versions", format!("[\"{}\"]", mc_version))]);
    if kind == "mod" { request = request.query(&[("loaders", format!("[\"{}\"]", loader))]); }
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
        .user_agent("TensuraLauncher/0.2").build().map_err(|e| e.to_string())?;
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
            .with_min_inner_size([1000.0, 650.0]).with_title("Tensura Launcher"),
        ..Default::default()
    };
    eframe::run_native("Tensura Launcher", options, Box::new(|cc| Ok(Box::new(TensuraApp::new(cc)))))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_comparison_handles_semantic_versions() {
        assert!(version_is_newer("v0.3.0", "0.2.9"));
        assert!(!version_is_newer("v0.2.0", "0.2.0"));
        assert!(!version_is_newer("v0.1.9", "0.2.0"));
    }

    #[test]
    fn instance_names_are_path_safe() {
        assert_eq!(sanitize_instance_name("../World?"), "World");
        assert_eq!(sanitize_instance_name("  "), "Tensura Client");
        assert_eq!(sanitize_instance_name("My Survival_01"), "My Survival_01");
    }

    #[test]
    fn finds_each_supported_loader_profile() {
        let root = std::env::temp_dir().join(format!("tensura-test-{}", std::process::id()));
        let versions = root.join("versions");
        fs::create_dir_all(&versions).unwrap();
        for name in [
            "fabric-loader-0.16.9-1.21.1",
            "quilt-loader-0.29.1-1.21.1",
            "1.21.1-forge-52.0.16",
            "1.21.1-neoforge-21.1.1",
        ] {
            fs::create_dir_all(versions.join(name)).unwrap();
        }
        assert!(find_profile_version_id(&root, "1.21.1", "fabric").is_some());
        assert!(find_profile_version_id(&root, "1.21.1", "quilt").is_some());
        assert!(find_profile_version_id(&root, "1.21.1", "forge").is_some());
        assert!(find_profile_version_id(&root, "1.21.1", "neoforge").is_some());
        assert!(find_profile_version_id(&root, "1.20.1", "fabric").is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
