use crate::model::Installed;
use std::{collections::HashSet, fs, path::PathBuf};

pub fn scan() -> Vec<Installed> {
    let mut roots = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
    ];
    if let Some(data) = dirs::data_dir() {
        roots.push(data.join("applications"));
    }
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    let icon_index = IconIndex::build(&icon_roots());
    for root in roots.into_iter().filter(|p| p.is_dir()) {
        for item in walkdir::WalkDir::new(root)
            .max_depth(3)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = item.path();
            if path.extension().and_then(|s| s.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(text) = fs::read_to_string(path) else {
                continue;
            };
            let mut in_entry = false;
            let mut name = None;
            let mut exec = None;
            let mut icon = String::new();
            let mut hidden = false;
            for line in text.lines().map(str::trim) {
                if line.starts_with('[') {
                    in_entry = line == "[Desktop Entry]";
                    continue;
                }
                if !in_entry || line.starts_with('#') {
                    continue;
                }
                if let Some(v) = line.strip_prefix("Name=") {
                    name.get_or_insert_with(|| v.to_owned());
                }
                if let Some(v) = line.strip_prefix("Exec=") {
                    exec.get_or_insert_with(|| strip_field_codes(v));
                }
                if let Some(v) = line.strip_prefix("Icon=") {
                    icon = v.to_owned();
                }
                if line == "NoDisplay=true" || line == "Hidden=true" {
                    hidden = true;
                }
                if line == "Terminal=true" {
                    hidden = true;
                }
            }
            let (Some(name), Some(exec)) = (name, exec) else {
                continue;
            };
            if hidden || exec.is_empty() || !seen.insert(name.to_lowercase()) {
                continue;
            }
            let (icon_path, icon) = resolve_icon(&icon_index, &icon);
            found.push(Installed {
                name,
                path: exec,
                icon,
                icon_path,
            });
        }
    }
    found.sort_by_key(|a| a.name.to_lowercase());
    found
}

fn strip_field_codes(exec: &str) -> String {
    exec.split_whitespace()
        .filter(|arg| !arg.starts_with('%'))
        .collect::<Vec<_>>()
        .join(" ")
}

/// System icon roots searched for bare `Icon=` theme names, in order.
/// hicolor first (the mandatory fallback theme), then common desktop themes,
/// then any other installed theme directory.
fn icon_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/usr/share/icons/hicolor"),
        PathBuf::from("/usr/share/icons/gnome"),
        PathBuf::from("/usr/share/icons/Adwaita"),
        PathBuf::from("/usr/share/icons/breeze"),
        PathBuf::from("/usr/share/icons/breeze-dark"),
        PathBuf::from("/usr/share/icons/Papirus"),
        PathBuf::from("/usr/share/icons/Papirus-Dark"),
        PathBuf::from("/usr/share/pixmaps"),
    ];
    roots.extend(discover_theme_roots(
        &PathBuf::from("/usr/share/icons"),
        &roots,
    ));
    if let Some(data) = dirs::data_dir() {
        let user_icons = data.join("icons");
        roots.push(user_icons.join("hicolor"));
        roots.push(user_icons.join("Papirus"));
        roots.extend(discover_theme_roots(&user_icons, &roots));
    }
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".icons"));
    }
    roots
}

