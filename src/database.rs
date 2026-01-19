//! Database connection manager.
//!
//! Manages database connections for SQL assertions and setup/teardown operations.
//! Supports PostgreSQL and SQLite with lazy initialization and per-file pooling.

// Allow dead code for now - these will be used when SQL assertions are implemented.
#![allow(dead_code)]

use crate::schema::{DatabaseConfig, DbDriver};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Error type for database operations.
#[derive(Debug)]
pub struct DbError {
    pub message: String,
    /// Database name that caused the error.
    pub database: Option<String>,
    /// The URL with password masked.
    pub masked_url: Option<String>,
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(db) = &self.database {
            write!(f, "Database '{}': {}", db, self.message)?;
            if let Some(url) = &self.masked_url {
                write!(f, " (URL: {})", url)?;
            }
            Ok(())
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for DbError {}

/// A database connection that can execute SQL statements.
pub enum Connection {
    /// PostgreSQL connection.
    Postgres(PostgresConnection),
    /// SQLite connection.
    Sqlite(SqliteConnection),
}

/// PostgreSQL connection wrapper.
pub struct PostgresConnection {
    client: tokio_postgres::Client,
    /// Handle to the connection task (kept alive for the connection duration).
    _handle: std::thread::JoinHandle<()>,
}

/// SQLite connection wrapper.
pub struct SqliteConnection {
    conn: rusqlite::Connection,
}

impl Connection {
    /// Execute a SQL statement and return the result as text.
    ///
    /// For queries that return rows, results are formatted as newline-separated values.
    /// For statements that don't return rows, returns an empty string.
    pub fn execute(&mut self, sql: &str) -> Result<String, DbError> {
        match self {
            Connection::Postgres(pg) => pg.execute(sql),
            Connection::Sqlite(sqlite) => sqlite.execute(sql),
        }
    }
}

impl PostgresConnection {
    /// Execute a SQL statement.
    fn execute(&mut self, sql: &str) -> Result<String, DbError> {
        use std::fmt::Write;

        // Use a simple runtime for blocking execution
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| DbError {
                message: format!("Failed to create runtime: {e}"),
                database: None,
                masked_url: None,
            })?;

        rt.block_on(async {
            let rows = self.client.query(sql, &[]).await.map_err(|e| DbError {
                message: format!("Query failed: {e}"),
                database: None,
                masked_url: None,
            })?;

            let mut result = String::new();
            for row in rows {
                let mut row_values = Vec::new();
                for i in 0..row.len() {
                    // Try to get the value as different types
                    let value = if let Ok(v) = row.try_get::<_, Option<String>>(i) {
                        v.unwrap_or_else(|| "NULL".to_string())
                    } else if let Ok(v) = row.try_get::<_, Option<i64>>(i) {
                        v.map(|n| n.to_string())
                            .unwrap_or_else(|| "NULL".to_string())
                    } else if let Ok(v) = row.try_get::<_, Option<i32>>(i) {
                        v.map(|n| n.to_string())
                            .unwrap_or_else(|| "NULL".to_string())
                    } else if let Ok(v) = row.try_get::<_, Option<f64>>(i) {
                        v.map(|n| n.to_string())
                            .unwrap_or_else(|| "NULL".to_string())
                    } else if let Ok(v) = row.try_get::<_, Option<bool>>(i) {
                        v.map(|b| b.to_string())
                            .unwrap_or_else(|| "NULL".to_string())
                    } else {
                        "<unknown>".to_string()
                    };
                    row_values.push(value);
                }
                if !result.is_empty() {
                    result.push('\n');
                }
                let _ = write!(result, "{}", row_values.join("\t"));
            }
            Ok(result)
        })
    }
}

impl SqliteConnection {
    /// Execute a SQL statement.
    fn execute(&mut self, sql: &str) -> Result<String, DbError> {
        use std::fmt::Write;

        // Try as a query first (SELECT, etc.)
        let mut stmt = match self.conn.prepare(sql) {
            Ok(s) => s,
            Err(e) => {
                return Err(DbError {
                    message: format!("Failed to prepare statement: {e}"),
                    database: None,
                    masked_url: None,
                });
            }
        };

        let column_count = stmt.column_count();
        if column_count == 0 {
            // This is likely a non-SELECT statement
            drop(stmt);
            self.conn.execute(sql, []).map_err(|e| DbError {
                message: format!("Execute failed: {e}"),
                database: None,
                masked_url: None,
            })?;
            return Ok(String::new());
        }

        let rows = stmt.query_map([], |row| {
            let mut values = Vec::new();
            for i in 0..column_count {
                let value: rusqlite::types::Value = row.get(i)?;
                let s = match value {
                    rusqlite::types::Value::Null => "NULL".to_string(),
                    rusqlite::types::Value::Integer(n) => n.to_string(),
                    rusqlite::types::Value::Real(f) => f.to_string(),
                    rusqlite::types::Value::Text(s) => s,
                    rusqlite::types::Value::Blob(_) => "<blob>".to_string(),
                };
                values.push(s);
            }
            Ok(values)
        });

        let rows = rows.map_err(|e| DbError {
            message: format!("Query failed: {e}"),
            database: None,
            masked_url: None,
        })?;

        let mut result = String::new();
        for row in rows {
            let values = row.map_err(|e| DbError {
                message: format!("Failed to read row: {e}"),
                database: None,
                masked_url: None,
            })?;
            if !result.is_empty() {
                result.push('\n');
            }
            let _ = write!(result, "{}", values.join("\t"));
        }
        Ok(result)
    }
}

/// Mask password in a database URL for error messages.
pub fn mask_password(url: &str) -> String {
    // Handle postgres:// and postgresql:// URLs
    // Use rfind to find the last @ (in case password contains @)
    if let Some(at_pos) = url.rfind('@')
        && let Some(proto_end) = url.find("://")
    {
        let before_creds = &url[..proto_end + 3];
        let after_at = &url[at_pos..];

        // Find the colon that separates user:pass
        let creds = &url[proto_end + 3..at_pos];
        if let Some(colon) = creds.find(':') {
            let user = &creds[..colon];
            return format!("{before_creds}{user}:****{after_at}");
        }
    }
    url.to_string()
}

/// Interpolate environment variables in a string.
///
/// Supports `${VAR}` syntax. Returns an error if a referenced variable is not set.
pub fn interpolate_env(s: &str) -> Result<String, DbError> {
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
                        return Err(DbError {
                            message: format!("Unclosed variable reference: ${{{var_name}"),
                            database: None,
                            masked_url: None,
                        });
                    }
                }
            }
            let value = std::env::var(&var_name).map_err(|_| DbError {
                message: format!("Environment variable '{var_name}' is not set"),
                database: None,
                masked_url: None,
            })?;
            result.push_str(&value);
        } else {
            result.push(c);
        }
    }

    Ok(result)
}

