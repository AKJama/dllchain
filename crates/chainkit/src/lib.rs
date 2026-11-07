//! chainkit: everything dllchain decides or changes in a game folder, with no window.
//!
//! Only one file next to the game can be called `dinput8.dll`. Mods that all ship under that name are put into one
//! chain: one of them stays `dinput8.dll` (the host) and starts the others, which are renamed and listed in
//! `dinput8-chain.ini`. The rules for that file are EMF's (matenm/emf-enshrouded, docs/modding/other_mods.md), so EMF
//! and dllchain read each other's setup.
//!
//! Every file this crate reads (a mod zip, the chain file, its own record) is untrusted input: paths go through
//! [`paths::RelPath`] and [`paths::resolve`], and a file is only deleted or moved back while its SHA-256 is the one
//! dllchain wrote.

pub mod chainini;
pub mod clock;
pub mod dllinfo;
pub mod engine;
pub mod game;
pub mod hash;
pub mod json;
pub mod paths;
pub mod procs;
pub mod record;
pub mod saves;
pub mod source;
pub mod steam;
pub mod txn;

/// The folder dllchain keeps its record, backups and log in, inside the game folder.
pub const OWN_DIR: &str = "dllchain";

/// What went wrong, in words a player can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// `return Err(Error(format!(...)))`
#[macro_export]
macro_rules! fail {
    ($($arg:tt)*) => { return Err($crate::Error(format!($($arg)*))) };
}
