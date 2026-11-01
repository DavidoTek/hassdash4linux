//! Randomness helpers backed by the operating system's CSPRNG (via `ring`).

use base64::Engine;
use ring::rand::{SecureRandom, SystemRandom};

/// Fill a buffer with cryptographically secure random bytes.
pub fn fill(buf: &mut [u8]) {
    SystemRandom::new()
        .fill(buf)
        .expect("the operating system's random number generator failed");
}

/// A random URL-safe token with `bytes` bytes of entropy.
pub fn token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    fill(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// A random (version 4) UUID in its canonical textual form.
pub fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn uuid_format() {
        let u = super::uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert_ne!(super::uuid_v4(), u);
    }

    #[test]
    fn token_len() {
        assert_eq!(super::token(32).len(), 43);
    }
}
