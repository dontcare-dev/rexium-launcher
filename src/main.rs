use eframe::egui;
use egui::{Color32, RichText, Vec2};
use serde::Deserialize;
use std::fs;
use std::io::BufReader;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const LOGO_URL: &str = "https://i.ibb.co/fz6RJNkH/8da0faf8b5a4ac0b1d18647577e404b8.jpg";

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Home,
    Mods,
    Modpacks,
    ResourcePacks,
    Settings,
}

impl Page {
    fn label(self) -> &'static str {
        match self {
            Self::Home => "Overview",
            Self::Mods => "Mods",
            Self::Modpacks => "Modpacks",
            Self::ResourcePacks => "Resource packs",
            Self::Settings => "Settings",
        }
    }

    fn project_type(self) -> &'static str {
        match self {
            Self::Mods => "mod",
            Self::Modpacks => "modpack",
            Self::ResourcePacks => "resourcepack",
            _ => "",
        }
    }
}

#[derive(Clone, Deserialize)]
struct Project {
    project_id: String,
    title: String,
    description: String,
    downloads: u64,
    slug: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    hits: Vec<Project>,
}

struct RexiumApp {
    page: Page,
    query: String,
    results: Vec<Project>,
    status: String,
    accent_hue: f32,
    background_frames: Vec<(egui::TextureHandle, Duration)>,
    background_started: Instant,
    background_path: String,
    logo: Option<egui::TextureHandle>,
}

