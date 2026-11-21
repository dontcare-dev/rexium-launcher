use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde::Serialize;
use sha1::{Digest, Sha1};
use std::{collections::{HashMap, HashSet}, fs, io::{Cursor, Read}, path::{Component, Path, PathBuf}};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

const API: &str = "https://api.modrinth.com/v2";

#[derive(Clone, Debug, Deserialize)]
pub struct Project {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub downloads: u64,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(skip)]
    pub icon_rgba: Option<(usize, usize, Vec<u8>)>,
}

#[derive(Deserialize)]
struct ProjectVersion {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    files: Vec<VersionFile>,
    #[serde(default)]
    dependencies: Vec<VersionDependency>,
}

#[derive(Clone, Deserialize)]
struct VersionDependency {
    project_id: Option<String>,
    version_id: Option<String>,
    dependency_type: Option<String>,
}

#[derive(Deserialize)]
struct VersionFile {
    url: String,
    filename: String,
    #[serde(default)]
    primary: bool,
    hashes: Option<std::collections::HashMap<String, String>>,
}

#[derive(Deserialize)]
struct PackManifest {
    name: String,
    #[serde(rename = "versionId", alias = "version_id")]
    version_id: String,
    dependencies: std::collections::HashMap<String, String>,
    files: Vec<PackFile>,
}

#[derive(Deserialize)]
struct PackFile {
    path: String,
    hashes: std::collections::HashMap<String, String>,
    downloads: Vec<String>,
    env: Option<std::collections::HashMap<String, String>>,
}

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("LUNAR/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(90))
        .build()?)
}

fn safe_instance_component(value: &str) -> Result<&str> {
    if value.is_empty()
        || value.len() > 80
        || value == "."
        || value == ".."
        || !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
    {
        bail!("The modpack manifest contains an unsafe instance identifier");
    }
    Ok(value)
}

fn safe_relative(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if path.as_os_str().is_empty() || path.is_absolute()
        || value.contains(['\\', ':'])
        || path.components().any(|part| !matches!(part, Component::Normal(_)))
    {
        bail!("The downloaded package contains an unsafe file path");
    }
    Ok(path.to_path_buf())
}


pub fn mark_project_installed(root: &Path, kind: &str, project_id: &str, installed_paths: &str) -> Result<()> {
    let path = root.join(".lunar-installed-content.json");
    let mut registry: serde_json::Value = if path.exists() {
        serde_json::from_slice(&fs::read(&path)?)?
    } else {
        serde_json::json!({})
    };
    let entries = registry.as_object_mut().context("Installed content registry is invalid")?;
    let projects = entries.entry(kind.to_owned()).or_insert_with(|| serde_json::json!({}));
    let projects = projects.as_object_mut().context("Installed content registry entry is invalid")?;
    let paths = installed_paths.lines().filter(|value| !value.trim().is_empty()).collect::<Vec<_>>();
    projects.insert(project_id.to_owned(), serde_json::json!(paths));
    fs::write(path, serde_json::to_vec_pretty(&registry)?)?;
    Ok(())
}

pub fn project_is_installed(root: &Path, kind: &str, project_id: &str) -> bool {
    let path = root.join(".lunar-installed-content.json");
    fs::read(path).ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|registry| registry.get(kind).and_then(serde_json::Value::as_object).cloned())
        .and_then(|projects| projects.get(project_id).and_then(serde_json::Value::as_array).cloned())
        .is_some_and(|paths| !paths.is_empty() && paths.iter().all(|value| {
            value.as_str().is_some_and(|path| Path::new(path).exists())
        }))
}

pub fn search(query: &str, kind: &str) -> Result<Vec<Project>> {
    search_sorted(query, kind, "relevance")
}

pub fn search_sorted(query: &str, kind: &str, sort: &str) -> Result<Vec<Project>> {
    if !matches!(kind, "modpack" | "resourcepack" | "mod" | "shader") { bail!("Unsupported Modrinth project type"); }
    let sort = if matches!(sort, "downloads" | "follows" | "updated") { sort } else { "relevance" };
    let facets = format!("[[\"project_type:{kind}\"]]");
    let response = client()?.get(format!("{API}/search"))
        .query(&[("query", query), ("facets", facets.as_str()), ("limit", "30"), ("index", sort)])
        .send()?.error_for_status()?.json::<serde_json::Value>()?;
    let mut projects: Vec<Project> = serde_json::from_value(response.get("hits").cloned().unwrap_or_default())?;
    let http = client()?;
    for project in projects.iter_mut().take(12) {
        let Some(url) = project.icon_url.as_deref() else { continue };
        let Ok(bytes) = http.get(url).timeout(std::time::Duration::from_secs(5)).send().and_then(reqwest::blocking::Response::error_for_status).and_then(|response| response.bytes()) else { continue };
        let Ok(image) = image::load_from_memory(&bytes) else { continue };
        let image = image.resize(88, 88, image::imageops::FilterType::Triangle).to_rgba8();
        project.icon_rgba = Some((image.width() as usize, image.height() as usize, image.into_raw()));
    }
    Ok(projects)
}

fn project_versions(project_id: &str, game_versions: Option<&str>, loaders: Option<&str>) -> Result<Vec<ProjectVersion>> {
    let http = client()?;
    let mut request = http.get(format!("{API}/project/{project_id}/version"));
    let game_versions_json = game_versions.map(|version| serde_json::to_string(&vec![version])).transpose()?;
    let loaders_json = loaders.filter(|loader| !loader.is_empty()).map(|loader| serde_json::to_string(&vec![loader])).transpose()?;
    if let Some(value) = game_versions_json.as_deref() {
        request = request.query(&[("game_versions", value)]);
    }
    if let Some(value) = loaders_json.as_deref() {
        request = request.query(&[("loaders", value)]);
    }
    Ok(request.send()?.error_for_status()?.json::<Vec<ProjectVersion>>()?)
}

