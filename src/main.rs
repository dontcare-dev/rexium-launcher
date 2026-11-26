use eframe::egui;
use base64::Engine;
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
    "entityculling", "modernfix", "dynamic-fps", "moreculling", "noisium",
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

fn svg_icon(ui:&mut egui::Ui,name:&'static str,size:f32,color:Color32){let index=match name{"home"=>0,"mods"=>1,"packs"=>2,"shaders"=>3,"settings"=>4,"discord"=>5,"globe"=>6,"refresh"=>7,"server"=>8,"plus"=>9,"user"=>10,"microsoft"=>11,"status"=>12,"folder"=>13,"play"=>14,"search"=>15,"download"=>16,"trash"=>17,"box"=>18,"cpu"=>19,"memory"=>20,"java"=>21,"palette"=>22,"repair"=>23,"logs"=>24,"chevron"=>25,"sparkles"=>26,"check"=>27,"close"=>28,"monitor"=>29,"rocket"=>30,"shield"=>31,_=>0};let u0=index as f32/32.0;let u1=(index+1) as f32/32.0;let uv=egui::Rect::from_min_max(egui::pos2(u0,0.0),egui::pos2(u1,1.0));ui.add(egui::Image::from_bytes("bytes://tensura-icons",include_bytes!("../assets/tensura-icons.svg")).uv(uv).fit_to_exact_size(Vec2::splat(size)).tint(color));}
fn paint_cover(p:&egui::Painter,t:&egui::TextureHandle,r:egui::Rect,tint:Color32){let z=t.size_vec2();let q=r.size();let sa=z.x/z.y.max(1.0_f32);let ta=q.x/q.y.max(1.0_f32);let uv=if sa>ta{let v=ta/sa;let m=(1.0_f32-v)*0.5_f32;egui::Rect::from_min_max(egui::pos2(m,0.0_f32),egui::pos2(1.0_f32-m,1.0_f32))}else{let v=sa/ta;let m=(1.0_f32-v)*0.5_f32;egui::Rect::from_min_max(egui::pos2(0.0_f32,m),egui::pos2(1.0_f32,1.0_f32-m))};p.image(t.id(),r,uv,tint);}