impl RexiumApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals = egui::Visuals::dark();
        style.visuals.window_fill = Color32::from_rgb(12, 17, 14);
        style.visuals.panel_fill = Color32::from_rgb(12, 17, 14);
        style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(25, 37, 29);
        style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(37, 58, 43);
        style.visuals.widgets.active.bg_fill = Color32::from_rgb(48, 78, 55);
        style.spacing.item_spacing = Vec2::new(12.0, 12.0);
        cc.egui_ctx.set_style(style);
        let logo = Self::load_logo(&cc.egui_ctx);
        Self {
            page: Page::Home,
            query: String::new(),
            results: Vec::new(),
            status: "Ready".to_owned(),
            accent_hue: 0.36,
            background_frames: Vec::new(),
            background_started: Instant::now(),
            background_path: String::new(),
            logo,
        }
    }

    fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
        let bytes = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .ok()?
            .get(LOGO_URL)
            .send()
            .ok()?
            .bytes()
            .ok()?;
        let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
        let size = [image.width() as usize, image.height() as usize];
        let pixels = image.into_raw();
        Some(ctx.load_texture(
            "rexium-client-logo",
            egui::ColorImage::from_rgba_unmultiplied(size, &pixels),
            egui::TextureOptions::LINEAR,
        ))
    }

    fn accent(&self) -> Color32 {
        Color32::from(egui::ecolor::Hsva::new(self.accent_hue, 0.66, 0.98, 1.0))
    }

    fn search(&mut self) {
        let kind = self.page.project_type();
        if kind.is_empty() {
            return;
        }
        let query = self.query.trim();
        let encoded_query = urlencoding::encode(query);
        let facet_query = format!("[[\"project_type:{}\"]]", kind);
        let facets = urlencoding::encode(&facet_query);
        let url = format!(
            "https://api.modrinth.com/v2/search?query={}&facets={}&limit=30&index=relevance",
            encoded_query, facets
        );
        self.status = "Searching Modrinth…".to_owned();
        match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("RexiumLauncher/0.1")
            .build()
            .and_then(|client| client.get(url).send())
            .and_then(|response| response.error_for_status())
            .and_then(|response| response.json::<SearchResponse>())
        {
            Ok(response) => {
                self.results = response.hits;
                self.status = format!("{} results", self.results.len());
            }
            Err(error) => {
                self.results.clear();
                self.status = format!("Search failed: {}", error);
            }
        }
    }

    fn minecraft_dir() -> PathBuf {
        #[cfg(target_os = "windows")]
        {
            if let Ok(appdata) = std::env::var("APPDATA") {
                return PathBuf::from(appdata).join(".minecraft");
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(home) = dirs::home_dir() {
                return home.join("Library/Application Support/minecraft");
            }
        }
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".minecraft")
    }

    fn install_project(&mut self, project: &Project, kind: &str) {
        self.status = format!("Getting {}…", project.title);
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(45))
            .user_agent("RexiumLauncher/0.1")
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                self.status = format!("Could not start download: {}", error);
                return;
            }
        };
        let versions_url = format!(
            "https://api.modrinth.com/v2/project/{}/version",
            project.project_id
        );
        let versions: Vec<serde_json::Value> = match client.get(versions_url).send()
            .and_then(|response| response.error_for_status())
            .and_then(|response| response.json())
        {
            Ok(versions) => versions,
            Err(error) => {
                self.status = format!("Could not load project versions: {}", error);
                return;
            }
        };
        let mut selected_file = None;
        for version in versions {
            if let Some(files) = version.get("files").and_then(|value| value.as_array()) {
                selected_file = files.iter().find(|file| file.get("primary").and_then(|v| v.as_bool()) == Some(true))
                    .or_else(|| files.first())
                    .cloned();
                if selected_file.is_some() {
                    break;
                }
            }
        }
        let file = match selected_file {
            Some(file) => file,
            None => {
                self.status = "No downloadable file was found for this project.".to_owned();
                return;
            }
        };
        let url = match file.get("url").and_then(|value| value.as_str()) {
            Some(url) => url.to_owned(),
            None => {
                self.status = "The download URL is missing.".to_owned();
                return;
            }
        };
        let filename = file.get("filename").and_then(|value| value.as_str())
            .unwrap_or("download")
            .replace(['/', '\\'], "_");
        let target_dir = match kind {
            "mod" => Self::minecraft_dir().join("mods"),
            "resourcepack" => Self::minecraft_dir().join("resourcepacks"),
            _ => dirs::data_dir().unwrap_or_else(|| PathBuf::from("."))
                .join("RexiumLauncher").join("Downloads"),
        };
        if let Err(error) = fs::create_dir_all(&target_dir) {
            self.status = format!("Could not create install folder: {}", error);
            return;
        }
        self.status = format!("Downloading {}…", filename);
        match client.get(url).send().and_then(|response| response.error_for_status())
            .and_then(|response| response.bytes())
        {
            Ok(bytes) => {
                let destination = target_dir.join(&filename);
                match fs::write(&destination, &bytes) {
                    Ok(()) => {
                        self.status = match kind {
                            "mod" => format!("Installed {} to .minecraft/mods", project.title),
                            "resourcepack" => format!("Installed {} to .minecraft/resourcepacks", project.title),
                            _ => format!("Downloaded {} to RexiumLauncher/Downloads", project.title),
                        };
                    }
                    Err(error) => self.status = format!("Could not save file: {}", error),
                }
            }
            Err(error) => self.status = format!("Download failed: {}", error),
        }
    }

    fn load_custom_background(&mut self, ctx: &egui::Context, path: PathBuf) -> Result<(), String> {
        let is_gif = path.extension().and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gif"));
        let mut decoded = Vec::new();
        if is_gif {
            let file = std::fs::File::open(&path).map_err(|error| error.to_string())?;
            let decoder = image::codecs::gif::GifDecoder::new(BufReader::new(file))
                .map_err(|error| error.to_string())?;
            let frames = image::AnimationDecoder::into_frames(decoder)
                .collect_frames().map_err(|error| error.to_string())?;
            for frame in frames {
                decoded.push((frame.into_buffer(), Duration::from_millis(100)));
            }
        } else {
            let image = image::open(&path).map_err(|error| error.to_string())?.to_rgba8();
            decoded.push((image, Duration::from_secs(1)));
        }
        if decoded.is_empty() {
            return Err("The selected image contains no frames.".to_owned());
        }
        self.background_frames = decoded.into_iter().enumerate().map(|(index, (image, delay))| {
            let size = [image.width() as usize, image.height() as usize];
            let pixels = image.into_raw();
            let texture = ctx.load_texture(
                format!("rexium-custom-background-{index}"),
                egui::ColorImage::from_rgba_unmultiplied(size, &pixels),
                egui::TextureOptions::LINEAR,
            );
            (texture, delay)
        }).collect();
        self.background_started = Instant::now();
        self.background_path = path.display().to_string();
        Ok(())
    }

    fn background(&self, ctx: &egui::Context) {
        let rect = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::background());
        painter.rect_filled(rect, 0.0, Color32::from_rgb(11, 16, 12));
        if self.background_frames.is_empty() {
            return;
        }
        let total: u128 = self.background_frames.iter().map(|(_, delay)| delay.as_millis()).sum();
        let elapsed = if total > 0 {
            self.background_started.elapsed().as_millis() % total
        } else {
            0
        };
        let mut position = 0_u128;
        let mut selected = 0;
        for (index, (_, delay)) in self.background_frames.iter().enumerate() {
            position += delay.as_millis();
            if elapsed < position {
                selected = index;
                break;
            }
        }
        let texture = &self.background_frames[selected].0;
        painter.image(
            texture.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        if self.background_frames.len() > 1 {
            ctx.request_repaint_after(self.background_frames[selected].1);
        }
    }

    fn nav_button(&mut self, ui: &mut egui::Ui, page: Page) {
        let active = self.page == page;
        let accent = self.accent();
        let response = ui.add_sized(
            [ui.available_width(), 42.0],
            egui::Button::new(
                RichText::new(page.label())
                    .size(14.0)
                    .color(if active { Color32::WHITE } else { Color32::from_rgb(169, 173, 190) }),
            )
            .fill(if active {
                Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 42)
            } else {
                Color32::TRANSPARENT
            })
            .corner_radius(10.0),
        );
        if response.clicked() {
            self.page = page;
            self.results.clear();
            self.status = "Ready".to_owned();
        }
    }

    fn header(&self, ui: &mut egui::Ui, title: &str, subtitle: &str) {
        ui.vertical(|ui| {
            ui.label(RichText::new(title).size(27.0).strong().color(Color32::WHITE));
            ui.label(RichText::new(subtitle).size(13.0).color(Color32::from_rgb(145, 160, 145)));
        });
        ui.add_space(18.0);
    }

    fn content_browser(&mut self, ui: &mut egui::Ui) {
        let title = match self.page {
            Page::Mods => "Mods",
            Page::Modpacks => "Modpacks",
            _ => "Resource packs",
        };
        self.header(ui, title, "Discover and download community projects from Modrinth.");
        ui.horizontal(|ui| {
            let search = ui.add_sized(
                [ui.available_width() - 112.0, 42.0],
                egui::TextEdit::singleline(&mut self.query).hint_text("Search projects…"),
            );
            if search.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                self.search();
            }
            if ui.add_sized([100.0, 42.0], egui::Button::new("Search").fill(self.accent())).clicked() {
                self.search();
            }
        });
        ui.add_space(14.0);
        if self.results.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(65.0);
                ui.label(RichText::new("⌕").size(42.0).color(self.accent()));
                ui.label(RichText::new("Find something to play").size(18.0).strong());
                ui.label(RichText::new("Search Modrinth to browse available projects.").color(Color32::from_gray(145)));
            });
        } else {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for project in self.results.clone() {
                    egui::Frame::new()
                        .fill(Color32::from_rgba_unmultiplied(24, 26, 37, 225))
                        .stroke(egui::Stroke::new(1.0_f32, Color32::from_gray(42)))
                        .corner_radius(12.0)
                        .inner_margin(14.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(&project.title).size(16.0).strong().color(Color32::WHITE));
                                    ui.label(RichText::new(&project.description).size(12.0).color(Color32::from_rgb(157, 162, 180)));
                                    ui.label(RichText::new(format!("{} downloads  ·  {}", project.downloads, project.slug))
                                        .size(11.0).color(Color32::from_rgb(120, 127, 149)));
                                });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let label = if self.page == Page::Modpacks { "Download" } else { "Install" };
                                    if ui.add(egui::Button::new(label).fill(self.accent())).clicked() {
                                        self.install_project(&project, self.page.project_type());
                                    }
                                });
                            });
                        });
                    ui.add_space(8.0);
                }
            });
        }
    }
}