fn safe_filename(filename: &str) -> Result<&str> {
    let path = Path::new(filename);
    if filename.trim().is_empty()
        || filename.contains('/') || filename.contains('\\')
        || path.file_name().and_then(|name| name.to_str()) != Some(filename)
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        bail!("Modrinth returned an unsafe download filename");
    }
    Ok(filename)
}


fn normalize_pack_archive(bytes: &[u8], kind: &str) -> Result<Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .with_context(|| format!("The downloaded {kind} is not a valid ZIP archive"))?;
    if archive.is_empty() { bail!("The downloaded {kind} archive contains no files"); }
    let names = (0..archive.len())
        .filter_map(|index| archive.by_index(index).ok().map(|file| file.name().to_owned()))
        .filter(|name| !name.ends_with('/'))
        .collect::<Vec<_>>();
    let marker_path = names.iter().find(|name| {
        let lower = name.to_ascii_lowercase();
        if kind == "resource pack" {
            lower == "pack.mcmeta" || lower.ends_with("/pack.mcmeta")
        } else {
            lower.starts_with("shaders/") || lower.contains("/shaders/")
        }
    }).with_context(|| {
        if kind == "resource pack" {
            "This ZIP does not contain pack.mcmeta, so Minecraft cannot recognize it as a resource pack"
        } else {
            "This ZIP does not contain a shaders/ folder, so Minecraft cannot recognize it as a shader pack"
        }
    })?;
    let strip_prefix = if kind == "resource pack" {
        marker_path.strip_suffix("pack.mcmeta").unwrap_or("")
    } else {
        marker_path.split_once("shaders/").map(|(prefix, _)| prefix).unwrap_or("")
    };
    if strip_prefix.is_empty() { return Ok(bytes.to_vec()); }
    let mut output = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut output);
        let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut written = HashSet::new();
        for index in 0..archive.len() {
            let mut file = archive.by_index(index)?;
            let original = file.name().to_owned();
            let Some(name) = original.strip_prefix(strip_prefix) else { continue };
            if name.is_empty() || !written.insert(name.to_owned()) { continue; }
            if file.is_dir() {
                writer.add_directory(name, options)?;
            } else {
                writer.start_file(name, options)?;
                std::io::copy(&mut file, &mut writer)?;
            }
        }
        writer.finish()?;
    }
    Ok(output.into_inner())
}

fn save_project_file(project_id: &str, root: &Path, folder: &str, kind: &str, game_version: Option<&str>, loader: Option<&str>) -> Result<String> {
    let versions = project_versions(project_id, game_version, loader)
        .with_context(|| format!("Could not retrieve {kind} versions from Modrinth"))?;
    let version = versions.iter().find(|version| !version.files.is_empty())
        .context(if game_version.is_some() { "No compatible downloadable version was found. Check the selected Minecraft version and loader." } else { "This project has no downloadable versions." })?;
    let file = version.files.iter().find(|file| file.primary).or_else(|| version.files.first())
        .context("The selected version contains no downloadable files")?;
    let filename = safe_filename(&file.filename)?;
    if kind != "mod" && !filename.to_ascii_lowercase().ends_with(".zip") {
        bail!("The selected {kind} is not a ZIP archive");
    }
    let http = client()?;
    let bytes = http.get(&file.url).send()
        .with_context(|| format!("Could not connect to the {kind} download host"))?
        .error_for_status()
        .with_context(|| format!("The {kind} download host rejected the request"))?
        .bytes()
        .with_context(|| format!("Could not read the {kind} download"))?;
    if bytes.is_empty() { bail!("The downloaded {kind} file was empty"); }
    if let Some(expected) = file.hashes.as_ref().and_then(|hashes| hashes.get("sha1"))
        && !format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(expected)
    {
        bail!("{kind} checksum verification failed");
    }
    let bytes = if kind == "mod" { bytes.to_vec() } else { normalize_pack_archive(&bytes, kind)? };
    let directory = root.join(folder);
    fs::create_dir_all(&directory)?;
    let mut destination = directory.join(filename);
    if destination.exists() {
        let stem = Path::new(filename).file_stem().and_then(|value| value.to_str()).unwrap_or("pack");
        let extension = Path::new(filename).extension().and_then(|value| value.to_str()).unwrap_or("zip");
        let mut suffix = 2usize;
        loop {
            destination = directory.join(format!("{stem}-{suffix}.{extension}"));
            if !destination.exists() { break; }
            suffix += 1;
        }
    }
    let temporary = destination.with_extension(format!("{}.lunar-download", destination.extension().and_then(|value| value.to_str()).unwrap_or("zip")));
    fs::write(&temporary, &bytes)?;
    if let Err(error) = fs::rename(&temporary, &destination) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(destination.display().to_string())
}

pub fn import_pack_file(source: &Path, root: &Path, folder: &str, kind: &str) -> Result<String> {
    if !source.is_file() { bail!("Selected package file was not found"); }
    let filename = source.file_name().and_then(|name| name.to_str()).context("Selected package has no valid filename")?;
    safe_filename(filename)?;
    if !filename.to_ascii_lowercase().ends_with(".zip") { bail!("The selected {kind} must be a ZIP archive"); }
    let original = fs::read(source)?;
    let bytes = normalize_pack_archive(&original, kind)?;
    let directory = root.join(folder);
    fs::create_dir_all(&directory)?;
    let mut destination = directory.join(filename);
    if destination.exists() {
        let stem = Path::new(filename).file_stem().and_then(|value| value.to_str()).unwrap_or("pack");
        let mut suffix = 2usize;
        loop {
            destination = directory.join(format!("{stem}-{suffix}.zip"));
            if !destination.exists() { break; }
            suffix += 1;
        }
    }
    let temporary = destination.with_extension("zip.lunar-download");
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, &destination)?;
    Ok(destination.display().to_string())
}

