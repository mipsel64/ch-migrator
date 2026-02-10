mod error;

use std::sync::Arc;

use ch::{ClickhouseExtension, clickhouse};
pub use error::Error;

#[async_trait::async_trait]
pub trait Status: Send + Sync {
    async fn status(
        &self,
        backup_ids: &[String],
        since: std::time::Duration,
    ) -> Result<Vec<BackupStatus>, Error>;
}

#[async_trait::async_trait]
pub trait Backup: Send + Sync {
    async fn backup(&self, config: BackupConfig) -> Result<Vec<String>, Error>;
}

#[async_trait::async_trait]
pub trait Restore: Send + Sync {
    async fn restore(&self, config: RestoreConfig) -> Result<Vec<String>, Error>;
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<clickhouse::Client>,
}

impl Client {
    pub fn from_client(client: clickhouse::Client) -> Self {
        Self {
            inner: Arc::new(client),
        }
    }
}

#[async_trait::async_trait]
impl Backup for Client {
    async fn backup(&self, cfg: BackupConfig) -> Result<Vec<String>, Error> {
        cfg.validate()?;

        // Verify database exists
        let dbs = self
            .inner
            .list_databases()
            .await
            .map_err(Error::ClickhouseError)?;

        if !dbs.contains(&cfg.db) {
            return Err(Error::InvalidInput(format!(
                "Database '{}' does not exist",
                cfg.db
            )));
        }

        // Verify tables exist
        let tables = self
            .inner
            .list_tables(&cfg.db)
            .await
            .map_err(Error::ClickhouseError)?;

        if !cfg.tables.is_empty() {
            for table in &cfg.tables {
                if !tables.contains(table) {
                    return Err(Error::InvalidInput(format!(
                        "Table '{}' does not exist in database '{}'",
                        table, cfg.db
                    )));
                }
            }
        }

        let options_str = if !cfg.options.is_empty() {
            format!(" SETTINGS {}", cfg.options.join(" "))
        } else {
            "".to_string()
        };

        let mut buffer = "BACKUP TABLE ?.? TO ".to_string();

        let url = cfg.backup_to.s3_url().unwrap_or_default();

        match &cfg.backup_to {
            StoreMethod::S3 { .. } => {
                buffer.push_str("S3(?, ?, ?)");
            }
            StoreMethod::Disk { .. } => {
                buffer.push_str("DISK(?, ?)");
            }
            StoreMethod::File(_) => {
                buffer.push_str("FILE(?)");
            }
        }

        buffer.push_str(" ASYNC"); // Always use ASYNC to avoid blocking the client connection
        buffer.push_str(&options_str);

        let mut ret = Vec::with_capacity(tables.len());
        tracing::info!("Starting backup for database '{}'", cfg.db);
        for table in &cfg.tables {
            tracing::info!(" - Table '{}'", table);
            let mut query = self.inner.query(&buffer).bind(&cfg.db).bind(table);

            match &cfg.backup_to {
                StoreMethod::S3 {
                    access_key,
                    secret_key,
                    ..
                } => {
                    query = query.bind(&url).bind(access_key).bind(secret_key);
                }
                StoreMethod::Disk { name, path } => {
                    query = query.bind(name).bind(path);
                }
                StoreMethod::File(path) => {
                    query = query.bind(path);
                }
            }

            let backup_id: String = query.fetch_one().await.map_err(Error::ClickhouseError)?;
            ret.push(backup_id);
        }
        Ok(ret)
    }
}

