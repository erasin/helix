pub mod config;
pub mod grammar;

use anyhow::{anyhow, Context, Result};
use etcetera::base_strategy::{choose_base_strategy, BaseStrategy};
use std::path::{Path, PathBuf};
use toml::Value;

pub const VERSION_AND_GIT_HASH: &str = env!("VERSION_AND_GIT_HASH");

pub static RUNTIME_DIR: once_cell::sync::Lazy<PathBuf> = once_cell::sync::Lazy::new(runtime_dir);

static CONFIG_FILE: once_cell::sync::OnceCell<PathBuf> = once_cell::sync::OnceCell::new();

pub fn initialize_config_file(specified_file: Option<PathBuf>) {
    let config_file = specified_file.unwrap_or_else(|| {
        let config_dir = config_dir();

        if !config_dir.exists() {
            std::fs::create_dir_all(&config_dir).ok();
        }

        config_dir.join("config.toml")
    });

    // We should only initialize this value once.
    CONFIG_FILE.set(config_file).ok();
}

pub fn runtime_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("HELIX_RUNTIME") {
        return dir.into();
    }

    if let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") {
        // this is the directory of the crate being run by cargo, we need the workspace path so we take the parent
        let path = std::path::PathBuf::from(dir).parent().unwrap().join(RT_DIR);
        log::debug!("runtime dir: {}", path.to_string_lossy());
        return path;
    }

    const RT_DIR: &str = "runtime";
    let conf_dir = config_dir().join(RT_DIR);
    if conf_dir.exists() {
        return conf_dir;
    }

    // fallback to location of the executable being run
    // canonicalize the path in case the executable is symlinked
    std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::canonicalize(path).ok())
        .and_then(|path| path.parent().map(|path| path.to_path_buf().join(RT_DIR)))
        .unwrap()
}

pub fn config_dir() -> PathBuf {
    // TODO: allow env var override
    let strategy = choose_base_strategy().expect("Unable to find the config directory!");
    let mut path = strategy.config_dir();
    path.push("helix");
    path
}

pub fn local_config_dirs() -> Vec<PathBuf> {
    let directories = find_local_config_dirs()
        .into_iter()
        .map(|path| path.join(".helix"))
        .collect();
    log::debug!("Located configuration folders: {:?}", directories);
    directories
}

pub fn cache_dir() -> PathBuf {
    // TODO: allow env var override
    let strategy = choose_base_strategy().expect("Unable to find the config directory!");
    let mut path = strategy.cache_dir();
    path.push("helix");
    path
}

pub fn config_file() -> PathBuf {
    CONFIG_FILE
        .get()
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| config_dir().join("config.toml"))
}

pub fn lang_config_file() -> PathBuf {
    config_dir().join("languages.toml")
}

pub fn log_file() -> PathBuf {
    cache_dir().join("helix.log")
}

pub fn icons_config_file() -> std::path::PathBuf {
    config_dir().join("icons.toml")
}

pub fn find_local_config_dirs() -> Vec<PathBuf> {
    let current_dir = std::env::current_dir().expect("unable to determine current directory");
    let mut directories = Vec::new();

    for ancestor in current_dir.ancestors() {
        if ancestor.join(".git").exists() {
            directories.push(ancestor.to_path_buf());
            // Don't go higher than repo if we're in one
            break;
        } else if ancestor.join(".helix").is_dir() {
            directories.push(ancestor.to_path_buf());
        }
    }
    directories
}

/// Merge two TOML documents, merging values from `right` onto `left`
///
/// When an array exists in both `left` and `right`, `right`'s array is
/// used. When a table exists in both `left` and `right`, the merged table
/// consists of all keys in `left`'s table unioned with all keys in `right`
/// with the values of `right` being merged recursively onto values of
/// `left`.
///
/// `merge_toplevel_arrays` controls whether a top-level array in the TOML
/// document is merged instead of overridden. This is useful for TOML
/// documents that use a top-level array of values like the `languages.toml`,
/// where one usually wants to override or add to the array instead of
/// replacing it altogether.
pub fn merge_toml_values(left: toml::Value, right: toml::Value, merge_depth: usize) -> toml::Value {
    fn get_name(v: &Value) -> Option<&str> {
        v.get("name").and_then(Value::as_str)
    }

    match (left, right) {
        (Value::Array(mut left_items), Value::Array(right_items)) => {
            // The top-level arrays should be merged but nested arrays should
            // act as overrides. For the `languages.toml` config, this means
            // that you can specify a sub-set of languages in an overriding
            // `languages.toml` but that nested arrays like Language Server
            // arguments are replaced instead of merged.
            if merge_depth > 0 {
                left_items.reserve(right_items.len());
                for rvalue in right_items {
                    let lvalue = get_name(&rvalue)
                        .and_then(|rname| {
                            left_items.iter().position(|v| get_name(v) == Some(rname))
                        })
                        .map(|lpos| left_items.remove(lpos));
                    let mvalue = match lvalue {
                        Some(lvalue) => merge_toml_values(lvalue, rvalue, merge_depth - 1),
                        None => rvalue,
                    };
                    left_items.push(mvalue);
                }
                Value::Array(left_items)
            } else {
                Value::Array(right_items)
            }
        }
        (Value::Table(mut left_map), Value::Table(right_map)) => {
            if merge_depth > 0 {
                for (rname, rvalue) in right_map {
                    match left_map.remove(&rname) {
                        Some(lvalue) => {
                            let merged_value = merge_toml_values(lvalue, rvalue, merge_depth - 1);
                            left_map.insert(rname, merged_value);
                        }
                        None => {
                            left_map.insert(rname, rvalue);
                        }
                    }
                }
                Value::Table(left_map)
            } else {
                Value::Table(right_map)
            }
        }
        // Catch everything else we didn't handle, and use the right value
        (_, value) => value,
    }
}