pub fn install_resource_pack(project_id: &str, root: &Path, _minecraft: &str) -> Result<String> {
    save_project_file(project_id, root, "resourcepacks", "resource pack", None, None)
}

pub fn install_shader_pack(project_id: &str, root: &Path, _minecraft: &str) -> Result<String> {
    save_project_file(project_id, root, "shaderpacks", "shader pack", None, None)
}

fn discover_modpack_dependencies(files: &[PackFile], instance: &Path) -> Result<Vec<VersionDependency>> {
    let mut hashes = files.iter()
        .filter(|item| item.path.to_ascii_lowercase().ends_with(".jar"))
        .filter_map(|item| item.hashes.get("sha1").cloned())
        .collect::<HashSet<_>>();
    let mods_directory = instance.join("mods");
    if mods_directory.exists() {
        for entry in fs::read_dir(&mods_directory)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() { continue; }
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()).is_none_or(|value| !value.eq_ignore_ascii_case("jar")) { continue; }
            let bytes = fs::read(path)?;
            hashes.insert(format!("{:x}", Sha1::digest(&bytes)));
        }
    }
    let hashes = hashes.into_iter().collect::<Vec<_>>();
    if hashes.is_empty() { return Ok(Vec::new()); }
    let http = client()?;
    let queue = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(hashes)));
    let dependencies = std::sync::Arc::new(std::sync::Mutex::new(Vec::<VersionDependency>::new()));
    let workers = queue.lock().map_err(|_| anyhow!("Dependency queue is unavailable"))?.len().min(4);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let queue = std::sync::Arc::clone(&queue);
            let dependencies = std::sync::Arc::clone(&dependencies);
            let http = http.clone();
            scope.spawn(move || loop {
                let hash = queue.lock().ok().and_then(|mut queue| queue.pop_front());
                let Some(hash) = hash else { break };
                if let Ok(response) = http.get(format!("{API}/version_file/{hash}?algorithm=sha1")).send()
                    && let Ok(response) = response.error_for_status()
                    && let Ok(version) = response.json::<serde_json::Value>()
                    && let Some(values) = version.get("dependencies").cloned()
                    && let Ok(mut found) = serde_json::from_value::<Vec<VersionDependency>>(values)
                    && let Ok(mut dependencies) = dependencies.lock()
                {
                    dependencies.append(&mut found);
                }
                std::thread::sleep(std::time::Duration::from_millis(800));
            });
        }
    });
    std::sync::Arc::try_unwrap(dependencies)
        .map_err(|_| anyhow!("Could not collect modpack dependency metadata"))?
        .into_inner()
        .map_err(|_| anyhow!("Could not read modpack dependency metadata"))
}

pub fn install_modpack_bytes(bytes: &[u8], root: &Path) -> Result<String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).context("This is not a valid Modrinth .mrpack file")?;
    let mut manifest_file = archive.by_name("modrinth.index.json").context("Modpack manifest is missing")?;
    let mut manifest_bytes = Vec::new();
    manifest_file.read_to_end(&mut manifest_bytes)?;
    drop(manifest_file);
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes).context("Modpack manifest is invalid")?;
    if !manifest.dependencies.contains_key("minecraft") { bail!("The modpack does not specify a Minecraft version"); }
    let version_id = safe_instance_component(&manifest.version_id)?;
    let slug: String = manifest.name.chars().take(64).map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '-' }).collect();
    let slug = slug.split('-').filter(|part| !part.is_empty()).collect::<Vec<_>>().join("-");
    if slug.is_empty() { bail!("The modpack name is invalid"); }
    let instance = root.join("modpacks").join(format!("{slug}-{version_id}")).join("instance");
    fs::create_dir_all(&instance)?;
    for prefix in ["overrides/", "client-overrides/"] {
        let mut overrides = ZipArchive::new(Cursor::new(bytes))?;
        for index in 0..overrides.len() {
            let mut entry = overrides.by_index(index)?;
            let entry_name = entry.name().to_owned();
            if let Some(relative) = entry_name.strip_prefix(prefix).filter(|relative| !relative.is_empty()) {
                let destination = instance.join(safe_relative(relative)?);
                if entry.is_dir() { fs::create_dir_all(destination)?; }
                else {
                    if let Some(parent) = destination.parent() { fs::create_dir_all(parent)?; }
                    let mut output = fs::File::create(destination)?;
                    std::io::copy(&mut entry, &mut output)?;
                }
            }
        }
    }
    let http = client()?;
    let mut recovered_dependencies = Vec::<VersionDependency>::new();
    for item in &manifest.files {
        if item.env.as_ref().and_then(|env| env.get("client")).is_some_and(|value| value == "unsupported") { continue; }
        let destination = instance.join(safe_relative(&item.path)?);
        if destination.exists() { continue; }
        let mut downloaded = None;
        for url in &item.downloads {
            for attempt in 0..3 {
                if let Ok(response) = http.get(url).send().and_then(reqwest::blocking::Response::error_for_status)
                    && let Ok(bytes) = response.bytes()
                {
                    let valid = item.hashes.get("sha1").is_none_or(|expected| {
                        format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(expected)
                    });
                    if valid && !bytes.is_empty() {
                        downloaded = Some(bytes);
                        break;
                    }
                }
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_millis(350 * (attempt + 1) as u64));
                }
            }
            if downloaded.is_some() { break; }
        }
        if downloaded.is_none()
            && let Some(hash) = item.hashes.get("sha1")
            && let Ok(response) = http.get(format!("{API}/version_file/{hash}?algorithm=sha1")).send()
            && let Ok(response) = response.error_for_status()
            && let Ok(version) = response.json::<serde_json::Value>()
        {
            if let Some(dependencies) = version.get("dependencies").cloned()
                && let Ok(dependencies) = serde_json::from_value::<Vec<VersionDependency>>(dependencies)
            {
                recovered_dependencies.extend(dependencies);
            }
            if let Some(files) = version.get("files").and_then(serde_json::Value::as_array) {
                for file in files {
                    let Some(url) = file.get("url").and_then(serde_json::Value::as_str) else { continue };
                    if let Ok(response) = http.get(url).send().and_then(reqwest::blocking::Response::error_for_status)
                        && let Ok(bytes) = response.bytes()
                        && !bytes.is_empty()
                        && format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(hash)
                    {
                        downloaded = Some(bytes);
                        break;
                    }
                }
            }
        }
        let bytes = downloaded.ok_or_else(|| anyhow!("Could not download or verify modpack file {} after retries and Modrinth hash recovery", item.path))?;
        if let Some(parent) = destination.parent() { fs::create_dir_all(parent)?; }
        fs::write(destination, bytes)?;
    }
    let mut dependencies = recovered_dependencies;
    dependencies.extend(discover_modpack_dependencies(&manifest.files, &instance)?);
    if !dependencies.is_empty() {
        let minecraft = manifest.dependencies.get("minecraft").context("Modpack Minecraft version is missing")?;
        let loader = manifest.dependencies.iter()
            .find(|(key, _)| key.as_str() != "minecraft")
            .map(|(key, _)| key.trim_end_matches("-loader").to_owned())
            .unwrap_or_else(|| "fabric".to_owned());
        install_required_mod_dependencies(&dependencies, &instance, minecraft, &loader)?;
    }
    let profile = serde_json::json!({
        "name": manifest.name,
        "version": manifest.version_id,
        "minecraft": manifest.dependencies.get("minecraft"),
        "loaders": manifest.dependencies.iter().filter(|(key, _)| key.as_str() != "minecraft").collect::<std::collections::BTreeMap<_, _>>(),
        "instance": instance
    });
    let profile_path = instance.parent().context("Modpack directory is invalid")?.join("lunar-modpack.json");
    fs::write(profile_path, serde_json::to_vec_pretty(&profile)?)?;
    Ok(instance.display().to_string())
}