/// Find theme directories not already listed: any child dir containing an
/// `index.theme`, an `apps` subdir, or `<size>/apps` layout.
fn discover_theme_roots(icons_dir: &PathBuf, known: &[PathBuf]) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(icons_dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() || known.contains(&path) || found.contains(&path) {
            continue;
        }
        let looks_like_theme = path.join("index.theme").is_file()
            || path.join("apps").is_dir()
            || fs::read_dir(&path)
                .map(|mut entries| {
                    entries.any(|entry| {
                        entry
                            .map(|entry| entry.path().join("apps").is_dir())
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
        if looks_like_theme {
            found.push(path);
        }
    }
    found.sort();
    found
}

/// Parse a theme size directory (`48x48`, `48`, `48@2x`) to pixel width.
fn parse_theme_size(name: &str) -> u32 {
    name.chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse::<u32>()
        .unwrap_or(0)
}

/// Resolve a `.desktop` `Icon=` value to an image file.
/// Absolute paths are used directly; bare theme names are looked up in the
/// system icon roots, preferring raster PNGs over scalable SVGs (XPM is
/// skipped: webviews cannot render it).
fn find_icon_file(icon: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let icon = icon.trim();
    if icon.is_empty() {
        return None;
    }
    let direct = PathBuf::from(icon);
    if direct.is_absolute() {
        return loadable_image(&direct).then(|| direct);
    }
    IconIndex::build(roots).lookup(icon_stem(icon))
}

/// Strip a real image suffix (`firefox.png` → `firefox`) while preserving
/// dots in theme names (`org.xfce.about`).
fn icon_stem(icon: &str) -> &str {
    match icon.rsplit_once('.') {
        Some((stem, ext))
            if ext.eq_ignore_ascii_case("png")
                || ext.eq_ignore_ascii_case("svg")
                || ext.eq_ignore_ascii_case("xpm") =>
        {
            stem
        }
        _ => icon,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IconCandidate {
    size: u32,
    svg: bool,
    order: usize,
    path: PathBuf,
}

/// Index of loadable theme icons, built once per scan: file stem →
/// candidates across all contexts (`apps`, `devices`, …) and both theme
/// layouts (`<size>/<context>`, `<context>/<size>`).
#[derive(Default)]
struct IconIndex {
    entries: std::collections::HashMap<String, Vec<IconCandidate>>,
}

impl IconIndex {
    fn build(roots: &[PathBuf]) -> Self {
        let mut index = Self::default();
        for (order, root) in roots.iter().enumerate() {
            if root.file_name().and_then(|n| n.to_str()) == Some("pixmaps") {
                index.insert_dir_files(root, 0, order);
            } else {
                index.insert_theme_root(root, order);
            }
        }
        index
    }

    fn insert_dir_files(&mut self, dir: &PathBuf, size: u32, order: usize) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let Some(name) = path.file_stem().and_then(|n| n.to_str()) else {
                continue;
            };
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ext != "png" && ext != "svg" {
                continue;
            }
            if fs::metadata(&path)
                .map(|m| m.len() > 1024 * 1024)
                .unwrap_or(true)
            {
                continue;
            }
            self.entries
                .entry(name.to_owned())
                .or_default()
                .push(IconCandidate {
                    size,
                    svg: ext == "svg",
                    order,
                    path,
                });
        }
    }

    fn insert_theme_root(&mut self, root: &PathBuf, order: usize) {
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "apps" {
                // Breeze layout: `<root>/apps/<size>`.
                let Ok(sizes) = fs::read_dir(&path) else {
                    continue;
                };
                for size_entry in sizes.filter_map(Result::ok) {
                    if !size_entry.path().is_dir() {
                        continue;
                    }
                    let size = parse_theme_size(&size_entry.file_name().to_string_lossy());
                    self.insert_dir_files(&size_entry.path(), size, order);
                }
            } else {
                // hicolor layout: `<root>/<size>/<context>`.
                let size = parse_theme_size(&name);
                if size == 0 && name != "scalable" {
                    continue;
                }
                let Ok(contexts) = fs::read_dir(&path) else {
                    continue;
                };
                for context in contexts.filter_map(Result::ok) {
                    if context.path().is_dir() {
                        self.insert_dir_files(&context.path(), size, order);
                    }
                }
            }
        }
    }

    fn lookup(&self, stem: &str) -> Option<PathBuf> {
        if stem.is_empty() {
            return None;
        }
        if let Some(path) = self.best_match(stem) {
            return Some(path);
        }
        // Fallback for suffixed variants (`quickgui` → `quickgui_512`).
        let mut keys: Vec<&String> = self
            .entries
            .keys()
            .filter(|key| {
                key.len() > stem.len()
                    && key.starts_with(stem)
                    && matches!(key.as_bytes().get(stem.len()), Some(b'_') | Some(b'-'))
            })
            .collect();
        keys.sort_by_key(|key| key.len());
        let mut merged: Vec<&IconCandidate> = keys
            .iter()
            .flat_map(|key| self.entries.get(*key).into_iter().flatten())
            .collect();
        merged.sort_by_key(|c| (c.svg as u8, size32(c.size), c.order));
        merged.into_iter().next().map(|c| c.path.clone())
    }

    fn best_match(&self, stem: &str) -> Option<PathBuf> {
        let candidates = self.entries.get(stem)?;
        let mut pngs: Vec<&IconCandidate> = candidates.iter().filter(|c| !c.svg).collect();
        pngs.sort_by_key(|c| (size32(c.size), c.order));
        if let Some(hit) = pngs.into_iter().next() {
            return Some(hit.path.clone());
        }
        let mut svgs: Vec<&IconCandidate> = candidates.iter().filter(|c| c.svg).collect();
        svgs.sort_by_key(|c| c.order);
        svgs.into_iter().next().map(|c| c.path.clone())
    }
}

fn resolve_icon(index: &IconIndex, icon: &str) -> (String, String) {
    let icon = icon.trim();
    if icon.is_empty() {
        return (String::new(), String::new());
    }
    let direct = PathBuf::from(icon);
    if direct.is_absolute() {
        if loadable_image(&direct) {
            let path = direct.to_string_lossy().into_owned();
            let data = encode_icon(&direct).unwrap_or_default();
            return (path, data);
        }
        return (String::new(), String::new());
    }
    let Some(path) = index.lookup(icon_stem(icon)) else {
        return (String::new(), String::new());
    };
    let data = encode_icon(&path).unwrap_or_default();
    (path.to_string_lossy().into_owned(), data)
}

fn size32(size: u32) -> u32 {
    // Prefer sizes near 48px; 0 (unparseable/scalable dir) sorts last.
    if size == 0 {
        u32::MAX
    } else {
        size.abs_diff(48)
    }
}

fn loadable_image(path: &PathBuf) -> bool {
    if !path.is_file() {
        return false;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext != "png" && ext != "svg" {
        return false;
    }
    // Skip oversized files; icons are small by nature.
    fs::metadata(path)
        .map(|m| m.len() <= 1024 * 1024)
        .unwrap_or(false)
}

fn mime_for(path: &PathBuf) -> &'static str {
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
    {
        "image/svg+xml"
    } else {
        "image/png"
    }
}

/// Encode an icon file as a `data:` URL, like the native scanner.
pub fn encode_icon(path: &PathBuf) -> Option<String> {
    use base64::Engine;
    let bytes = fs::read(path).ok()?;
    if bytes.is_empty() || bytes.len() > 1024 * 1024 {
        return None;
    }
    Some(format!(
        "data:{};base64,{}",
        mime_for(path),
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_roots(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "assist-icon-test-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn theme_names_resolve_to_preferred_png() {
        let root = temp_roots("theme");
        let hicolor = root.join("hicolor");
        for size in ["16x16", "48x48", "256x256"] {
            let dir = hicolor.join(size).join("apps");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("demo.png"), vec![1u8; 64]).unwrap();
        }
        // SVG fallback when no PNG exists.
        let svg_dir = hicolor.join("scalable").join("apps");
        std::fs::create_dir_all(&svg_dir).unwrap();
        std::fs::write(svg_dir.join("vector.svg"), b"<svg/>").unwrap();

        // Dotted theme names keep their dots; only image suffixes are stripped.
        let dotted = hicolor.join("48x48").join("apps");
        std::fs::write(dotted.join("org.example.app.png"), vec![2u8; 64]).unwrap();
        let found_dotted = find_icon_file("org.example.app", &[hicolor.clone()]).unwrap();
        assert!(
            found_dotted.ends_with("48x48/apps/org.example.app.png"),
            "{found_dotted:?}"
        );
        let found_ext = find_icon_file("demo.png", &[hicolor.clone()]).unwrap();
        assert!(found_ext.ends_with("48x48/apps/demo.png"), "{found_ext:?}");

        let found = find_icon_file("demo", &[hicolor.clone()]).unwrap();
        assert!(found.ends_with("48x48/apps/demo.png"), "{found:?}");
        let found_svg = find_icon_file("vector", &[hicolor]).unwrap();
        assert!(
            found_svg.ends_with("scalable/apps/vector.svg"),
            "{found_svg:?}"
        );
        // Breeze-style inverted layout (`apps/<size>`) resolves too.
        let breeze = root.join("breeze");
        std::fs::create_dir_all(breeze.join("apps").join("48")).unwrap();
        std::fs::write(breeze.join("apps").join("48").join("gear.svg"), b"<svg/>").unwrap();
        let found_breeze = find_icon_file("gear", &[breeze]).unwrap();
        assert!(
            found_breeze.ends_with("apps/48/gear.svg"),
            "{found_breeze:?}"
        );
        assert_eq!(parse_theme_size("48x48"), 48);
        assert_eq!(parse_theme_size("48"), 48);
        assert_eq!(parse_theme_size("16@2x"), 16);
        assert_eq!(parse_theme_size("scalable"), 0);
        assert!(find_icon_file("missing", &[root.clone()]).is_none());
        assert!(find_icon_file("", &[root.clone()]).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn icons_encode_as_data_urls() {
        let root = temp_roots("encode");
        let png = root.join("a.png");
        std::fs::write(&png, [137u8, 80, 78, 71]).unwrap();
        let url = encode_icon(&png).unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
        let svg = root.join("b.svg");
        std::fs::write(&svg, b"<svg/>").unwrap();
        assert!(encode_icon(&svg)
            .unwrap()
            .starts_with("data:image/svg+xml;base64,"));
        // Absolute paths are used directly; XPM is skipped.
        assert!(find_icon_file(png.to_str().unwrap(), &[]).is_some());
        let xpm = root.join("c.xpm");
        std::fs::write(&xpm, b"/* XPM */").unwrap();
        assert!(find_icon_file(xpm.to_str().unwrap(), &[]).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
