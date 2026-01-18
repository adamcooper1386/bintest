//! Spec file loader.
//!
//! Loads and parses test specification files from disk.

use crate::schema::TestSpec;
use std::path::Path;

/// Error type for spec loading operations.
#[derive(Debug)]
pub enum LoadError {
    /// Failed to read the file.
    Io(std::io::Error),
    /// Failed to parse YAML.
    Yaml(serde_yaml::Error),
    /// Unsupported file extension.
    UnsupportedFormat(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "failed to read file: {e}"),
            LoadError::Yaml(e) => write!(f, "invalid YAML: {e}"),
            LoadError::UnsupportedFormat(ext) => {
                write!(f, "unsupported file format: {ext} (expected .yaml or .yml)")
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// Load a test spec from a file path.
pub fn load_spec(path: &Path) -> Result<TestSpec, LoadError> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    match ext {
        "yaml" | "yml" => {
            let contents = std::fs::read_to_string(path).map_err(LoadError::Io)?;
            serde_yaml::from_str(&contents).map_err(LoadError::Yaml)
        }
        other => Err(LoadError::UnsupportedFormat(other.to_string())),
    }
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
            && (ext == "yaml" || ext == "yml")
        {
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
    fn find_specs_in_directory() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.yaml"), "").unwrap();
        std::fs::write(dir.path().join("b.yml"), "").unwrap();
        std::fs::write(dir.path().join("c.txt"), "").unwrap();

        let specs = find_specs(dir.path()).unwrap();
        assert_eq!(specs.len(), 2);
    }
}