#[async_trait::async_trait]
impl Restore for Client {
    async fn restore(&self, cfg: RestoreConfig) -> Result<Vec<String>, Error> {
        cfg.validate()?;

        let RestoreConfig {
            restore_from,
            target_db,
            source_db,
            tables,
            mut options,
            mode,
        } = cfg;

        let target_db = target_db.unwrap_or_else(|| source_db.clone());

        let avail_tables = restore_from.list_tables(&self.inner, &target_db).await?;

        if tables.is_empty() {
            return Err(Error::InvalidInput(
                "No tables found in the backup source".to_string(),
            ));
        }

        tracing::info!(
            "Found {} table(s) in backup source for database '{}'",
            tables.len(),
            target_db
        );

        // If no tables specified, restore all tables found
        let tables_to_restore = if tables.is_empty() {
            tables
        } else {
            // Verify specified tables exist in the backup
            for table in &tables {
                if !avail_tables.contains(table) {
                    return Err(Error::InvalidInput(format!(
                        "Table '{}' not found in backup source",
                        table
                    )));
                }
            }
            tables
        };

        if let Some(mode) = mode {
            match mode {
                RestoreMode::StructureOnly => {
                    options.push("structure_only=1".to_string());
                }
                RestoreMode::DataOnly => {
                    options.push("struct_only=0".to_string());
                    options.push("allow_non_empty_tables=1".to_string());
                }
            }
        }

        let options_str = if !options.is_empty() {
            format!(" SETTINGS {}", options.join(" "))
        } else {
            "".to_string()
        };

        let mut buffer = "RESTORE TABLE ?.? FROM ".to_string();

        match &restore_from {
            StoreMethod::S3 { .. } => {
                buffer.push_str("S3(?, ?, ?)");
            }
            StoreMethod::Disk { .. } => {
                buffer.push_str("DISK(?, ?)");
            }
            StoreMethod::File(_) => {
                buffer.push_str("FILE(?)");
            }
        }

        let s3_url = restore_from
            .s3_url()
            .map(|url| {
                format!(
                    "{}/{}",
                    url.trim_end_matches('/'),
                    source_db.trim_end_matches('/')
                )
            })
            .unwrap_or_default();

        buffer.push_str(" ASYNC"); // Always use ASYNC to avoid blocking the client connection
        buffer.push_str(&options_str);

        let mut ret: Vec<String> = Vec::with_capacity(tables_to_restore.len());
        tracing::info!(
            "Starting restore for '{}' from database '{}'",
            target_db,
            source_db
        );

        for table in &tables_to_restore {
            tracing::info!(" - Table '{}'", table);
            let mut query = self.inner.query(&buffer).bind(&target_db).bind(table);

            match &restore_from {
                StoreMethod::S3 {
                    access_key,
                    secret_key,
                    ..
                } => {
                    let url = format!("{}/{}", s3_url, table.trim_end_matches('/'));
                    query = query.bind(&url).bind(access_key).bind(secret_key);
                }
                StoreMethod::Disk { name, path } => {
                    query = query.bind(name).bind(format!(
                        "{}/{}/{}",
                        path.trim_end_matches('/'),
                        source_db.trim_end_matches('/'),
                        table.trim_end_matches('/')
                    ));
                }
                StoreMethod::File(path) => {
                    query = query.bind(format!(
                        "{}/{}/{}",
                        path.trim_end_matches('/'),
                        source_db.trim_end_matches('/'),
                        table.trim_end_matches('/')
                    ));
                }
            }

            let backup_id: String = query.fetch_one().await.map_err(Error::ClickhouseError)?;
            ret.push(backup_id);
        }

        Ok(ret)
    }
}

#[async_trait::async_trait]
impl Status for Client {
    async fn status(
        &self,
        backup_ids: &[String],
        since: std::time::Duration,
    ) -> Result<Vec<BackupStatus>, Error> {
        let mut buffer = "SELECT
                    id,
                    name,
                    status,
                    formatReadableSize(total_size) as total_size_fmt,
                    num_files,
                    files_read,
                    formatReadableSize(bytes_read) as bytes_read_fmt,
                    if(total_size > 0, bytes_read * 100.0 / total_size, 0.0) as progress_pct,
                    start_time,
                    end_time,
                    if (end_time > start_time, dateDiff('second', start_time, end_time), dateDiff('second', start_time, now())) as duration_seconds,
                    error
                FROM system.backups
                WHERE start_time >= fromUnixTimestamp64Second(?)".to_string();

        if !backup_ids.is_empty() {
            buffer.push_str(" AND id IN ?");
        }
        buffer.push_str("\nORDER BY start_time DESC");

        let mut query = self
            .inner
            .query(&buffer)
            .bind((chrono::Utc::now() - since).timestamp());
        if !backup_ids.is_empty() {
            query = query.bind(backup_ids);
        }

        query
            .fetch_all()
            .await
            .map_err(crate::Error::ClickhouseError)
    }
}

impl TryFrom<ch::Builder> for Client {
    type Error = ch::Error;

    fn try_from(value: ch::Builder) -> Result<Self, Self::Error> {
        let client = value.to_client()?;
        Ok(Self::from_client(client))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, clickhouse::Row)]
