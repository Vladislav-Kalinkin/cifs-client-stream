use std::fmt;
use std::io::ErrorKind as IoErrorKind;

use crate::netbios;
use crate::ntlm;
use crate::smb;
use crate::smb::info::{Cmd, Status};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Network,
    Timeout,
    Protocol,
    Server,
    Auth,
    Unsupported,
    Config,
    InvalidInput,
    Internal,
}

#[derive(Debug)]
pub enum Error {
    NetBios(netbios::Error),
    SMBError(smb::Error),
    NTLMError(ntlm::Error),
    InternalError(String),
    UnexpectedReply(Cmd, Cmd),
    TooManyReplies(usize),
    ServerError(Status),
    Unsupported(String),
    InvalidConfig(String),
    Timeout(String),
    InvalidUri,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::NetBios(error) => write!(f, "NetBios error: {}", error),
            Error::SMBError(err) => write!(f, "protocol error: {}", err),
            Error::NTLMError(err) => write!(f, "NTLM error: {}", err),
            Error::InternalError(what) => write!(f, "internal error: {}", what),
            Error::UnexpectedReply(want, got) => {
                write!(f, "unexpected reply, want: {:?}, got: {:?}", want, got)
            }
            Error::TooManyReplies(num) => write!(f, "we expect one reply but got {}", num),
            Error::ServerError(status) => write!(f, "server error: {}", status),
            Error::Unsupported(what) => write!(f, "unsupported feature: {}", what),
            Error::InvalidConfig(what) => write!(f, "invalid configuration: {}", what),
            Error::Timeout(what) => write!(f, "timeout: {}", what),
            Error::InvalidUri => write!(f, "URI is invalid"),
        }
    }
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Error::NetBios(error) => netbios_error_kind(error),
            Error::SMBError(error) => smb_error_kind(error),
            Error::NTLMError(error) => ntlm_error_kind(error),
            Error::InternalError(_) => ErrorKind::Internal,
            Error::UnexpectedReply(_, _) | Error::TooManyReplies(_) => ErrorKind::Protocol,
            Error::ServerError(_) => ErrorKind::Server,
            Error::Unsupported(_) => ErrorKind::Unsupported,
            Error::InvalidConfig(_) => ErrorKind::Config,
            Error::Timeout(_) => ErrorKind::Timeout,
            Error::InvalidUri => ErrorKind::InvalidInput,
        }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(self.kind(), ErrorKind::Network | ErrorKind::Timeout)
    }

    pub fn is_timeout(&self) -> bool {
        self.kind() == ErrorKind::Timeout
    }

    pub fn is_connection_lost(&self) -> bool {
        match self {
            Error::NetBios(netbios::Error::UnexpectedEOF) => true,
            Error::NetBios(netbios::Error::IoError(error))
            | Error::NTLMError(ntlm::Error::IO(error)) => is_connection_lost(error.kind()),
            _ => false,
        }
    }
}

impl From<netbios::Error> for Error {
    fn from(err: netbios::Error) -> Self {
        Error::NetBios(err)
    }
}

fn netbios_error_kind(error: &netbios::Error) -> ErrorKind {
    match error {
        netbios::Error::IoError(error) => io_error_kind(error.kind()),
        netbios::Error::UnexpectedEOF => ErrorKind::Network,
        netbios::Error::CreateSession(_)
        | netbios::Error::InvalidFrameType(_)
        | netbios::Error::InvalidFrame
        | netbios::Error::FrameTooBig
        | netbios::Error::UnexpectedFrame => ErrorKind::Protocol,
    }
}

fn smb_error_kind(error: &smb::Error) -> ErrorKind {
    match error {
        smb::Error::Unsupported(_) => ErrorKind::Unsupported,
        _ => ErrorKind::Protocol,
    }
}

fn ntlm_error_kind(error: &ntlm::Error) -> ErrorKind {
    match error {
        ntlm::Error::IO(error) => io_error_kind(error.kind()),
        ntlm::Error::NeedAuth => ErrorKind::Auth,
        ntlm::Error::InputParameter(_) => ErrorKind::InvalidInput,
        ntlm::Error::InvalidPacket => ErrorKind::Protocol,
    }
}

fn io_error_kind(kind: IoErrorKind) -> ErrorKind {
    match kind {
        IoErrorKind::TimedOut | IoErrorKind::WouldBlock => ErrorKind::Timeout,
        _ => ErrorKind::Network,
    }
}

fn is_connection_lost(kind: IoErrorKind) -> bool {
    matches!(
        kind,
        IoErrorKind::BrokenPipe
            | IoErrorKind::ConnectionAborted
            | IoErrorKind::ConnectionReset
            | IoErrorKind::NotConnected
            | IoErrorKind::UnexpectedEof
    )
}

impl From<smb::Error> for Error {
    fn from(err: smb::Error) -> Self {
        Error::SMBError(err)
    }
}

impl From<ntlm::Error> for Error {
    fn from(err: ntlm::Error) -> Self {
        Error::NTLMError(err)
    }
}

impl From<std::num::TryFromIntError> for Error {
    fn from(err: std::num::TryFromIntError) -> Self {
        Error::InternalError(format!("numeric conversion failed: {}", err))
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::{Error, ErrorKind};
    use crate::{netbios, smb};

    #[test]
    fn retryable_errors_are_limited_to_network_failures() {
        let reset = Error::NetBios(netbios::Error::IoError(io::Error::from(
            io::ErrorKind::ConnectionReset,
        )));
        let protocol = Error::SMBError(smb::Error::InvalidHeader);
        let config = Error::InvalidConfig("bad option".to_owned());
        let timeout = Error::Timeout("read".to_owned());

        assert_eq!(reset.kind(), ErrorKind::Network);
        assert!(reset.is_retryable());
        assert!(reset.is_connection_lost());
        assert!(!protocol.is_retryable());
        assert!(!config.is_retryable());
        assert!(timeout.is_retryable());
        assert!(timeout.is_timeout());
    }

    #[test]
    fn timeout_errors_are_retryable_and_distinct() {
        let error = Error::NetBios(netbios::Error::IoError(io::Error::from(
            io::ErrorKind::TimedOut,
        )));

        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert!(error.is_timeout());
        assert!(error.is_retryable());
        assert!(!error.is_connection_lost());
    }
}
