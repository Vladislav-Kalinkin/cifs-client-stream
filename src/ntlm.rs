mod auth;
mod buffer;
mod challenge;
mod error;
mod init;
mod packet;

pub use auth::Auth;
pub use challenge::ChallengeMsg;
pub use error::Error;
pub use init::{Flags, InitMsg};

const NTLMSSP_MAGIC: &[u8] = b"NTLMSSP\0";
const NTLM_MSG_INIT: u32 = 1;
const NTLM_MSG_CHALLENGE: u32 = 2;
const NTLM_MSG_RESPONSE: u32 = 3;
const NTLM_REVISION: u8 = 15;
