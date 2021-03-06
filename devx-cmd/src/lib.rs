//! `devx-cmd` provides more convenient primitives for spawning child processes
//! than [`std::process`] targeted for use in development scripts specifically.
//!
//! The main entities of the crate are [`Cmd`] (builder for executable
//! commands), and [`ChildProcess`] (represents a spawned process).
//!
//! There are also some convenient macros to reduce boilerplate.
//! Here is the basic usage example:
//!
//! ```
//! use devx_cmd::{read, run, cmd, Cmd};
//!
//! # || -> devx_cmd::Result<()> {
//! // Run the program, logging the invocation to `stderr` and waiting until it finishes
//! // This is used only for side-effects.
//! // Note that if the process ends with a non-zero status code, this will return an error.
//! run!("ls", "-la")?;
//!
//! // Same as `run!()`, but captures the stdout and returns it as a `String`
//! // there is also a `read_bytes!()` for non-utf8 sequences
//! let output = read!("echo", "foo")?;
//! assert_eq!(output.trim(), "foo");
//!
//! # if run!("rustfmt", "--version").is_ok() {
//! let mut cmd = cmd!("rustfmt");
//! cmd
//!     // Don't log command invocation and output to stderr
//!     .echo_cmd(false)
//!     // Don't log invocation error to stderr
//!     .echo_err(false)
//!     .stdin("fn foo () -> u32 {42}\n");
//!
//! // Spawn without waiting for its completion, but capturing the stdout
//! let mut child = cmd.spawn_piped()?;
//!
//! // Read output line-by-line
//! let first_line = child.stdout_lines().next().unwrap();
//!
//! assert_eq!(first_line.trim(), "fn foo() -> u32 {");
//!
//! // Dropping the child process `kill()`s it (and ignores the `Result`)
//! // Use `.wait()/.read()` to wait until its completion.
//! drop(child);
//! # }
//!
//! # Ok(())
//! # }().unwrap();
//! ```
//!
//! [`Cmd`]: struct.Cmd.html
//! [`ChildProcess`]: struct.ChildProcess.html
//! [`std::process`]: https://doc.rust-lang.org/std/process/index.html

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]
// Makes rustc abort compilation if there are any unsafe blocks in the crate.
// Presence of this annotation is picked up by tools such as cargo-geiger
// and lets them ensure that there is indeed no unsafe code as opposed to
// something they couldn't detect (e.g. unsafe added via macro expansion, etc).
#![forbid(unsafe_code)]

use std::{
    borrow::Cow,
    env,
    ffi::OsString,
    fmt,
    io::{self, Read, Write},
    iter,
    path::PathBuf,
    process::Stdio,
    sync::Arc,
};

pub use error::*;
use io::BufRead;

/// Create a [`Cmd`] with the given binary and arguments.
///
/// The parameters to this macro may have completely different types.
/// The single requirement for them is to implement [`Into<OsString>`][os-string]
///
/// ```
/// # use devx_cmd::{cmd, Result};
/// # use std::path::Path;
/// # || -> Result<()> {
/// #
/// let path = Path::new("/foo/bar");
///
/// let cmd = cmd!("echo", "hi", path);
/// cmd.run()?;
/// #
/// # Ok(())
/// # }().unwrap();
/// ```
///
/// [`Cmd`]: struct.Cmd.html
/// [os-string]: https://doc.rust-lang.org/std/ffi/struct.OsString.html
#[macro_export]
macro_rules! cmd {
    ($bin:expr $(, $arg:expr )* $(,)?) => {{
        use ::std::{ffi::OsString, convert::Into};
        let mut cmd = $crate::Cmd::new($bin);
        // Type annotation for the case when 0 arguments are passed
        let args: &[OsString] = &[$(Into::<OsString>::into($arg)),*];
        cmd.args(args);
        cmd
    }};
}

/// Shortcut for `cmd!(...).run()`.
/// See [`Cmd::run`](struct.Cmd.html#method.run) for details
#[macro_export]
macro_rules! run {
    ($($params:tt)*) => {{ $crate::cmd!($($params)*).run() }}
}

/// Shortcut for `cmd!(...).read()`.
/// See [`Cmd::read`](struct.Cmd.html#method.read) for details
#[macro_export]
macro_rules! read {
    ($($params:tt)*) => {{ $crate::cmd!($($params)*).read() }}
}

/// Shortcut for `cmd!(...).read_bytes()`.
/// See [`Cmd::read`](struct.Cmd.html#method.read_bytes) for details
#[macro_export]
macro_rules! read_bytes {
    ($($params:tt)*) => {{ $crate::cmd!($($params)*).read_bytes() }}
}

