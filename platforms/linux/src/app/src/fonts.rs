// Bundled brand fonts (Archivo, Instrument Serif; SIL OFL, the same files
// macOS ships in Resources/Fonts). Registered before GTK starts by pointing
// fontconfig at a generated config that includes the system one and adds the
// bundled directory, so no new crate is needed. Flatpak builds can instead
// install them to /app/share/fonts, which fontconfig already scans.

use std::path::{Path, PathBuf};

const FONT_FILES: [&str; 3] = [
    "Archivo-Variable.ttf",
    "InstrumentSerif-Regular.ttf",
    "InstrumentSerif-Italic.ttf",
];
const SYSTEM_CONFIGS: [&str; 1] = ["/etc/fonts/fonts.conf"];

fn has_fonts(dir: &Path) -> bool {
    FONT_FILES.iter().any(|file| dir.join(file).is_file())
}

/// `$FILEID_FONTS_DIR`, then `fonts/` beside the executable (the staged
/// dist), then `../share/fileid/fonts`, then the macOS resources in a
/// development checkout.
fn bundled_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::var_os("FILEID_FONTS_DIR").filter(|dir| !dir.is_empty()) {
        candidates.push(PathBuf::from(dir));
    }
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        candidates.push(exe_dir.join("fonts"));
        candidates.push(exe_dir.join("../share/fileid/fonts"));
    }
    if cfg!(debug_assertions) {
        candidates.push(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apple/app/Sources/FileID/Resources/Fonts"),
        );
    }
    candidates.into_iter().find(|dir| has_fonts(dir))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn config_xml(system_config: &Path, fonts_dir: &Path) -> String {
    format!(
        "<?xml version=\"1.0\"?>\n<!DOCTYPE fontconfig SYSTEM \"urn:fontconfig:fonts.dtd\">\n<fontconfig>\n  \
         <include ignore_missing=\"yes\">{}</include>\n  <dir>{}</dir>\n</fontconfig>\n",
        xml_escape(&system_config.to_string_lossy()),
        xml_escape(&fonts_dir.to_string_lossy()),
    )
}

/// Must run before GTK initializes (fontconfig reads FONTCONFIG_FILE once).
/// Leaves fonts untouched when the system config cannot be found, so a
/// missing bundle never costs the user their system fonts.
pub fn register() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let Some(fonts_dir) = bundled_dir().and_then(|dir| dir.canonicalize().ok()) else {
        tracing::debug!("brand fonts not bundled; using system fallbacks");
        return;
    };
    let system_config = std::env::var_os("FONTCONFIG_FILE")
        .map(PathBuf::from)
        .or_else(|| SYSTEM_CONFIGS.iter().map(PathBuf::from).find(|path| path.is_file()));
    let Some(system_config) = system_config else { return };
    let config_dir = gtk::glib::user_cache_dir().join("fileid");
    let config_path = config_dir.join("fonts.conf");
    let written = std::fs::create_dir_all(&config_dir)
        .and_then(|()| std::fs::write(&config_path, config_xml(&system_config, &fonts_dir)));
    match written {
        Ok(()) => std::env::set_var("FONTCONFIG_FILE", &config_path),
        Err(error) => tracing::warn!(%error, "could not register the bundled fonts"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_includes_the_system_fonts_and_adds_the_bundle() {
        let xml = config_xml(Path::new("/etc/fonts/fonts.conf"), Path::new("/opt/File & ID/fonts"));
        assert!(xml.contains("<include ignore_missing=\"yes\">/etc/fonts/fonts.conf</include>"));
        assert!(xml.contains("<dir>/opt/File &amp; ID/fonts</dir>"));
    }

    #[test]
    fn the_development_checkout_finds_the_bundled_fonts() {
        let dir = bundled_dir().expect("debug builds find the macOS font resources");
        assert!(FONT_FILES.iter().all(|file| dir.join(file).is_file()));
    }
}