fn install_required_mod_dependencies(dependencies: &[VersionDependency], instance: &Path, minecraft: &str, loader: &str) -> Result<Vec<String>> {
    let http = client()?;
    let mods_directory = instance.join("mods");
    let mut seen = std::collections::HashSet::new();
    let mut installed = Vec::new();
    for dependency in dependencies.iter().filter(|dependency| dependency.dependency_type.as_deref() == Some("required")) {
        let key = dependency.project_id.as_deref().or(dependency.version_id.as_deref()).unwrap_or_default();
        if key.is_empty() || !seen.insert(key.to_owned()) { continue; }
        let version = if let Some(version_id) = dependency.version_id.as_deref() {
            http.get(format!("{API}/version/{version_id}")).send()?.error_for_status()?.json::<ProjectVersion>()?
        } else if let Some(project_id) = dependency.project_id.as_deref() {
            let metadata = http.get(format!("{API}/project/{project_id}")).send()?.error_for_status()?.json::<serde_json::Value>()?;
            match metadata.get("project_type").and_then(serde_json::Value::as_str) {
                Some("mod") => {
                    let version = project_versions(project_id, Some(minecraft), Some(loader))?
                        .into_iter().find(|version| !version.files.is_empty())
                        .with_context(|| format!("Required mod dependency {project_id} has no compatible release for Minecraft {minecraft} and {loader}"))?;
                    let paths = download_mod_version(&version, &mods_directory, &mut seen, 0, minecraft, loader)
                        .with_context(|| format!("Could not install required mod dependency {project_id}"))?;
                    installed.extend(paths);
                    continue;
                }
                Some("resourcepack") => {
                    installed.push(save_project_file(project_id, instance, "resourcepacks", "required resource pack", None, None)?);
                    continue;
                }
                Some("shader") => {
                    installed.push(save_project_file(project_id, instance, "shaderpacks", "required shader pack", None, None)?);
                    continue;
                }
                Some(kind) => bail!("Required dependency {project_id} is a {kind}; Lunar cannot merge that project type into this modpack yet"),
                None => bail!("Modrinth did not identify the type of required dependency {project_id}"),
            }
        } else {
            continue;
        };
        if let Some(project_id) = version.project_id.as_deref() {
            let metadata = http.get(format!("{API}/project/{project_id}")).send()?.error_for_status()?.json::<serde_json::Value>()?;
            if metadata.get("project_type").and_then(serde_json::Value::as_str) != Some("mod") {
                bail!("Required version dependency {key} is not a Minecraft mod");
            }
        }
        installed.extend(download_mod_version(&version, &mods_directory, &mut seen, 0, minecraft, loader)
            .with_context(|| format!("Could not install required mod dependency {key}"))?);
    }
    Ok(installed)
}