mod error;

#[derive(Clone)]
enum BinOrUtf8 {
    Bin(Vec<u8>),
    Utf8(String),
}

impl fmt::Display for BinOrUtf8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinOrUtf8::Bin(bytes) => write!(f, "[bytes]: {:?}", bytes),
            BinOrUtf8::Utf8(utf8) => write!(f, "[utf8]: {}", utf8),
        }
    }
}

impl AsRef<[u8]> for BinOrUtf8 {
    fn as_ref(&self) -> &[u8] {
        match self {
            BinOrUtf8::Bin(it) => it.as_ref(),
            BinOrUtf8::Utf8(it) => it.as_ref(),
        }
    }
}

/// More convenient version of [`std::process::Command`]. Allows for
/// spawning child processes with or without capturing their stdout.
/// It also comes with inbuilt logging of the invocations to `stderr`.
///
/// All the methods for invoking a [`Cmd`]:
/// - [`spawn_piped`](struct.Cmd.html#method.spawn_piped)
/// - [`spawn`](struct.Cmd.html#method.spawn)
/// - [`run`](struct.Cmd.html#method.run)
/// - [`read`](struct.Cmd.html#method.read)
/// - [`read_bytes`](struct.Cmd.html#method.read_bytes)
///
/// For more laconic usage see [`cmd`] and other macros.
///
/// Example:
/// ```
/// # use devx_cmd::{Cmd, ChildProcess, Result};
/// # || -> Result<()> {
/// #
/// let mut cmd = Cmd::new("cargo");
/// cmd
///     // `arg*()` methods append arguments
///     .arg("metadata")
///     .arg2("--color", "never")
///     .args(&["--verbose", "--no-deps", "--all-features"])
///     .replace_arg(3, "--quiet")
///     // `echo*()` are `true` by default
///     .echo_cmd(false)
///     .echo_err(false)
///     // repetated `stdin*()` calls overwrite previous ones
///     .stdin("Hi")
///     .stdin_bytes(vec![0, 1, 2]);
///
/// let () = cmd.run()?;
/// let output: String = cmd.read()?;
/// let output: Vec<u8> = cmd.read_bytes()?;
/// let process: ChildProcess = cmd.spawn()?;
/// #
/// # Ok(())
/// # }().unwrap();
/// ```
///
/// [`cmd`]: macro.cmd.html
/// [`std::process::Command`]: https://doc.rust-lang.org/std/process/struct.Command.html
#[must_use = "commands are not executed until run(), read() or spawn() is called"]
#[derive(Clone)]
pub struct Cmd(Arc<CmdShared>);

#[derive(Clone)]
struct CmdShared {
    bin: PathBuf,
    args: Vec<OsString>,
    stdin: Option<BinOrUtf8>,
    current_dir: Option<PathBuf>,
    echo_cmd: bool,
    echo_err: bool,
}

impl fmt::Debug for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", (self.0).bin.display())?;
        for arg in &(self.0).args {
            write!(f, " {}", arg.to_string_lossy())?;
        }
        if let Some(dir) = &self.0.current_dir {
            write!(f, "\n(at {})", dir.display())?;
        }
        if let Some(stdin) = &self.0.stdin {
            write!(f, "\nstdin <<< {}", stdin)?;
        }
        Ok(())
    }
}

impl Cmd {
    /// Returns a command builder that invokes the binary at `bin`.
    /// You should also be able to pass the command by name if it is in `PATH`.
    ///
    /// Does not verify that the binary is actually available at the given path.
    /// If it isn't, then an error will be returned when executing the command.
    pub fn new(bin: impl Into<PathBuf>) -> Self {
        Self(Arc::new(CmdShared {
            bin: bin.into(),
            args: Vec::new(),
            echo_cmd: true,
            echo_err: true,
            stdin: None,
            current_dir: None,
        }))
    }

    /// Returns a command builder if there is some file available at `bin_path`.
    /// If there is no file at the given path returns `None`.
    /// Beware that this won't take `PATH` env variable into account.
    /// This function expects a relative or absolute filesystem path to the binary,
    /// and tries to check if there is some file there
    /// (retrying with `.exe` extension on windows).
    ///
    /// If you want to find a binary through `PATH`, you should use
    /// [`Cmd::lookup_in_path`]
    ///
    /// [`Cmd::lookup_in_path`]: struct.Cmd.html#method.lookup_in_path
    pub fn try_at(bin_path: impl Into<PathBuf>) -> Option<Self> {
        let bin: PathBuf = bin_path.into();
        let with_extension = match env::consts::EXE_EXTENSION {
            "" => None,
            it if bin.extension().is_none() => Some(bin.with_extension(it)),
            _ => None,
        };
        iter::once(bin)
            .chain(with_extension)
            .find(|it| it.is_file())
            .map(Self::new)
    }

