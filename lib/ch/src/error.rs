#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Input Clickhouse URL is empty")]
    EmptyUrl,

    #[error(transparent)]
    ClickhouseError(#[from] crate::ClickhouseError),

    #[error("Invalid Input: {0}")]
    InvalidInput(String),
}
