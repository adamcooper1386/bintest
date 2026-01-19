//! Spec file loader.
//!
//! Loads and parses test specification files from disk.

use crate::schema::{SuiteConfig, TestSpec};
use std::path::Path;

/// Error type for spec loading operations.
#[derive(Debug)]
pub enum LoadError {
    /// Failed to read the file.
    Io(std::io::Error),
    /// Failed to parse YAML.
    Yaml(serde_yaml::Error),
    /// Failed to parse TOML.
    Toml(toml::de::Error),
    /// Unsupported file extension.
    UnsupportedFormat(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "failed to read file: {e}"),
            LoadError::Yaml(e) => write!(f, "invalid YAML: {e}"),
            LoadError::Toml(e) => write!(f, "invalid TOML: {e}"),
            LoadError::UnsupportedFormat(ext) => {
                write!(
                    f,
                    "unsupported file format: {ext} (expected .yaml, .yml, or .toml)"
                )
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// The name of the suite configuration file.
pub const SUITE_CONFIG_FILENAME: &str = "bintest.yaml";

/// Load a test spec from a file path.
pub fn load_spec(path: &Path) -> Result<TestSpec, LoadError> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let contents = std::fs::read_to_string(path).map_err(LoadError::Io)?;

    match ext {
        "yaml" | "yml" => serde_yaml::from_str(&contents).map_err(LoadError::Yaml),
        "toml" => toml::from_str(&contents).map_err(LoadError::Toml),
        other => Err(LoadError::UnsupportedFormat(other.to_string())),
    }
}

/// Load suite configuration from a directory.
///
/// Looks for `bintest.yaml` in the given directory.
/// Returns `None` if the file doesn't exist, `Err` if it exists but is invalid.
pub fn load_suite_config(dir: &Path) -> Result<Option<SuiteConfig>, LoadError> {
    let config_path = dir.join(SUITE_CONFIG_FILENAME);

    if !config_path.exists() {
        return Ok(None);
    }

    let contents = std::fs::read_to_string(&config_path).map_err(LoadError::Io)?;
    let config: SuiteConfig = serde_yaml::from_str(&contents).map_err(LoadError::Yaml)?;
    Ok(Some(config))
}

/// Find all spec files in a directory or return the single file.
pub fn find_specs(path: &Path) -> Result<Vec<std::path::PathBuf>, std::io::Error> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }

    let mut specs = Vec::new();
    collect_specs_recursive(path, &mut specs)?;
    specs.sort();
    Ok(specs)
}

fn collect_specs_recursive(
    dir: &Path,
    specs: &mut Vec<std::path::PathBuf>,
) -> Result<(), std::io::Error> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            collect_specs_recursive(&path, specs)?;
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && (ext == "yaml" || ext == "yml" || ext == "toml")
        {
            // Skip suite config file
            if path.file_name().is_some_and(|f| f == SUITE_CONFIG_FILENAME) {
                continue;
            }
            specs.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn load_valid_spec() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            r#"
version: 1
tests:
  - name: test1
    run:
      cmd: echo
    expect:
      exit: 0
"#
        )
        .unwrap();

        let spec = load_spec(&path).unwrap();
        assert_eq!(spec.version, 1);
        assert_eq!(spec.tests.len(), 1);
    }

    #[test]
    fn load_invalid_yaml() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.yaml");
        std::fs::write(&path, "invalid: [yaml: {").unwrap();

        let result = load_spec(&path);
        assert!(matches!(result, Err(LoadError::Yaml(_))));
    }

    #[test]
    fn unsupported_format() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "").unwrap();

        let result = load_spec(&path);
        assert!(matches!(result, Err(LoadError::UnsupportedFormat(_))));
    }

    #[test]
    fn load_valid_toml_spec() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.toml");
        std::fs::write(
            &path,
            r#"
version = 1

[[tests]]
name = "test1"

[tests.run]
cmd = "echo"

[tests.expect]
exit = 0
"#,
        )
        .unwrap();

        let spec = load_spec(&path).unwrap();
        assert_eq!(spec.version, 1);
        assert_eq!(spec.tests.len(), 1);
        assert_eq!(spec.tests[0].name, "test1");
    }

    #[test]
    fn load_invalid_toml() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "invalid = [toml").unwrap();

        let result = load_spec(&path);
        assert!(matches!(result, Err(LoadError::Toml(_))));
    }

    #[test]
    fn find_specs_in_directory() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.yaml"), "").unwrap();
        std::fs::write(dir.path().join("b.yml"), "").unwrap();
        std::fs::write(dir.path().join("c.toml"), "").unwrap();
        std::fs::write(dir.path().join("d.txt"), "").unwrap();

        let specs = find_specs(dir.path()).unwrap();
        assert_eq!(specs.len(), 3);
    }

    #[test]
    fn find_specs_excludes_bintest_yaml() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.yaml"), "").unwrap();
        std::fs::write(dir.path().join("bintest.yaml"), "version: 1").unwrap();

        let specs = find_specs(dir.path()).unwrap();
        assert_eq!(specs.len(), 1);
        assert!(specs[0].file_name().unwrap() != "bintest.yaml");
    }

    #[test]
    fn load_suite_config_not_found() {
        let dir = tempdir().unwrap();
        let result = load_suite_config(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_suite_config_valid() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("bintest.yaml"),
            r#"
version: 1
timeout: 10
env:
  MY_VAR: my_value
"#,
        )
        .unwrap();

        let config = load_suite_config(dir.path()).unwrap().unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.timeout, Some(10));
        assert_eq!(config.env.get("MY_VAR"), Some(&"my_value".to_string()));
    }

    #[test]
    fn load_suite_config_invalid() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("bintest.yaml"), "invalid: [yaml: {").unwrap();

        let result = load_suite_config(dir.path());
        assert!(matches!(result, Err(LoadError::Yaml(_))));
    }
}