/// Connect to a database using the provided configuration.
fn connect(config: &DatabaseConfig, name: &str) -> Result<Connection, DbError> {
    // Interpolate environment variables in URL
    let url = interpolate_env(&config.url).map_err(|mut e| {
        e.database = Some(name.to_string());
        e
    })?;

    let masked = mask_password(&url);

    match config.driver {
        DbDriver::Postgres => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| DbError {
                    message: format!("Failed to create runtime: {e}"),
                    database: Some(name.to_string()),
                    masked_url: Some(masked.clone()),
                })?;

            let (client, connection) = rt
                .block_on(tokio_postgres::connect(&url, tokio_postgres::NoTls))
                .map_err(|e| DbError {
                    message: format!("Connection failed: {e}"),
                    database: Some(name.to_string()),
                    masked_url: Some(masked.clone()),
                })?;

            // Spawn connection handler in a background thread
            let handle = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to create runtime for connection");
                rt.block_on(async {
                    if let Err(e) = connection.await {
                        eprintln!("PostgreSQL connection error: {e}");
                    }
                });
            });

            Ok(Connection::Postgres(PostgresConnection {
                client,
                _handle: handle,
            }))
        }
        DbDriver::Sqlite => {
            // Parse SQLite URL format
            let path = if url == "sqlite::memory:" || url == ":memory:" {
                ":memory:".to_string()
            } else if let Some(path) = url.strip_prefix("sqlite:///") {
                path.to_string()
            } else if let Some(path) = url.strip_prefix("sqlite://") {
                path.to_string()
            } else {
                url.clone()
            };

            let conn = if path == ":memory:" {
                rusqlite::Connection::open_in_memory()
            } else {
                rusqlite::Connection::open(&path)
            }
            .map_err(|e| DbError {
                message: format!("Failed to open database: {e}"),
                database: Some(name.to_string()),
                masked_url: Some(masked),
            })?;

            Ok(Connection::Sqlite(SqliteConnection { conn }))
        }
    }
}

/// Manages database connections for a test file.
///
/// Connections are lazily initialized on first use and pooled per-file.
pub struct ConnectionManager {
    configs: HashMap<String, DatabaseConfig>,
    connections: Arc<Mutex<HashMap<String, Connection>>>,
}