/// This trait allows theme and icon flavors to be loaded from TOML files, with inheritance
pub trait FlavorLoader<T> {
    fn user_dir(&self) -> &Path;
    fn default_dir(&self) -> &Path;
    fn log_type_display(&self) -> String;

    // Returns the path to the flavor with the name
    // With `only_default_dir` as false the path will first search for the user path
    // disabled it ignores the user path and returns only the default path
    fn path(&self, name: &str, only_default_dir: bool) -> PathBuf {
        let filename = format!("{}.toml", name);

        let user_path = self.user_dir().join(&filename);
        if !only_default_dir && user_path.exists() {
            user_path
        } else {
            self.default_dir().join(filename)
        }
    }

    /// Loads the flavor data as `toml::Value` first from the `user_dir` then in `default_dir`
    fn load_toml(&self, path: PathBuf) -> Result<Value> {
        let data = std::fs::read_to_string(&path)?;

        toml::from_str(&data).context("Failed to deserialize flavor")
    }

    /// Merge one theme into the parent theme
    fn merge_flavors(&self, parent_flavor_toml: Value, flavor_toml: Value) -> Value;

    /// Load the flavor and its parent recursively and merge them.
    /// `base_flavor_name` is the flavor from the config.toml, used to prevent some circular loading scenarios.
    fn load_flavor(
        &self,
        name: &str,
        base_flavor_name: &str,
        only_default_dir: bool,
    ) -> Result<Value> {
        let path = self.path(name, only_default_dir);
        let flavor_toml = self.load_toml(path)?;

        let inherits = flavor_toml.get("inherits");

        let flavor_toml = if let Some(parent_flavor_name) = inherits {
            let parent_flavor_name = parent_flavor_name.as_str().ok_or_else(|| {
                anyhow!(
                    "{}: expected 'inherits' to be a string: {}",
                    self.log_type_display(),
                    parent_flavor_name
                )
            })?;

            let parent_flavor_toml = match self.default_data(parent_flavor_name) {
                Some(p) => p,
                None => self.load_flavor(
                    parent_flavor_name,
                    base_flavor_name,
                    base_flavor_name == parent_flavor_name,
                )?,
            };

            self.merge_flavors(parent_flavor_toml, flavor_toml)
        } else {
            flavor_toml
        };

        Ok(flavor_toml)
    }

    /// Lists all flavor names available in default and user directory
    fn names(&self) -> Vec<String> {
        let mut names = toml_names_in_dir(self.user_dir());
        names.extend(toml_names_in_dir(self.default_dir()));
        names
    }

    /// Get the data for the defaults
    fn default_data(&self, name: &str) -> Option<Value>;
}

/// Get the names of the TOML documents within a directory
pub fn toml_names_in_dir(path: &Path) -> Vec<String> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    entries
        .filter_map(|entry| {
            entry
                .ok()?
                .file_name()
                .to_str()?
                .strip_suffix(".toml")
                .filter(|name| !name.is_empty())
                .map(ToString::to_string)
        })
        .collect()
}

#[cfg(test)]
mod merge_toml_tests {
    use std::str;

    use super::merge_toml_values;
    use toml::Value;

    #[test]
    fn language_toml_map_merges() {
        const USER: &str = r#"
        [[language]]
        name = "nix"
        test = "bbb"
        indent = { tab-width = 4, unit = "    ", test = "aaa" }
        "#;

        let base = include_bytes!("../../languages.toml");
        let base = str::from_utf8(base).expect("Couldn't parse built-in languages config");
        let base: Value = toml::from_str(base).expect("Couldn't parse built-in languages config");
        let user: Value = toml::from_str(USER).unwrap();

        let merged = merge_toml_values(base, user, 3);
        let languages = merged.get("language").unwrap().as_array().unwrap();
        let nix = languages
            .iter()
            .find(|v| v.get("name").unwrap().as_str().unwrap() == "nix")
            .unwrap();
        let nix_indent = nix.get("indent").unwrap();

        // We changed tab-width and unit in indent so check them if they are the new values
        assert_eq!(
            nix_indent.get("tab-width").unwrap().as_integer().unwrap(),
            4
        );
        assert_eq!(nix_indent.get("unit").unwrap().as_str().unwrap(), "    ");
        // We added a new keys, so check them
        assert_eq!(nix.get("test").unwrap().as_str().unwrap(), "bbb");
        assert_eq!(nix_indent.get("test").unwrap().as_str().unwrap(), "aaa");
        // We didn't change comment-token so it should be same
        assert_eq!(nix.get("comment-token").unwrap().as_str().unwrap(), "#");
    }

    #[test]
    fn language_toml_nested_array_merges() {
        const USER: &str = r#"
        [[language]]
        name = "typescript"
        language-server = { command = "deno", args = ["lsp"] }
        "#;

        let base = include_bytes!("../../languages.toml");
        let base = str::from_utf8(base).expect("Couldn't parse built-in languages config");
        let base: Value = toml::from_str(base).expect("Couldn't parse built-in languages config");
        let user: Value = toml::from_str(USER).unwrap();

        let merged = merge_toml_values(base, user, 3);
        let languages = merged.get("language").unwrap().as_array().unwrap();
        let ts = languages
            .iter()
            .find(|v| v.get("name").unwrap().as_str().unwrap() == "typescript")
            .unwrap();
        assert_eq!(
            ts.get("language-server")
                .unwrap()
                .get("args")
                .unwrap()
                .as_array()
                .unwrap(),
            &vec![Value::String("lsp".into())]
        )
    }
}