    /// Returns a command builder for the given `bin_name` only if the this
    /// `bin_name` is accessible trough `PATH` env variable, otherwise returns `None`
    pub fn lookup_in_path(bin_name: &str) -> Option<Self> {
        let paths = env::var_os("PATH").unwrap_or_default();
        env::split_paths(&paths)
            .map(|path| path.join(bin_name))
            .find_map(Self::try_at)
    }

    fn as_mut(&mut self) -> &mut CmdShared {
        // Clone-on-write is so easy to do with `Arc` :D
        Arc::make_mut(&mut self.0)
    }

    /// Set binary path, overwrites the path that was set before.
    pub fn bin(&mut self, bin: impl Into<PathBuf>) -> &mut Self {
        self.as_mut().bin = bin.into();
        self
    }

    /// Set the current directory for the child process.
    ///
    /// Inherits this process current dir by default.
    pub fn current_dir(&mut self, dir: impl Into<PathBuf>) -> &mut Self {
        self.as_mut().current_dir = Some(dir.into());
        self
    }

    /// When set to `true` the command with its arguments will be logged to `stderr`.
    /// The command's output will also be logged to `stderr`.
    ///
    /// Default: `true`
    pub fn echo_cmd(&mut self, yes: bool) -> &mut Self {
        self.as_mut().echo_cmd = yes;
        self
    }

    /// When set to `true` the invocation error will be logged to `stderr`.
    /// Set it to `false` if non-zero exit code is an expected/recoverable error
    /// which doesn't need to be logged.
    ///
    /// Default: `true`
    pub fn echo_err(&mut self, yes: bool) -> &mut Self {
        self.as_mut().echo_err = yes;
        self
    }

    /// Sets the string input passed to child process's `stdin`.
    /// This overwrites the previous value.
    ///
    /// Use [`Cmd::stdin_bytes`] if you need to pass non-utf8 byte sequences.
    ///
    /// Nothing is written to `stdin` by default.
    ///
    /// [`Cmd::stdin_bytes`]: struct.Cmd.html#method.stdin_bytes
    pub fn stdin(&mut self, stdin: impl Into<String>) -> &mut Self {
        self.as_mut().stdin = Some(BinOrUtf8::Utf8(stdin.into()));
        self
    }

    /// Sets the bytes input passed to child process's `stdin`.
    /// This overwrites the previous value.
    ///
    /// Nothing is written to `stdin` by default.
    pub fn stdin_bytes(&mut self, stdin: Vec<u8>) -> &mut Self {
        self.as_mut().stdin = Some(BinOrUtf8::Bin(stdin));
        self
    }

    /// Same as `cmd.arg(arg1).arg(arg2)`. This is just a convenient shortcut
    /// mostly used to lexically group related arguments (for example named arguments).
    pub fn arg2(&mut self, arg1: impl Into<OsString>, arg2: impl Into<OsString>) -> &mut Self {
        self.arg(arg1).arg(arg2)
    }

    /// Appends a single argument to the list of arguments passed to the child process.
    pub fn arg(&mut self, arg: impl Into<OsString>) -> &mut Self {
        self.as_mut().args.push(arg.into());
        self
    }

    /// Replaces the argument at the given index with a new value.
    ///
    /// # Panics
    /// Panics if the given index is out of range of the arguments already set
    /// on this command builder.
    pub fn replace_arg(&mut self, idx: usize, arg: impl Into<OsString>) -> &mut Self {
        self.as_mut().args[idx] = arg.into();
        self
    }

