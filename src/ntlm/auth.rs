use bytes::{BufMut, Bytes, BytesMut};
use hex_literal::hex;
use rand::{rngs::OsRng, RngCore};

use super::{ChallengeMsg, Error};
use crate::utils;

#[derive(Debug, Clone)]
pub struct Auth {
    pub user: String,
    pub workstation: String,
    pub domain: String,
    pub password: String,
}

impl Auth {
    pub fn new(user: &str, workstation: &str, domain: &str, password: &str) -> Self {
        Auth {
            user: user.to_owned(),
            workstation: workstation.to_owned(),
            domain: domain.to_owned(),
            password: password.to_owned(),
        }
    }

    pub(crate) fn ntlmv1_authenticate(&self, challenge: &[u8]) -> [u8; 24] {
        let mut result: [u8; 24] = [0; 24];

        let hash = self.ntlm_hash();
        utils::des_oneshot(&hash[0..7], challenge, &mut result[0..8]);
        utils::des_oneshot(&hash[7..14], challenge, &mut result[8..16]);
        utils::des_oneshot(&hash[14..16], challenge, &mut result[16..24]);

        result
    }

    pub(crate) fn ntlmv2_authenticate(
        &self,
        challenge_msg: &ChallengeMsg,
    ) -> Result<([u8; 16], Bytes), Error> {
        let mut random = [0u8; 8];
        OsRng.fill_bytes(&mut random);

        // generate blob
        let mut blob = BytesMut::with_capacity(1024);
        blob.put(hex!("0101000000000000").as_slice());
        blob.put_u64_le(utils::get_windows_time());
        blob.put(random.as_slice());
        blob.put_u32_le(0);
        blob.extend_from_slice(&challenge_msg.info[..]);
        blob.put_u32_le(0);

        let mut data = BytesMut::with_capacity(challenge_msg.challenge.len() + blob.len());
        data.put(&challenge_msg.challenge[..]);
        data.put(&blob[..]);

        let key = self.ntlmv2_hash();
        let tag = utils::hmac_md5_oneshot(&key, &data);
        let sk = utils::hmac_md5_oneshot(&key, &tag);

        let mut result = BytesMut::with_capacity(tag.len() + blob.len());
        result.put(tag.as_slice());
        result.put(blob);

        Ok((sk, result.freeze()))
    }

    fn ntlm_hash(&self) -> [u8; 16] {
        let unicoded = utils::encode_utf16le(&self.password);
        utils::md4_oneshot(&unicoded)
    }

    fn ntlmv2_hash(&self) -> [u8; 16] {
        let key = self.ntlm_hash();
        let data = utils::encode_utf16le(&(self.user.to_uppercase() + &self.domain));

        utils::hmac_md5_oneshot(&key, &data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex_literal::hex;

    #[test]
    fn ntlm_hash() {
        let auth = Auth::new("user", "workstation", "DOMAIN", "SecREt01");
        let hash = auth.ntlm_hash();

        assert_eq!(hash, hex!("cd06ca7c7e10c99b1d33b7485a2ed808"));
    }

    #[test]
    fn ntlmv2_hash() {
        let auth = Auth::new("user", "workstation", "DOMAIN", "SecREt01");
        let hash = auth.ntlmv2_hash();

        assert_eq!(hash, hex!("04b8e0ba74289cc540826bab1dee63ae"));
    }
}
