use ch::ClickhouseExtension;
use eyre::Context;

#[derive(clap::Parser)]
pub struct Command {
    /// ClickHouse server URL (e.g., http://localhost:8123)
    #[clap(
        long = "clickhouse-url",
        short = 'c',
        env = "CLICKHOUSE_URL",
        default_value = "",
        global = true
    )]
    pub url: String,

    /// ClickHouse username for authentication
    #[clap(
        long = "clickhouse-user",
        short = 'u',
        env = "CLICKHOUSE_USER",
        global = true
    )]
    pub username: Option<String>,

    /// ClickHouse password for authentication
    #[clap(
        long = "clickhouse-password",
        short = 'p',
        env = "CLICKHOUSE_PASSWORD",
        global = true
    )]
    pub password: Option<String>,

    /// Additional ClickHouse request options (space-delimited key=value pairs)
    #[clap(long = "clickhouse-option", short='o', env = "CLICKHOUSE_OPTIONS", value_parser = ch::parse_request_options, global = true, value_delimiter = ',')]
    pub options: Vec<(String, String)>,

    #[clap(subcommand)]
    command: SubCommands,
}

#[derive(clap::Parser)]
enum SubCommands {
    ListDatabases,
    ListTables {
        /// Database name to list tables from
        #[clap(long, short = 'd')]
        database: String,
    },
}

impl Command {
    pub async fn execute(self) -> eyre::Result<()> {
        let Command {
            username,
            password,
            url,
            options,
            command,
        } = self;

        if url.is_empty() {
            eyre::bail!(
                "ClickHouse URL must be provided via --clickhouse-url or CLICKHOUSE_URL env variable"
            );
        }

        let builder = ch::Builder::new(url)
            .with_username(username)
            .with_password(password)
            .with_options(options);

        let ch_client = builder
            .to_client()
            .wrap_err_with(|| "Failed to build ClickHouse client")?;

        match command {
            SubCommands::ListDatabases => {
                let dbs = ch_client
                    .list_databases()
                    .await
                    .wrap_err_with(|| "Failed to list databases")?;
                eprintln!("Databases:");
                for db in dbs {
                    eprintln!("- {}", db);
                }
            }
            SubCommands::ListTables { database } => {
                let tables = ch_client.list_tables(&database).await.wrap_err_with(|| {
                    format!("Failed to list tables in database '{}'", database)
                })?;
                eprintln!("Tables in database '{}':", database);
                for table in tables {
                    eprintln!("- {}", table);
                }
            }
        }
        Ok(())
    }
}