pub fn resolve_instance_dependencies(instance: &Path, minecraft: &str, loader: &str) -> Result<Vec<String>> {
    let mods_directory = instance.join("mods");
    if !mods_directory.exists() { return Ok(Vec::new()); }
    let http = client()?;
    let mut dependencies = Vec::new();
    let mut installed_projects = std::collections::HashSet::new();
    let mut installed_versions = std::collections::HashSet::new();
    for entry in fs::read_dir(&mods_directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() { continue; }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()).is_none_or(|value| !value.eq_ignore_ascii_case("jar")) { continue; }
        let bytes = fs::read(&path)?;
        let hash = format!("{:x}", Sha1::digest(&bytes));
        let response = match http.get(format!("{API}/version_file/{hash}?algorithm=sha1")).send() {
            Ok(response) if response.status().is_success() => response,
            _ => continue,
        };
        let version = match response.json::<serde_json::Value>() { Ok(value) => value, Err(_) => continue };
        if let Some(project_id) = version.get("project_id").and_then(serde_json::Value::as_str) {
            installed_projects.insert(project_id.to_owned());
        }
        if let Some(version_id) = version.get("id").and_then(serde_json::Value::as_str) {
            installed_versions.insert(version_id.to_owned());
        }
        if let Some(values) = version.get("dependencies").cloned()
            && let Ok(mut found) = serde_json::from_value::<Vec<VersionDependency>>(values)
        {
            dependencies.append(&mut found);
        }
    }
    let missing = dependencies.into_iter()
        .filter(|dependency| dependency.dependency_type.as_deref() == Some("required"))
        .filter(|dependency| {
            if let Some(version_id) = dependency.version_id.as_deref() {
                !installed_versions.contains(version_id)
            } else if let Some(project_id) = dependency.project_id.as_deref() {
                !installed_projects.contains(project_id)
            } else {
                false
            }
        })
        .collect::<Vec<_>>();
    if missing.is_empty() { return Ok(Vec::new()); }
    install_required_mod_dependencies(&missing, instance, minecraft, loader)
        .context("Lunar found required dependencies but could not resolve all of them")
}

pub fn analyze_instance_compatibility(instance: &Path, minecraft: &str, loader: &str) -> Result<Vec<String>> {
    let mods_directory = instance.join("mods");
    if !mods_directory.exists() {
        return Ok(vec!["No mods folder exists in this instance".into()]);
    }
    let mut issues = Vec::new();
    let mut scanned = 0usize;
    let mut mod_ids = HashMap::<String, String>::new();
    let mut required_mods = Vec::<(String, String)>::new();
    let mut seen_hashes = HashSet::<String>::new();
    for entry in fs::read_dir(&mods_directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()).is_none_or(|value| !value.eq_ignore_ascii_case("jar")) {
            continue;
        }
        scanned += 1;
        let bytes = fs::read(&path)?;
        let filename = path.file_name().and_then(|value| value.to_str()).unwrap_or("Unknown mod").to_owned();
        let hash = format!("{:x}", Sha1::digest(&bytes));
        if !seen_hashes.insert(hash) {
            issues.push(format!("{filename}: duplicate JAR contents detected"));
            continue;
        }
        let mut archive = match ZipArchive::new(Cursor::new(bytes)) {
            Ok(archive) => archive,
            Err(_) => {
                issues.push(format!("{filename}: file is not a readable JAR archive"));
                continue;
            }
        };
        let fabric = archive.by_name("fabric.mod.json").ok().and_then(|mut file| {
            let mut text = String::new();
            file.read_to_string(&mut text).ok()?;
            serde_json::from_str::<serde_json::Value>(&text).ok()
        });
        let quilt = archive.by_name("quilt.mod.json").ok().and_then(|mut file| {
            let mut text = String::new();
            file.read_to_string(&mut text).ok()?;
            serde_json::from_str::<serde_json::Value>(&text).ok()
        });
        let forge = archive.file_names().any(|name| name.eq_ignore_ascii_case("META-INF/mods.toml"));
        let neoforge = archive.file_names().any(|name| name.eq_ignore_ascii_case("META-INF/neoforge.mods.toml"));
        let name = fabric.as_ref().and_then(|value| value.get("name")).and_then(serde_json::Value::as_str)
            .or_else(|| fabric.as_ref().and_then(|value| value.get("id")).and_then(serde_json::Value::as_str))
            .or_else(|| quilt.as_ref().and_then(|value| value.get("quilt_loader")).and_then(|value| value.get("id")).and_then(serde_json::Value::as_str))
            .unwrap_or(&filename).to_owned();
        let mod_id = fabric.as_ref().and_then(|value| value.get("id")).and_then(serde_json::Value::as_str)
            .or_else(|| quilt.as_ref().and_then(|value| value.get("quilt_loader")).and_then(|value| value.get("id")).and_then(serde_json::Value::as_str));
        if let Some(id) = mod_id
            && let Some(previous) = mod_ids.insert(id.to_owned(), filename.clone())
        {
            issues.push(format!("{name}: duplicate mod ID '{id}' also found in {previous}"));
        }
        if let Some(dependencies) = fabric.as_ref().and_then(|value| value.get("depends")).and_then(serde_json::Value::as_object) {
            for dependency in dependencies.keys() {
                if !matches!(dependency.as_str(), "minecraft" | "java" | "fabricloader") {
                    required_mods.push((name.clone(), dependency.clone()));
                }
            }
        }
        if let Some(dependencies) = quilt.as_ref().and_then(|value| value.get("quilt_loader")).and_then(|value| value.get("depends")).and_then(serde_json::Value::as_array) {
            for dependency in dependencies {
                if let Some(id) = dependency.get("id").and_then(serde_json::Value::as_str)
                    && !matches!(id, "minecraft" | "java" | "quilt_loader" | "fabricloader")
                {
                    required_mods.push((name.clone(), id.to_owned()));
                }
            }
        }
        let selected = loader.to_ascii_lowercase();
        if fabric.is_some() && selected != "fabric" {
            issues.push(format!("{name}: Fabric mod metadata found, but this instance uses {loader}"));
        }
        if quilt.is_some() && selected != "quilt" {
            issues.push(format!("{name}: Quilt mod metadata found, but this instance uses {loader}"));
        }
        if forge && selected != "forge" {
            issues.push(format!("{name}: Forge mod metadata found, but this instance uses {loader}"));
        }
        if neoforge && selected != "neoforge" {
            issues.push(format!("{name}: NeoForge mod metadata found, but this instance uses {loader}"));
        }
        if let Some(dependencies) = fabric.as_ref().and_then(|value| value.get("depends")).and_then(serde_json::Value::as_object)
            && let Some(required) = dependencies.get("minecraft")
        {
            let constraint = required.as_str().map(str::to_owned).unwrap_or_else(|| required.to_string());
            if !constraint.is_empty() && constraint != "*" && !constraint.contains(minecraft) {
                issues.push(format!("{name}: declares Minecraft constraint {constraint}; selected version is {minecraft} (review required)"));
            }
        }
    }
    for (mod_name, dependency) in required_mods {
        if !mod_ids.contains_key(&dependency) {
            issues.push(format!("{mod_name}: required mod dependency '{dependency}' was not found in this instance"));
        }
    }
    if scanned == 0 {
        return Ok(vec!["No JAR mods were found to inspect".into()]);
    }
    if issues.is_empty() {
        issues.push(format!("Scanned {scanned} JAR files. No obvious loader, duplicate, or required-dependency conflicts were found."));
    } else {
        issues.insert(0, format!("Scanned {scanned} JAR files for metadata conflicts:"));
    }
    Ok(issues)
}

