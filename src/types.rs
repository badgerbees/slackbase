pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Serde(serde_json::Error),
    NotFound,
    InvalidRecord,
    Lua(mlua::Error),
    SystemTime(std::time::SystemTimeError),
}

impl From<std::time::SystemTimeError> for Error {
    fn from(err: std::time::SystemTimeError) -> Self {
        Error::SystemTime(err)
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io(err)
    }
}
impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::Serde(err)
    }
}
impl From<mlua::Error> for Error {
    fn from(err: mlua::Error) -> Self {
        Error::Lua(err)
    }
}