    /// Extends the array of arguments passed to the child process with `args`.
    pub fn args<I>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator,
        I::Item: Into<OsString>,
    {
        self.as_mut().args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Same as `cmd.spawn()?.wait()`
    /// See [`ChildProcess::wait`] for details.
    ///
    /// [`ChildProcess::wait`]: struct.ChildProcess.html#method.wait
    pub fn run(&self) -> Result<()> {
        self.spawn()?.wait()?;
        Ok(())
    }

    /// Same as `cmd.spawn_piped()?.read()`
    /// See [`ChildProcess::read`] for details.
    ///
    /// [`ChildProcess::read`]: struct.ChildProcess.html#method.read
    pub fn read(&self) -> Result<String> {
        self.spawn_piped()?.read()
    }

    /// Same as `cmd.spawn_piped()?.read_bytes()`
    /// See [`ChildProcess::read_bytes`] for details.
    ///
    /// [`ChildProcess::read_bytes`]: struct.ChildProcess.html#method.read_bytes
    pub fn read_bytes(&self) -> Result<Vec<u8>> {
        self.spawn_piped()?.read_bytes()
    }

    /// Spawns a child process returning a handle to it.
    /// The child inherits both `stdout` and `stderr`.
    /// See the docs for [`ChildProcess`] for more details.
    /// Note that reading the child process output streams will panic!
    /// If you want to read the output, see [`Cmd::spawn_piped`]
    ///
    /// [`ChildProcess`]: struct.ChildProcess.html
    /// [`Cmd::spawn_piped`]: struct.Cmd.html#method.spawn_piped
    pub fn spawn(&self) -> Result<ChildProcess> {
        self.spawn_with(Stdio::inherit())
    }

    /// Spawns a child process returning a handle to it.
    /// Child's `stdout` will be piped for further reading from it, but
    /// `stderr` will be inherited.
    /// See the docs for [`ChildProcess`] for more details.
    ///
    /// [`ChildProcess`]: struct.ChildProcess.html
    pub fn spawn_piped(&self) -> Result<ChildProcess> {
        self.spawn_with(Stdio::piped())
    }

    fn spawn_with(&self, stdout: Stdio) -> Result<ChildProcess> {
        let mut cmd = std::process::Command::new(&self.0.bin);
        cmd.args(&self.0.args)
            .stderr(Stdio::inherit())
            .stdout(stdout);

        if let Some(dir) = &self.0.current_dir {
            cmd.current_dir(dir);
        }

        let child = match &self.0.stdin {
            None => cmd.stdin(Stdio::null()).spawn().cmd_context(self)?,
            Some(_) => {
                cmd.stdin(Stdio::piped());
                cmd.spawn().cmd_context(self)?
            }
        };

        let mut child = ChildProcess {
            cmd: Cmd(Arc::clone(&self.0)),
            child,
        };

        if self.0.echo_cmd {
            eprintln!("{}", child);
        }

        if let Some(stdin) = &self.0.stdin {
            child
                .child
                .stdin
                .take()
                .unwrap()
                .write_all(stdin.as_ref())
                .cmd_context(self)?;
        }
        Ok(child)
    }

    fn bin_name(&self) -> Cow<'_, str> {
        self.0
            .bin
            .components()
            .next()
            .expect("Binary name must not be empty")
            .as_os_str()
            .to_string_lossy()
    }
}

/// Wraps [`std::process::Child`], kills and waits for the process on [`Drop`].
/// It will log the fact that [`std::process::Child::kill`] was called in [`Drop`].
/// You should use wait for the process to finish with any of the available
/// methods if you want to handle the error, otherwise it will be ignored.
///
/// Beware that [`ChildProcess`] holds an invariant that is not propagated to the
/// type system. The invariant is that if [`ChildProcess`] was not spawned via
/// [`Cmd::spawn_piped`], then any methods that read the child's `stdout` will panic.
///
/// [`ChildProcess`]: struct.ChildProcess.html
/// [`Cmd::spawn_piped`]: struct.Cmd.html#method.spawn_piped
/// [`Drop`]: https://doc.rust-lang.org/std/ops/trait.Drop.html
/// [`std::process::Child`]: https://doc.rust-lang.org/std/process/struct.Child.html
/// [`std::process::Child::kill`]: https://doc.rust-lang.org/std/process/struct.Child.html#method.kill
pub struct ChildProcess {
    cmd: Cmd,
    child: std::process::Child,
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(None) => {
                eprintln!("[KILL {}] {}", self.child.id(), self.cmd.bin_name());
                let _ = self.child.kill();
                self.child.wait().unwrap_or_else(|err| {
                    panic!("Failed to wait for process: {}\nProcess: {}", err, self);
                });
            }
            // Already exited, no need for murder
            Ok(Some(_status)) => {}
            Err(err) => panic!("Failed to collect process exit status: {}", err),
        }
    }
}

impl fmt::Display for ChildProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let id = self.child.id();
        write!(f, "[PID {}] {}", id, self.cmd)
    }
}