pub fn install_modpack(project_id: &str, root: &Path) -> Result<String> {
    let http = client()?;
    let versions = http.get(format!("{API}/project/{project_id}/version")).send()?.error_for_status()?.json::<Vec<ProjectVersion>>()?;
    let version = versions.first().context("No modpack version was found")?;
    let dependencies = version.dependencies.clone();
    let file = version.files.iter().find(|file| file.primary).or_else(|| version.files.first()).context("The modpack version contains no file")?;
    let expected = file.hashes.as_ref().and_then(|hashes| hashes.get("sha1"));
    let mut downloaded = None;
    for attempt in 0..3 {
        if let Ok(response) = http.get(&file.url).send().and_then(reqwest::blocking::Response::error_for_status)
            && let Ok(bytes) = response.bytes()
            && !bytes.is_empty()
            && expected.is_none_or(|hash| format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(hash))
        {
            downloaded = Some(bytes);
            break;
        }
        if attempt < 2 {
            std::thread::sleep(std::time::Duration::from_millis(500 * (attempt + 1) as u64));
        }
    }
    let bytes = downloaded.context("Could not download or verify the modpack archive after three attempts")?;
    let instance_path = install_modpack_bytes(&bytes, root)?;
    if !dependencies.is_empty() {
        let instance = PathBuf::from(&instance_path);
        let profile_path = instance.parent().context("Modpack directory is invalid")?.join("lunar-modpack.json");
        let profile: serde_json::Value = serde_json::from_slice(&fs::read(profile_path)?)?;
        let minecraft = profile.get("minecraft").and_then(serde_json::Value::as_str).context("Modpack Minecraft version is missing")?;
        let loader = profile.get("loaders").and_then(serde_json::Value::as_object)
            .and_then(|loaders| loaders.iter().find(|(key, _)| key.as_str() != "minecraft"))
            .map(|(key, _)| key.trim_end_matches("-loader").to_owned())
            .unwrap_or_else(|| "fabric".to_owned());
        install_required_mod_dependencies(&dependencies, &instance, minecraft, &loader)?;
    }
    Ok(instance_path)
}


#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledPack {
    pub name: String,
    pub version: String,
    pub minecraft: String,
    pub loaders: std::collections::BTreeMap<String, String>,
    pub instance: PathBuf,
}

pub fn installed_modpacks(root: &Path) -> Result<Vec<InstalledPack>> {
    let mut packs = Vec::new();
    let directory = root.join("modpacks");
    if !directory.exists() { return Ok(packs); }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() { continue; }
        let instance = entry.path().join("instance");
        let profile_path = entry.path().join("lunar-modpack.json");
        if !profile_path.exists() { continue; }
        let value: ValuePack = serde_json::from_slice(&fs::read(profile_path)?)?;
        packs.push(InstalledPack {
            name: value.name,
            version: value.version,
            minecraft: value.minecraft,
            loaders: value.loaders,
            instance,
        });
    }
    packs.sort_by_key(|pack| pack.name.to_lowercase());
    Ok(packs)
}

#[derive(Deserialize)]
struct ValuePack {
    name: String,
    version: String,
    minecraft: String,
    #[serde(default)]
    loaders: std::collections::BTreeMap<String, String>,
}

fn download_mod_version(version: &ProjectVersion, directory: &Path, seen: &mut std::collections::HashSet<String>, depth: usize, minecraft: &str, loader: &str) -> Result<Vec<String>> {
    if depth > 8 { bail!("Mod dependency chain is unexpectedly deep"); }
    fs::create_dir_all(directory)?;
    let file = version.files.iter().find(|file| file.primary).or_else(|| version.files.first())
        .context("Mod version contains no downloadable file")?;
    let filename = safe_filename(&file.filename)?;
    if !filename.to_ascii_lowercase().ends_with(".jar") { bail!("Modrinth returned a non-JAR mod file"); }
    let mut paths = Vec::new();
    let destination = directory.join(filename);
    if !destination.exists() {
        let bytes = client()?.get(&file.url).send()?.error_for_status()?.bytes()?;
        if bytes.is_empty() { bail!("The downloaded mod file was empty"); }
        if let Some(expected) = file.hashes.as_ref().and_then(|hashes| hashes.get("sha1"))
            && !format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(expected)
        {
            bail!("Mod checksum verification failed");
        }
        let temporary = destination.with_extension("jar.lunar-download");
        fs::write(&temporary, &bytes)?;
        fs::rename(&temporary, &destination)?;
    }
    paths.push(destination.display().to_string());
    for dependency in version.dependencies.iter().filter(|dependency| dependency.dependency_type.as_deref() == Some("required")) {
        let key = dependency.project_id.as_deref().or(dependency.version_id.as_deref()).unwrap_or_default();
        if key.is_empty() || !seen.insert(key.to_owned()) { continue; }
        let dependency_version = if let Some(version_id) = dependency.version_id.as_deref() {
            client()?.get(format!("{API}/version/{version_id}")).send()?.error_for_status()?.json::<ProjectVersion>()?
        } else if let Some(project_id) = dependency.project_id.as_deref() {
            project_versions(project_id, Some(minecraft), Some(loader))?
                .into_iter().find(|candidate| !candidate.files.is_empty())
                .with_context(|| format!("A required dependency ({project_id}) has no compatible release for Minecraft {minecraft} and {loader}"))?
        } else {
            continue;
        };
        paths.extend(download_mod_version(&dependency_version, directory, seen, depth + 1, minecraft, loader)?);
    }
    Ok(paths)
}