pub struct BackupStatus {
    pub id: String,
    pub name: String,
    pub status: String,
    pub total_size_fmt: String,
    pub num_files: u64,
    pub file_read: u64,
    pub bytes_read_fmt: String,
    pub progress_pct: f32,
    pub start_time: String,
    pub end_time: Option<String>,
    pub dureation_secs: Option<f64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BackupConfig {
    pub db: String,
    pub tables: Vec<String>,
    pub backup_to: StoreMethod,
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum RestoreMode {
    StructureOnly,
    DataOnly,
}

#[derive(Debug, Clone)]
pub struct RestoreConfig {
    pub restore_from: StoreMethod,
    pub target_db: Option<String>,
    pub source_db: String,
    pub tables: Vec<String>,
    pub options: Vec<String>,
    pub mode: Option<RestoreMode>,
}

impl BackupConfig {
    pub fn new(method: StoreMethod, db: impl Into<String>) -> Self {
        Self {
            db: db.into(),
            tables: vec![],
            backup_to: method,
            options: vec![],
        }
    }

    pub fn store_method(mut self, method: StoreMethod) -> Self {
        self.backup_to = method;
        self
    }

    pub fn tables(mut self, tables: Vec<String>) -> Self {
        self.tables = tables;
        self
    }

    pub fn db(mut self, db: impl Into<String>) -> Self {
        self.db = db.into();
        self
    }

    pub fn add_table(mut self, table: impl Into<String>) -> Self {
        self.tables.push(table.into());
        self
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.db.is_empty() {
            return Err(Error::InvalidInput(
                "Database name must be specified".to_string(),
            ));
        }

        self.backup_to.validate()?;
        Ok(())
    }

    pub fn options(mut self, options: Vec<String>) -> Self {
        self.options = options;
        self
    }

    pub fn add_option(mut self, option: impl Into<String>) -> Self {
        self.options.push(option.into());
        self
    }
}

impl RestoreConfig {
    pub fn new(method: StoreMethod, src_db: impl Into<String>) -> Self {
        Self {
            restore_from: method,
            source_db: src_db.into(),
            tables: vec![],
            options: vec![],
            mode: None,
            target_db: None,
        }
    }

    pub fn store_method(mut self, method: StoreMethod) -> Self {
        self.restore_from = method;
        self
    }

    pub fn target_db<T>(mut self, db: Option<T>) -> Self
    where
        T: Into<String>,
    {
        self.target_db = db.map(|d| d.into());
        self
    }

    pub fn source_db(mut self, db: impl Into<String>) -> Self {
        self.source_db = db.into();
        self
    }

    pub fn tables(mut self, tables: Vec<String>) -> Self {
        self.tables = tables;
        self
    }

    pub fn add_table(mut self, table: impl Into<String>) -> Self {
        self.tables.push(table.into());
        self
    }

    pub fn options(mut self, options: Vec<String>) -> Self {
        self.options = options;
        self
    }

    pub fn add_option(mut self, option: impl Into<String>) -> Self {
        self.options.push(option.into());
        self
    }

    pub fn mode(mut self, mode: RestoreMode) -> Self {
        self.mode = Some(mode);
        self
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.source_db.is_empty() {
            return Err(Error::InvalidInput(
                "Source database name must be specified".to_string(),
            ));
        }

        self.restore_from.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum StoreMethod {
    S3 {
        url: String,
        access_key: String,
        secret_key: String,
        prefix_path: Option<String>,
    },
    Disk {
        name: String,
        path: String,
    },
    File(String),
}

impl StoreMethod {
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            StoreMethod::S3 {
                url,
                access_key,
                secret_key,
                ..
            } => {
                if url.is_empty() {
                    return Err(Error::InvalidInput("S3 URL must be specified".to_string()));
                }

                if access_key.is_empty() {
                    return Err(Error::InvalidInput(
                        "S3 Access Key must be specified".to_string(),
                    ));
                }

                if secret_key.is_empty() {
                    return Err(Error::InvalidInput(
                        "S3 Secret Key must be specified".to_string(),
                    ));
                }
            }
            StoreMethod::Disk { name, path } => {
                if name.is_empty() {
                    return Err(Error::InvalidInput(
                        "Disk name must be specified".to_string(),
                    ));
                }

                if path.is_empty() {
                    return Err(Error::InvalidInput(
                        "Disk path must be specified".to_string(),
                    ));
                }
            }
            StoreMethod::File(path) => {
                if path.is_empty() {
                    return Err(Error::InvalidInput(
                        "File path must be specified".to_string(),
                    ));
                }
            }
        }

        Ok(())
    }

    fn s3_url(&self) -> Option<String> {
        match self {
            StoreMethod::S3 {
                url, prefix_path, ..
            } => {
                let url = if let Some(prefix) = prefix_path {
                    format!(
                        "{}/{}",
                        url.trim_end_matches('/'),
                        prefix.trim_start_matches('/')
                    )
                } else {
                    url.clone()
                };
                Some(url)
            }

            _ => None,
        }
    }

    async fn list_tables(
        &self,
        client: &clickhouse::Client,
        db: &str,
    ) -> Result<Vec<String>, Error> {
        let mut buffer =
            "SELECT DISTINCT arrayElement(splitByChar('/', _path), -2) AS table_name FROM "
                .to_string();

        match self {
            StoreMethod::S3 { .. } => {
                buffer.push_str("s3('?', '?', '?') ");
            }
            StoreMethod::Disk { .. } => {
                buffer.push_str("disk('?', '?') ");
            }
            StoreMethod::File(_) => {
                buffer.push_str("file('?') ");
            }
        }

        buffer.push_str("ORDER BY table_name");

        let mut query = client.query(&buffer);
        match self {
            StoreMethod::S3 {
                access_key,
                secret_key,
                ..
            } => {
                let url = format!(
                    "{}/{}/*/.backup",
                    self.s3_url().unwrap_or_default().trim_end_matches('/'),
                    db
                );
                query = query.bind(url).bind(access_key).bind(secret_key);
            }
            StoreMethod::Disk { name, path } => {
                query = query.bind(name).bind(format!(
                    "{}/{}/*/.backup",
                    path.trim_end_matches('/'),
                    db
                ));
            }
            StoreMethod::File(path) => {
                query = query.bind(format!("{}/{}/*/.backup", path.trim_end_matches('/'), db));
            }
        }

        let tables: Vec<String> = query.fetch_all().await.map_err(Error::ClickhouseError)?;
        Ok(tables)
    }
}
