//! File access and commands for the script host: what the guest asks of
//! the server's side (crates/temzdf-script/src/native_fs.rs and
//! native_process.rs do it, with the OS's own paths) and the answers. They cross the guest's imports `temzdf.fs_*`
//! (waited for) and `temzdf.recv` (asynchronous ones, [`ToGuest`]), framed
//! as [`crate::send`] frames them; the guest has no file system of its own
//! (doc/design/script-host.md, "The sandbox").

use serde::{Deserialize, Serialize};

/// One operation. Paths are the OS's own, absolute (relative ones are
/// refused: there is no working directory to resolve them against).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FsOp {
    /// A regular file's bytes (symbolic links followed).
    ReadFile(String),
    /// What a path is, symbolic links followed.
    Stat(String),
    /// What a path is, a symbolic link itself.
    Lstat(String),
    /// A directory's entries, in the order the OS gives them.
    ReadDir(String),
    /// The path with every symbolic link resolved.
    RealPath(String),
    /// Where a command is: its absolute path, as `Run` would find it
    /// (`temzdf/process`'s `exepath`, Vim's `exepath()`).
    Exepath(String),
    /// Run a command to its end (`temzdf/process`'s `run`), each on a
    /// thread of its own.
    Run(RunSpec),
    /// Kill the command the operation of this id runs, answering nothing
    /// (its `Run` answers, killed).
    Kill(u64),
}

/// A command to run: no shell; `program` is found as [`FsOp::Exepath`]
/// finds it (with `env`'s `PATH` when it has one).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Absolute; `None`: the server's working directory.
    pub cwd: Option<String>,
    /// Added to the server's environment.
    pub env: Vec<(String, String)>,
    /// Written to its standard input, then closed; `None`: none (null).
    pub stdin: Option<Vec<u8>>,
    /// Killed after this long (an error, `TimedOut`).
    pub timeout_ms: Option<u64>,
}

/// How a command ended, and what it wrote.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Exit {
    /// Its exit code (none when a signal ended it).
    pub code: Option<i32>,
    /// The signal that ended it (Unix).
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// What an operation answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FsReply {
    Bytes(Vec<u8>),
    Info(FileInfo),
    Entries(Vec<DirEntry>),
    Path(String),
    /// A command's path (`Exepath`), if found.
    Exe(Option<String>),
    Exited(Exit),
}

pub type FsResult = Result<FsReply, FsError>;

/// What the guest's `temzdf.recv` hands it: the session's messages, and
/// the asynchronous file operations' results (`temzdf.fs_start`) as they
/// complete, in one queue.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToGuest {
    Msg(crate::script::ToHost),
    FsDone { id: u64, result: FsResult },
}

/// As Deno's `FileInfo`, what every OS has.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileInfo {
    pub is_file: bool,
    pub is_directory: bool,
    pub is_symlink: bool,
    pub size: u64,
    /// Milliseconds since the Unix epoch, when the OS tells.
    pub mtime: Option<f64>,
    pub atime: Option<f64>,
    pub birthtime: Option<f64>,
}

/// One entry of a directory (as Deno's `DirEntry`: the entry itself, a
/// symbolic link not followed).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DirEntry {
    /// Not UTF-8: with replacement characters.
    pub name: String,
    pub is_file: bool,
    pub is_directory: bool,
    pub is_symlink: bool,
}

/// Why an operation failed: the kind (the scripts' error classes) and a
/// message that is the same on every OS.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FsError {
    pub kind: FsErrorKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsErrorKind {
    NotFound,
    PermissionDenied,
    IsADirectory,
    NotADirectory,
    /// Neither a regular file nor a directory where one is read (a FIFO, a
    /// device).
    NotAFile,
    /// Larger than the host reads at once (a file; a command's output).
    TooLarge,
    /// A command ran longer than its timeout.
    TimedOut,
    /// More entries than the host lists at once.
    TooMany,
    NotAbsolute,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(msg: &T) {
        let mut buf = Vec::new();
        crate::send(&mut buf, msg).unwrap();
        assert_eq!(crate::recv::<T>(&mut buf.as_slice()).unwrap().as_ref(), Some(msg));
    }

    #[test]
    fn operations_and_answers_cross() {
        round_trip(&FsOp::ReadDir("/a".into()));
        round_trip(&FsOp::Run(RunSpec {
            program: "git".into(),
            args: vec!["status".into()],
            cwd: Some("/a".into()),
            env: vec![("K".into(), "V".into())],
            stdin: Some(b"in".to_vec()),
            timeout_ms: Some(5),
        }));
        round_trip(&FsReply::Exited(Exit { code: Some(1), signal: None, stdout: b"o".to_vec(), stderr: Vec::new() }));
        let info = FileInfo {
            is_file: true,
            is_directory: false,
            is_symlink: false,
            size: 3,
            mtime: Some(1.5),
            atime: None,
            birthtime: None,
        };
        round_trip::<FsResult>(&Ok(FsReply::Info(info)));
        round_trip::<FsResult>(&Ok(FsReply::Bytes(vec![0, 255])));
        round_trip::<FsResult>(&Err(FsError { kind: FsErrorKind::TooLarge, message: "x".into() }));
        round_trip(&ToGuest::FsDone { id: 7, result: Ok(FsReply::Path("/a".into())) });
        round_trip(&ToGuest::Msg(crate::script::ToHost::Interrupt));
    }
}