pub fn install_mod(project_id: &str, root: &Path, minecraft: &str, loader: &str) -> Result<String> {
    let versions = project_versions(project_id, Some(minecraft), Some(loader))
        .with_context(|| format!("Could not retrieve mod versions for Minecraft {minecraft}"))?;
    let version = versions.iter().find(|version| !version.files.is_empty())
        .context("No mod version matches the selected Minecraft version and loader")?;
    let mut seen = std::collections::HashSet::new();
    seen.insert(project_id.to_owned());
    let paths = download_mod_version(version, &root.join("mods"), &mut seen, 0, minecraft, loader)?;
    Ok(paths.join("\n"))
}

pub fn list_files(root: &Path, kind: &str) -> Result<Vec<PathBuf>> {
    let directory = root.join(kind);
    if !directory.exists() { return Ok(Vec::new()); }
    let mut files = fs::read_dir(directory)?.filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path()).collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

pub fn set_file_enabled(path: &Path, enabled: bool) -> Result<PathBuf> {
    let parent = path.parent().context("Content path is invalid")?.canonicalize()?;
    let target = path.canonicalize()?;
    if !target.starts_with(&parent) || !target.is_file() {
        bail!("Selected content is not a regular file in its managed folder");
    }
    let name = target.file_name().and_then(|value| value.to_str()).context("Invalid content filename")?;
    let disabled = name.ends_with(".disabled");
    if enabled == !disabled {
        return Ok(target);
    }
    let new_name = if enabled { name.strip_suffix(".disabled").unwrap_or(name).to_owned() } else { format!("{name}.disabled") };
    let destination = parent.join(new_name);
    if destination.exists() { bail!("A file with the target name already exists"); }
    fs::rename(&target, &destination)?;
    Ok(destination)
}

pub fn remove_file(path: &Path, root: &Path, kind: &str) -> Result<()> {
    let directory = root.join(kind).canonicalize()?;
    let target = path.canonicalize()?;
    if !target.starts_with(&directory) || !target.is_file() { bail!("Selected content file is outside its managed directory"); }
    fs::remove_file(target)?;
    Ok(())
}

pub fn remove_modpack(pack: &InstalledPack, root: &Path) -> Result<()> {
    let directory = root.join("modpacks").canonicalize()?;
    let target = pack.instance.parent().context("Modpack instance path is invalid")?.canonicalize()?;
    if !target.starts_with(&directory) || !target.join("lunar-modpack.json").exists() { bail!("Selected folder is not a managed LUNAR modpack"); }
    fs::remove_dir_all(target)?;
    Ok(())
}

fn option_list(root: &Path, key: &str) -> Vec<String> {
    let text = fs::read_to_string(root.join("options.txt")).unwrap_or_default();
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name != key { return None; }
        serde_json::from_str::<Vec<String>>(value).ok()
    }).unwrap_or_default()
}

pub fn resource_pack_enabled(root: &Path, filename: &str) -> bool {
    option_list(root, "resourcePacks").contains(&format!("file/{filename}"))
}

