//! Environment variable interpolation utilities.

use std::collections::HashMap;

/// Interpolate environment variables in a string.
///
/// Supports `${VAR}` syntax. Returns an error message if a referenced variable is not set.
///
/// # Examples
///
/// ```
/// std::env::set_var("MY_VAR", "hello");
/// assert_eq!(bintest::env::interpolate_env("${MY_VAR}").unwrap(), "hello");
/// assert_eq!(bintest::env::interpolate_env("prefix_${MY_VAR}_suffix").unwrap(), "prefix_hello_suffix");
/// ```
pub fn interpolate_env(s: &str) -> Result<String, String> {
    interpolate_env_with(s, &HashMap::new())
}

/// Interpolate environment variables in a string, with additional variables from a map.
///
/// First checks the provided map, then falls back to system environment variables.
/// Supports `${VAR}` syntax. Returns an error message if a referenced variable is not set.
pub fn interpolate_env_with(s: &str, env: &HashMap<String, String>) -> Result<String, String> {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut var_name = String::new();
            loop {
                match chars.next() {
                    Some('}') => break,
                    Some(c) => var_name.push(c),
                    None => {
                        return Err(format!("Unclosed variable reference: ${{{var_name}"));
                    }
                }
            }
            // First check provided env map, then system env
            let value = env
                .get(&var_name)
                .cloned()
                .or_else(|| std::env::var(&var_name).ok())
                .ok_or_else(|| format!("Environment variable '{var_name}' is not set"))?;
            result.push_str(&value);
        } else {
            result.push(c);
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interpolate_env() {
        // SAFETY: This test is single-threaded and only modifies BINTEST_TEST_VAR
        unsafe {
            std::env::set_var("BINTEST_TEST_VAR", "hello");
        }
        assert_eq!(interpolate_env("${BINTEST_TEST_VAR}").unwrap(), "hello");
        assert_eq!(
            interpolate_env("prefix_${BINTEST_TEST_VAR}_suffix").unwrap(),
            "prefix_hello_suffix"
        );
        assert_eq!(interpolate_env("no vars here").unwrap(), "no vars here");
        assert_eq!(interpolate_env("").unwrap(), "");
    }

    #[test]
    fn test_interpolate_env_missing_var() {
        let result = interpolate_env("${NONEXISTENT_VAR_12345}");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("NONEXISTENT_VAR_12345"));
    }

    #[test]
    fn test_interpolate_env_unclosed() {
        let result = interpolate_env("${UNCLOSED");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unclosed"));
    }
}