impl ConnectionManager {
    /// Create a new connection manager with the given database configurations.
    ///
    /// Configurations from suite and file levels should be merged before creating
    /// the manager (file-level overrides suite-level for the same name).
    pub fn new(configs: HashMap<String, DatabaseConfig>) -> Self {
        Self {
            configs,
            connections: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get or create a connection to the named database.
    ///
    /// Returns an error if the database is not configured or connection fails.
    pub fn get(
        &self,
        name: &str,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<String, Connection>>, DbError> {
        let config = self.configs.get(name).ok_or_else(|| DbError {
            message: format!("Database '{name}' is not configured"),
            database: Some(name.to_string()),
            masked_url: None,
        })?;

        let mut connections = self.connections.lock().map_err(|_| DbError {
            message: "Connection pool lock poisoned".to_string(),
            database: Some(name.to_string()),
            masked_url: None,
        })?;

        // Create connection if it doesn't exist
        if !connections.contains_key(name) {
            let conn = connect(config, name)?;
            connections.insert(name.to_string(), conn);
        }

        Ok(connections)
    }

    /// Execute a SQL statement on the named database.
    pub fn execute(&self, database: &str, sql: &str) -> Result<String, DbError> {
        let mut connections = self.get(database)?;
        let conn = connections.get_mut(database).ok_or_else(|| DbError {
            message: format!("Connection for '{database}' not found after creation"),
            database: Some(database.to_string()),
            masked_url: None,
        })?;

        conn.execute(sql).map_err(|mut e| {
            e.database = Some(database.to_string());
            e
        })
    }

    /// Check if any databases are configured.
    pub fn has_databases(&self) -> bool {
        !self.configs.is_empty()
    }

    /// Get the driver type for a named database.
    pub fn get_driver(&self, name: &str) -> Option<DbDriver> {
        self.configs.get(name).map(|c| c.driver)
    }

    /// Close all connections.
    pub fn close_all(&self) {
        if let Ok(mut connections) = self.connections.lock() {
            connections.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_password() {
        assert_eq!(
            mask_password("postgres://user:secret@localhost:5432/db"),
            "postgres://user:****@localhost:5432/db"
        );
        assert_eq!(
            mask_password("postgresql://admin:p@ss@host/db"),
            "postgresql://admin:****@host/db"
        );
        // No password
        assert_eq!(
            mask_password("postgres://user@localhost/db"),
            "postgres://user@localhost/db"
        );
        // No credentials
        assert_eq!(
            mask_password("sqlite:///path/to/db"),
            "sqlite:///path/to/db"
        );
    }

    #[test]
    fn test_interpolate_env() {
        // SAFETY: This test is single-threaded and only modifies TEST_VAR
        unsafe {
            std::env::set_var("TEST_VAR", "hello");
        }
        assert_eq!(interpolate_env("${TEST_VAR}").unwrap(), "hello");
        assert_eq!(
            interpolate_env("prefix_${TEST_VAR}_suffix").unwrap(),
            "prefix_hello_suffix"
        );
        assert_eq!(interpolate_env("no vars here").unwrap(), "no vars here");
        // SAFETY: This test is single-threaded and only modifies TEST_VAR
        unsafe {
            std::env::remove_var("TEST_VAR");
        }
    }

    #[test]
    fn test_interpolate_env_missing_var() {
        let result = interpolate_env("${NONEXISTENT_VAR_12345}");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .message
                .contains("NONEXISTENT_VAR_12345")
        );
    }

    #[test]
    fn test_interpolate_env_unclosed() {
        let result = interpolate_env("${UNCLOSED");
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("Unclosed"));
    }

    #[test]
    fn test_sqlite_memory() {
        let config = DatabaseConfig {
            driver: DbDriver::Sqlite,
            url: "sqlite::memory:".to_string(),
        };

        let mut conn = connect(&config, "test").unwrap();

        // Create a table
        conn.execute("CREATE TABLE test (id INTEGER, name TEXT)")
            .unwrap();

        // Insert data
        conn.execute("INSERT INTO test VALUES (1, 'alice')")
            .unwrap();
        conn.execute("INSERT INTO test VALUES (2, 'bob')").unwrap();

        // Query data
        let result = conn
            .execute("SELECT id, name FROM test ORDER BY id")
            .unwrap();
        assert_eq!(result, "1\talice\n2\tbob");

        // Count
        let count = conn.execute("SELECT COUNT(*) FROM test").unwrap();
        assert_eq!(count, "2");
    }

    #[test]
    fn test_connection_manager_sqlite() {
        let mut configs = HashMap::new();
        configs.insert(
            "default".to_string(),
            DatabaseConfig {
                driver: DbDriver::Sqlite,
                url: "sqlite::memory:".to_string(),
            },
        );

        let manager = ConnectionManager::new(configs);
        assert!(manager.has_databases());

        // Create table and insert
        manager
            .execute("default", "CREATE TABLE users (name TEXT)")
            .unwrap();
        manager
            .execute("default", "INSERT INTO users VALUES ('test')")
            .unwrap();

        // Query
        let result = manager.execute("default", "SELECT * FROM users").unwrap();
        assert_eq!(result, "test");

        // Unknown database
        let err = manager.execute("unknown", "SELECT 1").unwrap_err();
        assert!(err.message.contains("not configured"));
    }
}