impl eframe::App for RexiumApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.background(ctx);
        egui::SidePanel::left("rexium_navigation")
            .exact_width(218.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(Color32::from_rgba_unmultiplied(13, 14, 22, 242)).inner_margin(16.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(logo) = &self.logo {
                        ui.add(egui::Image::new((logo.id(), Vec2::splat(42.0))));
                    } else {
                        ui.label(RichText::new("R").size(31.0).strong().color(self.accent()));
                    }
                    ui.vertical(|ui| {
                        ui.label(RichText::new("REXIUM").size(18.0).strong().color(Color32::WHITE));
                        ui.label(RichText::new("MINECRAFT").size(9.0).strong().color(Color32::from_gray(125)));
                    });
                });
                ui.add_space(34.0);
                ui.label(RichText::new("LIBRARY").size(10.0).strong().color(Color32::from_gray(112)));
                ui.add_space(8.0);
                self.nav_button(ui, Page::Home);
                self.nav_button(ui, Page::Mods);
                self.nav_button(ui, Page::Modpacks);
                self.nav_button(ui, Page::ResourcePacks);
                ui.add_space(22.0);
                ui.label(RichText::new("PREFERENCES").size(10.0).strong().color(Color32::from_gray(112)));
                ui.add_space(8.0);
                self.nav_button(ui, Page::Settings);
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::TRANSPARENT).inner_margin(26.0))
            .show(ctx, |ui| {
                match self.page {
                    Page::Home => {
                        self.header(ui, "Rexium Launcher", "Rexium Launcher is a Minecraft launcher built in Rust.");
                        egui::Frame::new()
                            .fill(Color32::from_rgba_unmultiplied(28, 30, 45, 225))
                            .stroke(egui::Stroke::new(1.0_f32, Color32::from_gray(48)))
                            .corner_radius(18.0)
                            .inner_margin(24.0)
                            .show(ui, |ui| {
                                ui.label(RichText::new("REXIUM").size(12.0).strong().color(self.accent()));
                                ui.add_space(8.0);
                                ui.label(RichText::new("Everything you need.\nNothing in your way.").size(29.0).strong().color(Color32::WHITE));
                                ui.add_space(8.0);
                                ui.add_space(18.0);
                                ui.horizontal(|ui| {
                                    if ui.add(egui::Button::new("Browse mods").fill(self.accent()).min_size(Vec2::new(130.0, 40.0))).clicked() {
                                        self.page = Page::Mods;
                                        self.results.clear();
                                    }
                                    if ui.add(egui::Button::new("Explore modpacks").min_size(Vec2::new(150.0, 40.0))).clicked() {
                                        self.page = Page::Modpacks;
                                        self.results.clear();
                                    }
                                });
                            });
                        ui.add_space(20.0);
                        ui.label(RichText::new("CONTENT LIBRARY").size(11.0).strong().color(Color32::from_gray(130)));
                        ui.add_space(10.0);
                        ui.columns(3, |columns| {
                            for (index, (name, detail, page)) in [
                                ("Mods", "Add game modifications", Page::Mods),
                                ("Modpacks", "Download curated setups", Page::Modpacks),
                                ("Resource packs", "Change your game's look", Page::ResourcePacks),
                            ].iter().enumerate() {
                                egui::Frame::new().fill(Color32::from_rgba_unmultiplied(23, 25, 36, 230))
                                    .stroke(egui::Stroke::new(1.0_f32, Color32::from_gray(43)))
                                    .corner_radius(14.0).inner_margin(16.0).show(&mut columns[index], |ui| {
                                        ui.label(RichText::new(*name).size(17.0).strong().color(Color32::WHITE));
                                        ui.label(RichText::new(*detail).size(11.0).color(Color32::from_gray(145)));
                                        ui.add_space(14.0);
                                        if ui.button("Open library  →").clicked() {
                                            self.page = *page;
                                            self.results.clear();
                                        }
                                    });
                            }
                        });
                    }
                    Page::Mods | Page::Modpacks | Page::ResourcePacks => self.content_browser(ui),
                    Page::Settings => {
                        self.header(ui, "Settings", "Make Rexium feel like yours.");
                        egui::Frame::new().fill(Color32::from_rgba_unmultiplied(24, 26, 37, 230))
                            .stroke(egui::Stroke::new(1.0_f32, Color32::from_gray(44)))
                            .corner_radius(14.0).inner_margin(20.0).show(ui, |ui| {
                                ui.label(RichText::new("Appearance").size(18.0).strong());
                                ui.add_space(12.0);
                                ui.label("Accent color");
                                ui.add(egui::Slider::new(&mut self.accent_hue, 0.0..=1.0).show_value(false));
                                ui.horizontal(|ui| {
                                    ui.label("Preview");
                                    ui.colored_label(self.accent(), "Rexium accent");
                                });
                                ui.add_space(16.0);
                                ui.label("Background image or GIF");
                                ui.horizontal(|ui| {
                                    if ui.button("Choose file").clicked() {
                                        if let Some(path) = rfd::FileDialog::new()
                                            .add_filter("Images and GIFs", &["png", "jpg", "jpeg", "webp", "gif"])
                                            .pick_file()
                                        {
                                            if let Err(error) = self.load_custom_background(ctx, path) {
                                                self.status = error;
                                            }
                                        }
                                    }
                                    if ui.button("Remove background").clicked() {
                                        self.background_frames.clear();
                                        self.background_path.clear();
                                    }
                                });
                                if !self.background_path.is_empty() {
                                    ui.label(RichText::new(&self.background_path).size(11.0).color(Color32::from_gray(145)));
                                }
                                ui.label(RichText::new("PNG, JPG, WEBP and animated GIF supported.")
                                    .size(11.0).color(Color32::from_gray(135)));
                            });
                        ui.add_space(14.0);
                        egui::Frame::new().fill(Color32::from_rgba_unmultiplied(24, 26, 37, 230))
                            .corner_radius(14.0).inner_margin(20.0).show(ui, |ui| {
                                ui.label(RichText::new("Minecraft folders").size(17.0).strong());
                                ui.add_space(8.0);
                                ui.label(RichText::new(Self::minecraft_dir().display().to_string()).size(12.0).color(Color32::from_gray(155)));
                                ui.label(RichText::new("Mods and resource packs are installed into this Minecraft directory.")
                                    .size(11.0).color(Color32::from_gray(125)));
                            });
                    }
                }
            });


    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([900.0, 620.0])
            .with_title("Rexium Launcher"),
        ..Default::default()
    };
    eframe::run_native(
        "Rexium Launcher",
        options,
        Box::new(|cc| Ok(Box::new(RexiumApp::new(cc)))),
    )
}
