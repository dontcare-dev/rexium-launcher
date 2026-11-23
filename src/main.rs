use eframe::egui;
use egui::{Color32, RichText, Vec2};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

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
    animated_background: bool,
    logo: Option<egui::TextureHandle>,
}

impl RexiumApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals = egui::Visuals::dark();
        style.visuals.window_fill = Color32::from_rgb(12, 13, 20);
        style.visuals.panel_fill = Color32::from_rgb(12, 13, 20);
        style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(27, 29, 41);
        style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(43, 45, 62);
        style.visuals.widgets.active.bg_fill = Color32::from_rgb(57, 59, 82);
        style.spacing.item_spacing = Vec2::new(12.0, 12.0);
        cc.egui_ctx.set_style(style);
        let logo = Self::load_logo(&cc.egui_ctx);
        Self {
            page: Page::Home,
            query: String::new(),
            results: Vec::new(),
            status: "Ready".to_owned(),
            accent_hue: 0.68,
            animated_background: true,
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
            .replace('/', "_").replace('\\', "_");
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

    fn background(&self, ctx: &egui::Context) {
        let rect = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::background());
        painter.rect_filled(rect, 0.0, Color32::from_rgb(9, 10, 16));
        let time = ctx.input(|input| input.time) as f32;
        let accent = self.accent();
        let glow = Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 18);
        painter.circle_filled(
            egui::pos2(rect.right() - 80.0, rect.top() + 90.0),
            320.0,
            glow,
        );
        painter.circle_filled(
            egui::pos2(rect.left() + 40.0, rect.bottom() + 60.0),
            260.0,
            Color32::from_rgba_unmultiplied(62, 186, 176, 10),
        );
        if self.animated_background {
            for i in 0..18 {
                let phase = time * (0.24 + (i % 4) as f32 * 0.06) + i as f32 * 1.71;
                let x = rect.left() + (i as f32 * 97.0) % rect.width().max(1.0);
                let y = rect.top() + ((i as f32 * 53.0 + phase.sin() * 34.0 + time * 7.0) % rect.height().max(1.0));
                let radius = 1.5 + (i % 3) as f32;
                painter.circle_filled(
                    egui::pos2(x, y),
                    radius,
                    Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 75),
                );
            }
            ctx.request_repaint_after(Duration::from_millis(32));
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
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(title).size(27.0).strong().color(Color32::WHITE));
                ui.label(RichText::new(subtitle).size(13.0).color(Color32::from_rgb(145, 150, 170)));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("REXIUM").size(12.0).strong().color(self.accent()));
            });
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
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.label(RichText::new("Rexium Launcher  ·  0.1.0").size(10.0).color(Color32::from_gray(100)));
                    ui.label(RichText::new(&self.status).size(10.0).color(self.accent()));
                });
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
                                ui.checkbox(&mut self.animated_background, "Animated floating background");
                                ui.label(RichText::new("Background artwork is built from lightweight SVG-style vector shapes.")
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

        egui::TopBottomPanel::bottom("rexium_status")
            .frame(egui::Frame::new().fill(Color32::from_rgba_unmultiplied(11, 12, 18, 240)).inner_margin(egui::Margin::symmetric(20, 8)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(self.accent()).size(10.0));
                    ui.label(RichText::new(&self.status).size(11.0).color(Color32::from_gray(150)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("MODRINTH").size(10.0).strong().color(Color32::from_gray(110)));
                    });
                });
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
