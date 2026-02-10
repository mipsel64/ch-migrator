#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Input is invalid: {0}")]
    InvalidInput(String),

    #[error(transparent)]
    ClickhouseError(#[from] ch::ClickhouseError),
}