impl ChildProcess {
    /// Waits for the process to finish. Returns an error if the process has
    /// finished with non-zero exit code.
    ///
    /// You should use this method for processes spawned via [`Cmd::spawn`]
    /// since the output of the command won't be read and returned,
    /// but just written to this process's `stdout` (as `stdout` is inherited
    /// with [`Cmd::spawn`])
    ///
    /// [`Cmd::echo_cmd`]: struct.Cmd.html#method.echo_cmd
    /// [`Cmd::spawn`]: struct.Cmd.html#method.spawn
    pub fn wait(&mut self) -> Result<()> {
        let exit_status = self.child.wait().proc_context(self)?;

        if !exit_status.success() {
            return Err(Error::proc(
                &self,
                &format_args!("Non-zero exit code: {}", exit_status),
            ));
        }
        Ok(())
    }

    /// Same as [`ChildProcess::read`] but reads any bytes sequence from the
    /// child process `stdout`.
    ///
    /// # Panics
    /// Same as for [`ChildProcess::read`].
    ///
    /// [`ChildProcess::read`]: struct.ChildProcess.html#method.read
    pub fn read_bytes(self) -> Result<Vec<u8>> {
        match self.read_impl(false)? {
            BinOrUtf8::Utf8(_) => unreachable!(),
            BinOrUtf8::Bin(it) => Ok(it),
        }
    }

    /// Waits for the process to finish and returns all that it has written
    /// to `stdout`. Returns an error if the process has finished with
    /// non-zero exit code. Expects a valid utf8 bytes sequence (since it returns
    /// a Rust [`String`]), if the process is not guaranteed to output valid utf8
    /// you might want to use [`ChildProcess::read_bytes`] instead.
    ///
    /// If [`Cmd::echo_cmd`] has been set to `true` then prints captured output to
    /// `stderr`.
    ///
    /// # Panics
    /// Panics if the process was spawned with non-piped `stdout`.
    /// This method is expected to be used only for processes spawned via
    /// [`Cmd::spawn_piped`].
    ///
    /// [`String`]: https://doc.rust-lang.org/std/string/struct.String.html
    /// [`ChildProcess::read_bytes`]: struct.ChildProcess.html#method.read_bytes
    /// [`Cmd::echo_cmd`]: struct.Cmd.html#method.echo_cmd
    /// [`Cmd::spawn_piped`]: struct.Cmd.html#method.spawn_piped
    pub fn read(self) -> Result<String> {
        match self.read_impl(true)? {
            BinOrUtf8::Utf8(it) => Ok(it),
            BinOrUtf8::Bin(_) => unreachable!(),
        }
    }

    fn read_impl(mut self, expect_utf8: bool) -> Result<BinOrUtf8> {
        let stdout = {
            let stdout = self
                .child
                .stdout
                .as_mut()
                .expect("use spawn_piped() to capture stdout instead of spawn()");
            if expect_utf8 {
                let mut out = String::new();
                stdout.read_to_string(&mut out).proc_context(&self)?;
                BinOrUtf8::Utf8(out)
            } else {
                let mut out = Vec::new();
                stdout.read_to_end(&mut out).proc_context(&self)?;
                BinOrUtf8::Bin(out)
            }
        };

        self.wait()?;

        if self.cmd.0.echo_cmd {
            eprintln!("[STDOUT {}] {}", self.cmd.bin_name(), &stdout);
        }
        Ok(stdout)
    }

    /// Returns an iterator over the lines of data output to `stdout` by the child process.
    /// Beware that the iterator buffers the output, thus when the it is
    /// dropped the buffered data will be discarded and following reads
    /// won't restore it.
    ///
    /// # Panics
    /// Panics if some [`std::io::Error`] happens during the reading.
    /// All invariants from [`ChildProcess::read_bytes`] apply here too.
    ///
    /// [`ChildProcess::read`]: struct.ChildProcess.html#method.read
    /// [`std::io::Error`]: https://doc.rust-lang.org/std/io/struct.Error.html
    pub fn stdout_lines(&mut self) -> impl Iterator<Item = String> + '_ {
        let echo = self.cmd.0.echo_cmd;
        let id = self.child.id();
        let bin_name = self.cmd.bin_name();
        let stdout = io::BufReader::new(self.child.stdout.as_mut().unwrap());
        stdout
            .lines()
            .map(|line| line.expect("Unexpected io error"))
            .inspect(move |line| {
                if echo {
                    eprintln!("[{} {}] {}", id, bin_name, line);
                }
            })
    }
}