pub fn set_resource_pack_enabled(root: &Path, filename: &str, enabled: bool) -> Result<()> {
    fs::create_dir_all(root)?;
    let path = root.join("options.txt");
    let source = fs::read_to_string(&path).unwrap_or_default();
    let token = format!("file/{filename}");
    let mut active = option_list(root, "resourcePacks");
    let inactive = option_list(root, "incompatibleResourcePacks");
    active.retain(|value| value != &token);
    if enabled { active.push(token); }
    let mut output = Vec::new();
    let mut active_written = false;
    let mut inactive_written = false;
    for line in source.lines() {
        if let Some((key, _)) = line.split_once(':') {
            if key == "resourcePacks" {
                output.push(format!("resourcePacks:{}", serde_json::to_string(&active)?));
                active_written = true;
                continue;
            }
            if key == "incompatibleResourcePacks" {
                output.push(format!("incompatibleResourcePacks:{}", serde_json::to_string(&inactive)?));
                inactive_written = true;
                continue;
            }
        }
        output.push(line.to_owned());
    }
    if !active_written { output.push(format!("resourcePacks:{}", serde_json::to_string(&active)?)); }
    if !inactive_written { output.push(format!("incompatibleResourcePacks:{}", serde_json::to_string(&inactive)?)); }
    fs::write(path, output.join("\n"))?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut output = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut output);
            for (name, bytes) in entries {
                writer.start_file(*name, SimpleFileOptions::default()).unwrap();
                writer.write_all(bytes).unwrap();
            }
            writer.finish().unwrap();
        }
        output.into_inner()
    }

    #[test]
    fn modpack_client_overrides_take_precedence_over_common_overrides() {
        let manifest = br#"{"name":"Override Test","versionId":"1","dependencies":{"minecraft":"1.20.1"},"files":[]}"#;
        let bytes = make_test_zip(&[
            ("modrinth.index.json", manifest),
            ("overrides/config/example.txt", b"common"),
            ("client-overrides/config/example.txt", b"client"),
        ]);
        let root = std::env::temp_dir().join(format!("lunar-mrpack-overrides-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let instance = PathBuf::from(install_modpack_bytes(&bytes, &root).unwrap());
        assert_eq!(fs::read_to_string(instance.join("config/example.txt")).unwrap(), "client");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resource_pack_archive_strips_wrapper_folder_even_with_macos_metadata() {
        let bytes = make_test_zip(&[
            ("ExamplePack/pack.mcmeta", br#"{"pack":{"pack_format":15,"description":"test"}}"#),
            ("ExamplePack/assets/minecraft/lang/en_us.json", b"{}"),
            ("__MACOSX/._ExamplePack", b"metadata"),
        ]);
        let normalized = normalize_pack_archive(&bytes, "resource pack").unwrap();
        let mut archive = ZipArchive::new(Cursor::new(normalized)).unwrap();
        assert!(archive.by_name("pack.mcmeta").is_ok());
        assert!(archive.by_name("assets/minecraft/lang/en_us.json").is_ok());
        assert!(archive.by_name("ExamplePack/pack.mcmeta").is_err());
    }

    #[test]
    fn shader_archive_strips_wrapper_folder() {
        let bytes = make_test_zip(&[
            ("BSL/shaders/program/gbuffers_basic.fsh", b"shader"),
            ("BSL/shaders/shaders.properties", b""),
        ]);
        let normalized = normalize_pack_archive(&bytes, "shader pack").unwrap();
        let mut archive = ZipArchive::new(Cursor::new(normalized)).unwrap();
        assert!(archive.by_name("shaders/program/gbuffers_basic.fsh").is_ok());
        assert!(archive.by_name("shaders/shaders.properties").is_ok());
    }

    #[test]
    fn safe_filename_rejects_paths() {
        assert!(safe_filename("valid-pack.zip").is_ok());
        assert!(safe_filename("../outside.zip").is_err());
        assert!(safe_filename("folder\\\\outside.zip").is_err());
    }

    #[test]
    fn resource_pack_toggle_does_not_mark_disabled_packs_incompatible() {
        let root = std::env::temp_dir().join(format!("lunar-resource-toggle-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        set_resource_pack_enabled(&root, "sample.zip", true).unwrap();
        let options = fs::read_to_string(root.join("options.txt")).unwrap();
        assert!(options.contains("resourcePacks:[\"file/sample.zip\"]"));
        assert!(options.contains("incompatibleResourcePacks:[]"));
        set_resource_pack_enabled(&root, "sample.zip", false).unwrap();
        let options = fs::read_to_string(root.join("options.txt")).unwrap();
        assert!(options.contains("resourcePacks:[]"));
        assert!(options.contains("incompatibleResourcePacks:[]"));
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn modpack_instance_identifier_rejects_path_traversal() {
        assert!(safe_instance_component("../outside").is_err());
        assert!(safe_instance_component("nested/name").is_err());
        assert!(safe_instance_component("C:outside").is_err());
        assert!(safe_instance_component("..").is_err());
        assert!(safe_instance_component(&"x".repeat(81)).is_err());
        assert_eq!(safe_instance_component("1.20.1+build_2").unwrap(), "1.20.1+build_2");
        assert_eq!(safe_instance_component("1.20.1-build_2").unwrap(), "1.20.1-build_2");
    }

    #[test]
    fn safe_relative_accepts_nested_normal_paths() {
        assert_eq!(safe_relative("overrides/config/example.json").unwrap(), PathBuf::from("overrides/config/example.json"));
    }

    #[test]
    fn safe_relative_rejects_parent_traversal() {
        assert!(safe_relative("../outside").is_err());
        assert!(safe_relative("a/../../outside").is_err());
    }

    #[test]
    fn safe_relative_rejects_absolute_and_empty_paths() {
        assert!(safe_relative("").is_err());
        assert!(safe_relative("/tmp/outside").is_err());
        assert!(safe_relative("C:\\outside").is_err());
    }

    #[test]
    fn safe_filename_rejects_empty_and_dot_names() {
        assert!(safe_filename("").is_err());
        assert!(safe_filename(".").is_err());
        assert!(safe_filename("..").is_err());
    }

    #[test]
    fn resource_pack_enable_is_idempotent() {
        let root = std::env::temp_dir().join(format!("lunar-resource-idempotent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        set_resource_pack_enabled(&root, "pack.zip", true).unwrap();
        set_resource_pack_enabled(&root, "pack.zip", true).unwrap();
        let options = fs::read_to_string(root.join("options.txt")).unwrap();
        assert_eq!(options.matches("file/pack.zip").count(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resource_pack_disable_preserves_other_enabled_packs() {
        let root = std::env::temp_dir().join(format!("lunar-resource-preserve-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        set_resource_pack_enabled(&root, "one.zip", true).unwrap();
        set_resource_pack_enabled(&root, "two.zip", true).unwrap();
        set_resource_pack_enabled(&root, "one.zip", false).unwrap();
        let options = fs::read_to_string(root.join("options.txt")).unwrap();
        assert!(!options.contains("file/one.zip"));
        assert!(options.contains("file/two.zip"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unknown_project_types_before_network_access() {
        assert!(search("", "unknown").is_err());
    }

    #[test]
    fn installed_project_status_tracks_installed_files() {
        let root = std::env::temp_dir().join(format!("lunar-installed-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let installed = root.join("mods").join("sample.jar");
        fs::create_dir_all(installed.parent().unwrap()).unwrap();
        fs::write(&installed, b"test").unwrap();
        mark_project_installed(&root, "mod", "project-123", installed.to_str().unwrap()).unwrap();
        assert!(project_is_installed(&root, "mod", "project-123"));
        fs::remove_file(&installed).unwrap();
        assert!(!project_is_installed(&root, "mod", "project-123"));
        let _ = fs::remove_dir_all(root);
    }

}
