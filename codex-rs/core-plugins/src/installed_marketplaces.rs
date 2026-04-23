use std::path::Path;
use std::path::PathBuf;

use codex_config::ConfigLayerStack;
use codex_plugin::validate_plugin_segment;
use codex_utils_absolute_path::AbsolutePathBuf;
use tracing::warn;

use crate::marketplace::find_marketplace_manifest_path;

pub const INSTALLED_MARKETPLACES_DIR: &str = ".tmp/marketplaces";

pub fn marketplace_install_root(codex_home: &Path) -> PathBuf {
    codex_home.join(INSTALLED_MARKETPLACES_DIR)
}

pub fn installed_marketplace_roots_from_layer_stack(
    config_layer_stack: &ConfigLayerStack,
    codex_home: &Path,
) -> Vec<AbsolutePathBuf> {
    let Some(user_layer) = config_layer_stack.get_user_layer() else {
        return Vec::new();
    };
    let Some(marketplaces_value) = user_layer.config.get("marketplaces") else {
        return Vec::new();
    };
    let Some(marketplaces) = marketplaces_value.as_table() else {
        warn!("invalid marketplaces config: expected table");
        return Vec::new();
    };
    let default_install_root = marketplace_install_root(codex_home);
    let mut roots = marketplaces
        .iter()
        .filter_map(|(marketplace_name, marketplace)| {
            if !marketplace.is_table() {
                warn!(
                    marketplace_name,
                    "ignoring invalid configured marketplace entry"
                );
                return None;
            }
            if let Err(err) = validate_plugin_segment(marketplace_name, "marketplace name") {
                warn!(
                    marketplace_name,
                    error = %err,
                    "ignoring invalid configured marketplace name"
                );
                return None;
            }
            let path = resolve_configured_marketplace_root(
                marketplace_name,
                marketplace,
                &default_install_root,
            )?;
            find_marketplace_manifest_path(&path).map(|_| path)
        })
        .filter_map(|path| AbsolutePathBuf::try_from(path).ok())
        .collect::<Vec<_>>();
    roots.sort_unstable_by(|left, right| left.as_path().cmp(right.as_path()));
    roots
}

pub fn bundled_marketplace_roots_from_current_exe(
    current_exe: Option<&Path>,
) -> Vec<AbsolutePathBuf> {
    let Some(bundled_plugins_root) = bundled_plugins_root_from_current_exe(current_exe) else {
        return Vec::new();
    };

    let Ok(entries) = bundled_plugins_root.read_dir() else {
        return Vec::new();
    };

    let mut roots = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| find_marketplace_manifest_path(&path).map(|_| path))
        .filter_map(|path| AbsolutePathBuf::try_from(path).ok())
        .collect::<Vec<_>>();
    roots.sort_unstable();
    roots
}

fn bundled_plugins_root_from_current_exe(current_exe: Option<&Path>) -> Option<PathBuf> {
    let exe_dir = current_exe?.parent()?;
    let mut candidate_resource_dirs = Vec::new();

    if exe_dir.file_name().is_some_and(|name| name == "Resources") {
        candidate_resource_dirs.push(exe_dir.to_path_buf());
    }

    if exe_dir.file_name().is_some_and(|name| name == "MacOS")
        && let Some(contents_dir) = exe_dir.parent()
    {
        candidate_resource_dirs.push(contents_dir.join("Resources"));
    }

    candidate_resource_dirs.push(exe_dir.join("codex-resources"));

    candidate_resource_dirs
        .into_iter()
        .find(|resource_dir| resource_dir.is_dir())
        .map(|resource_dir| resource_dir.join("plugins"))
}

pub fn resolve_configured_marketplace_root(
    marketplace_name: &str,
    marketplace: &toml::Value,
    default_install_root: &Path,
) -> Option<PathBuf> {
    match marketplace.get("source_type").and_then(toml::Value::as_str) {
        Some("local") => marketplace
            .get("source")
            .and_then(toml::Value::as_str)
            .filter(|source| !source.is_empty())
            .map(PathBuf::from),
        _ => Some(default_install_root.join(marketplace_name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::fs;

    #[test]
    fn bundled_marketplace_roots_are_discovered_for_resources_bundled_cli() {
        let tempdir = tempfile::tempdir().unwrap();
        let resources_dir = tempdir.path().join("CodexCustom.app/Contents/Resources");
        let marketplace_root = resources_dir.join("plugins/openai-bundled");
        let manifest_dir = marketplace_root.join(".agents/plugins");
        let plugin_dir = marketplace_root.join("plugins/computer-use/.codex-plugin");
        fs::create_dir_all(&manifest_dir).unwrap();
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            manifest_dir.join("marketplace.json"),
            r#"{"name":"openai-bundled","plugins":[]}"#,
        )
        .unwrap();
        let exe_path = resources_dir.join("codex");
        fs::write(&exe_path, "").unwrap();

        let roots = bundled_marketplace_roots_from_current_exe(Some(&exe_path));

        assert_eq!(
            roots,
            vec![AbsolutePathBuf::try_from(marketplace_root).unwrap()]
        );
    }

    #[test]
    fn bundled_marketplace_roots_are_discovered_for_macos_app_launcher() {
        let tempdir = tempfile::tempdir().unwrap();
        let contents_dir = tempdir.path().join("CodexCustom.app/Contents");
        let resources_dir = contents_dir.join("Resources");
        let marketplace_root = resources_dir.join("plugins/openai-bundled");
        let manifest_dir = marketplace_root.join(".agents/plugins");
        fs::create_dir_all(&manifest_dir).unwrap();
        fs::write(
            manifest_dir.join("marketplace.json"),
            r#"{"name":"openai-bundled","plugins":[]}"#,
        )
        .unwrap();
        let macos_dir = contents_dir.join("MacOS");
        fs::create_dir_all(&macos_dir).unwrap();
        let exe_path = macos_dir.join("Codex");
        fs::write(&exe_path, "").unwrap();

        let roots = bundled_marketplace_roots_from_current_exe(Some(&exe_path));

        assert_eq!(
            roots,
            vec![AbsolutePathBuf::try_from(marketplace_root).unwrap()]
        );
    }
}
