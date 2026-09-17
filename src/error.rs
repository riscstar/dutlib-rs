use std::{backtrace::Backtrace, fmt, io};

/// An error from which the test harness cannot recover.
///
/// Functions and methods that can return recoverable errors should use a
/// custom error type (which can wrap [`UnrecoverableError`] if needed
/// to handle unrecoverable cases).
///
/// Every error carries a [`Backtrace`] captured at the point it was
/// constructed, which is useful for tracking down where an abort originated.
/// This is largely to assist in the conversion of unrecoverable errors into a
/// recoverable form when this is appropriate.
///
/// [`CommandExecutor`]: crate::CommandExecutor
#[derive(Debug)]
pub struct UnrecoverableError {
    kind: UnrecoverableErrorKind,
    backtrace: Backtrace,
}

/// The kind of an [`UnrecoverableError`].
#[derive(Debug)]
pub enum UnrecoverableErrorKind {
    /// A timeout was reached while waiting for expected output.
    ExpectTimeout,
    /// The session reached EOF.
    Eof,
    /// An I/O error.
    Io(io::Error),
    /// Any other error reported by `expectrl`.
    Expectrl(expectrl::Error),
    /// A failure detected by the test harness itself, described by a message.
    ///
    /// This is used for problems such as a test reporting failures, a
    /// configuration error or command output that could not be parsed.
    Message(String),
}

impl UnrecoverableError {
    /// Create an error of the given `kind`, capturing a backtrace.
    pub fn new(kind: UnrecoverableErrorKind) -> Self {
        Self {
            kind,
            backtrace: Backtrace::force_capture(),
        }
    }

    /// Create a [`UnrecoverableErrorKind::Message`] error, capturing a backtrace.
    pub fn message(msg: impl Into<String>) -> Self {
        Self::new(UnrecoverableErrorKind::Message(msg.into()))
    }

    /// The kind of this error.
    pub fn kind(&self) -> &UnrecoverableErrorKind {
        &self.kind
    }

    /// The backtrace captured when this error was created.
    pub fn backtrace(&self) -> &Backtrace {
        &self.backtrace
    }
}

impl fmt::Display for UnrecoverableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(f)
    }
}

impl fmt::Display for UnrecoverableErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnrecoverableErrorKind::ExpectTimeout => {
                write!(f, "Reached a timeout for expect type of command")
            }
            UnrecoverableErrorKind::Eof => {
                write!(f, "EOF was reached; the read may succeed later")
            }
            UnrecoverableErrorKind::Io(e) => write!(f, "{e}"),
            UnrecoverableErrorKind::Expectrl(e) => write!(f, "{e}"),
            UnrecoverableErrorKind::Message(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for UnrecoverableError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.kind.source()
    }
}

impl std::error::Error for UnrecoverableErrorKind {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            UnrecoverableErrorKind::Io(e) => Some(e),
            UnrecoverableErrorKind::Expectrl(e) => Some(e),
            UnrecoverableErrorKind::ExpectTimeout
            | UnrecoverableErrorKind::Eof
            | UnrecoverableErrorKind::Message(_) => None,
        }
    }
}

/// An error returned by a test run.
///
/// This distinguishes between a run that completed but reported test failures
/// (which is not an unrecoverable condition) and a run that could not be
/// completed at all.
#[derive(Debug)]
pub enum RunTestError {
    /// The test run completed, but reported the given number of failures.
    CompleteWithFailures(u32),
    /// The test run could not be completed.
    Unrecoverable(UnrecoverableError),
}

impl fmt::Display for RunTestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunTestError::CompleteWithFailures(n) => {
                write!(f, "Test run completed with {n} failures")
            }
            RunTestError::Unrecoverable(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for RunTestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RunTestError::CompleteWithFailures(_) => None,
            RunTestError::Unrecoverable(e) => Some(e),
        }
    }
}

impl From<UnrecoverableError> for RunTestError {
    fn from(e: UnrecoverableError) -> Self {
        RunTestError::Unrecoverable(e)
    }
}

impl From<io::Error> for RunTestError {
    fn from(e: io::Error) -> Self {
        RunTestError::Unrecoverable(e.into())
    }
}

impl From<io::Error> for UnrecoverableError {
    fn from(e: io::Error) -> Self {
        Self::new(UnrecoverableErrorKind::Io(e))
    }
}

impl From<expectrl::Error> for UnrecoverableError {
    fn from(e: expectrl::Error) -> Self {
        Self::new(match e {
            expectrl::Error::IO(e) => UnrecoverableErrorKind::Io(e),
            expectrl::Error::ExpectTimeout => UnrecoverableErrorKind::ExpectTimeout,
            expectrl::Error::Eof => UnrecoverableErrorKind::Eof,
            e => UnrecoverableErrorKind::Expectrl(e),
        })
    }
}
