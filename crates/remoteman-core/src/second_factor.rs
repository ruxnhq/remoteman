//! The second factor for a paired device: a code from an authenticator app (TOTP, RFC 6238 with
//! SHA-1, six digits, 30 seconds), asked for on top of the device's key when the host's owner
//! turns it on (docs/PROTOCOL.md, "Second factor"). The store lives in
//! `<state>/host/second_factor.json`; a host with no such file has it off.

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const STEP: u64 = 30;
/// Wrong codes one device may send in [`WINDOW`] before it is refused, and all devices together.
pub const TRIES_PER_DEVICE: u32 = 5;
pub const TRIES_OVERALL: u32 = 20;
/// The window wrong codes are counted in, and how long a device is refused after too many.
pub const WINDOW: u64 = 600;
pub const REFUSED_FOR: u64 = 600;
/// How long "don't ask again on this device" lasts.
pub const REMEMBER_FOR: u64 = 30 * 24 * 3600;

/// Seconds since 1970, for the callers that have no reason to fake the time.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---- TOTP --------------------------------------------------------------------------------------

fn hotp(secret: &[u8], counter: u64) -> u32 {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = ring::hmac::sign(&key, &counter.to_be_bytes());
    let h = tag.as_ref();
    let o = (h[19] & 0xf) as usize;
    u32::from_be_bytes([h[o] & 0x7f, h[o + 1], h[o + 2], h[o + 3]])
}

/// The six digits for `secret` at `unix` seconds (RFC 6238: the RFC's vectors are eight digits
/// of the same number).
pub fn totp(secret: &[u8], unix: u64) -> u32 {
    hotp(secret, unix / STEP) % 1_000_000
}

/// Whether `code` is the right one now, give or take a step (a clock a little off, a code typed
/// slowly).
pub fn verify(secret: &[u8], code: &str, unix: u64) -> bool {
    let code = code.trim();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let step = unix / STEP;
    let mut ok = false;
    // (All three are always computed, whatever the first said.)
    for s in [step.saturating_sub(1), step, step + 1] {
        ok |= format!("{:06}", hotp(secret, s) % 1_000_000) == code;
    }
    ok
}

const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32 without padding, as authenticator apps take it.
pub fn base32(bytes: &[u8]) -> String {
    let (mut out, mut buf, mut bits) = (String::new(), 0u32, 0);
    for &b in bytes {
        buf = buf << 8 | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(B32[(buf >> bits & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(B32[(buf << (5 - bits) & 31) as usize] as char);
    }
    out
}

pub fn unbase32(text: &str) -> Option<Vec<u8>> {
    let (mut out, mut buf, mut bits) = (Vec::new(), 0u32, 0);
    for c in text.bytes().filter(|c| !matches!(c, b' ' | b'-' | b'=')) {
        let v = B32.iter().position(|&x| x == c.to_ascii_uppercase())? as u32;
        buf = buf << 5 | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The secret in groups of four, for typing into an app by hand.
pub fn grouped(secret: &str) -> String {
    secret.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or("")).collect::<Vec<_>>().join(" ")
}

fn percent(text: &str) -> String {
    text.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// What an authenticator app scans or opens.
pub fn otpauth_uri(host_name: &str, secret: &str) -> String {
    format!("otpauth://totp/remoteman:{}?secret={secret}&issuer=remoteman", percent(host_name))
}

// ---- The store ----------------------------------------------------------------------------------

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct Data {
    /// Base32. Present from `enable` on; the factor is asked for only once `enabled`.
    #[serde(default)]
    secret: String,
    #[serde(default)]
    enabled: bool,
    /// Fingerprint to the time (unix seconds) the device need not be asked until.
    #[serde(default)]
    remembered: BTreeMap<String, u64>,
}

#[derive(Default)]
struct Tries {
    /// When each device sent a wrong code (within [`WINDOW`]).
    wrong: BTreeMap<String, Vec<u64>>,
    all: Vec<u64>,
    /// Refused until this time.
    refused: BTreeMap<String, u64>,
    all_refused: u64,
}

/// What came of a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Right,
    /// Wrong; this many tries are left.
    Wrong(u32),
    /// Too many wrong codes: this device is refused for a while.
    Refused,
}

pub struct SecondFactorStore {
    path: Option<PathBuf>,
    data: Mutex<Data>,
    tries: Mutex<Tries>,
}

impl SecondFactorStore {
    pub fn in_memory() -> Self {
        Self { path: None, data: Mutex::default(), tries: Mutex::default() }
    }

    /// A missing or unreadable file means the second factor is off.
    pub fn load(path: &Path) -> Self {
        let data = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Self { path: Some(path.to_owned()), data: Mutex::new(data), tries: Mutex::default() }
    }

    pub fn enabled(&self) -> bool {
        self.data.lock().unwrap().enabled
    }

    /// How many devices are remembered (not yet expired at `now`).
    pub fn remembered(&self, now: u64) -> usize {
        self.data.lock().unwrap().remembered.values().filter(|&&t| t > now).count()
    }

    /// Whether this device must give a code to be let in at `now`.
    pub fn required_for(&self, fingerprint: &str, now: u64) -> bool {
        let d = self.data.lock().unwrap();
        d.enabled && d.remembered.get(fingerprint).is_none_or(|&t| t <= now)
    }

    /// Whether this device is refused at the moment (too many wrong codes).
    pub fn is_refused(&self, fingerprint: &str, now: u64) -> bool {
        let t = self.tries.lock().unwrap();
        t.all_refused > now || t.refused.get(fingerprint).is_some_and(|&u| u > now)
    }

    /// Make a new secret and keep it, not yet asked for; [`Self::confirm`] turns the factor on.
    /// Fails when it is on already (turn it off first: replacing the secret would lock the
    /// owner's app out).
    pub fn enable(&self) -> Result<String> {
        let mut d = self.data.lock().unwrap();
        if d.enabled {
            bail!("the second factor is on already; turn it off first");
        }
        let mut raw = [0u8; 20];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut raw).map_err(|_| anyhow::anyhow!("no random numbers"))?;
        d.secret = base32(&raw);
        d.remembered.clear();
        self.save(&d)?;
        Ok(d.secret.clone())
    }

    /// The code the app shows is right: the factor is on from now.
    pub fn confirm(&self, code: &str, now: u64) -> Result<bool> {
        let mut d = self.data.lock().unwrap();
        let Some(secret) = unbase32(&d.secret).filter(|s| !s.is_empty()) else { return Ok(false) };
        if d.enabled || !verify(&secret, code, now) {
            return Ok(false);
        }
        d.enabled = true;
        self.save(&d)?;
        Ok(true)
    }

    pub fn disable(&self) -> Result<()> {
        let mut d = self.data.lock().unwrap();
        *d = Data::default();
        self.save(&d)
    }

    pub fn forget_remembered(&self) -> Result<()> {
        let mut d = self.data.lock().unwrap();
        d.remembered.clear();
        self.save(&d)
    }

    /// A device was revoked: it is no longer remembered.
    pub fn forget_device(&self, fingerprint: &str) -> Result<()> {
        let mut d = self.data.lock().unwrap();
        if d.remembered.remove(fingerprint).is_some() {
            self.save(&d)?;
        }
        Ok(())
    }

    /// Check one code from a device at `now`, counting it if it's wrong. With `remember`, a right
    /// code lets the device in without one for [`REMEMBER_FOR`].
    pub fn check(&self, fingerprint: &str, code: &str, remember: bool, now: u64) -> Check {
        if self.is_refused(fingerprint, now) {
            return Check::Refused;
        }
        let right = {
            let d = self.data.lock().unwrap();
            d.enabled && unbase32(&d.secret).is_some_and(|s| verify(&s, code, now))
        };
        let mut t = self.tries.lock().unwrap();
        if right {
            t.wrong.remove(fingerprint);
            drop(t);
            if remember {
                let mut d = self.data.lock().unwrap();
                d.remembered.retain(|_, &mut u| u > now);
                d.remembered.insert(fingerprint.to_owned(), now + REMEMBER_FOR);
                let _ = self.save(&d);
            }
            return Check::Right;
        }
        t.all.retain(|&w| w + WINDOW > now);
        t.all.push(now);
        let mine = t.wrong.entry(fingerprint.to_owned()).or_default();
        mine.retain(|&w| w + WINDOW > now);
        mine.push(now);
        let (mine, all) = (mine.len() as u32, t.all.len() as u32);
        if all >= TRIES_OVERALL {
            t.all_refused = now + REFUSED_FOR;
            t.all.clear();
            return Check::Refused;
        }
        if mine >= TRIES_PER_DEVICE {
            t.refused.insert(fingerprint.to_owned(), now + REFUSED_FOR);
            t.wrong.remove(fingerprint);
            return Check::Refused;
        }
        Check::Wrong((TRIES_PER_DEVICE - mine).min(TRIES_OVERALL - all))
    }

    fn save(&self, data: &Data) -> Result<()> {
        if let Some(path) = &self.path {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_string_pretty(data)?).with_context(|| format!("write {}", tmp.display()))?;
            std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn secret(&self) -> String {
        self.data.lock().unwrap().secret.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC: &[u8] = b"12345678901234567890";

    #[test]
    fn rfc_6238_vectors() {
        // The RFC's SHA-1 vectors (eight digits there; the six we use are their last six).
        for (time, eight) in [(59u64, 94287082u32), (1111111109, 7081804), (1111111111, 14050471), (1234567890, 89005924), (2000000000, 69279037), (20000000000, 65353130)] {
            assert_eq!(totp(RFC, time), eight % 1_000_000, "at {time}");
        }
    }

    #[test]
    fn one_step_either_way() {
        let code = format!("{:06}", totp(RFC, 1111111109));
        assert!(verify(RFC, &code, 1111111109));
        assert!(verify(RFC, &code, 1111111109 + 30));
        assert!(verify(RFC, &code, 1111111109 - 30));
        assert!(!verify(RFC, &code, 1111111109 + 90));
        assert!(!verify(RFC, "12345", 59));
        assert!(!verify(RFC, "abcdef", 59));
    }

    #[test]
    fn base32_round_trip() {
        assert_eq!(base32(RFC), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(unbase32("gezd gnbv-GY3TQOJQGEZDGNBVGY3TQOJQ").unwrap(), RFC);
        assert_eq!(grouped("GEZDGNBVGY"), "GEZD GNBV GY");
        assert!(unbase32("0189").is_none());
        assert_eq!(otpauth_uri("My PC", "AB"), "otpauth://totp/remoteman:My%20PC?secret=AB&issuer=remoteman");
    }

    fn on(store: &SecondFactorStore, now: u64) {
        let secret = store.enable().unwrap();
        assert!(!store.enabled() && !store.required_for("a", now), "not asked before a code confirms it");
        assert!(!store.confirm("000000", now).unwrap() || verify(&unbase32(&secret).unwrap(), "000000", now));
        let code = format!("{:06}", totp(&unbase32(&secret).unwrap(), now));
        assert!(store.confirm(&code, now).unwrap());
        assert!(store.enabled());
        assert!(store.enable().is_err(), "can't replace a secret that's in use");
    }

    /// A code that is right at any time within the next two hours.
    fn wrong(store: &SecondFactorStore, now: u64) -> String {
        let secret = unbase32(&store.secret()).unwrap();
        (0..1_000_000u32).map(|n| format!("{n:06}")).find(|c| (0..400).all(|i| !verify(&secret, c, now + i * 30))).unwrap()
    }

    fn code(store: &SecondFactorStore, now: u64) -> String {
        format!("{:06}", totp(&unbase32(&store.secret()).unwrap(), now))
    }

    #[test]
    fn lockout_counts_and_expires() {
        let s = SecondFactorStore::in_memory();
        on(&s, 1000);
        // (A code that is never right: one step far away from now.)
        let bad = wrong(&s, 1000);
        for left in [4, 3, 2, 1] {
            assert_eq!(s.check("a", &bad, false, 1000), Check::Wrong(left));
        }
        assert_eq!(s.check("a", &bad, false, 1000), Check::Refused);
        assert!(s.is_refused("a", 1000 + 599));
        assert_eq!(s.check("a", &code(&s, 1000), false, 1001), Check::Refused, "even the right code, while refused");
        assert!(!s.is_refused("a", 1000 + 601));
        assert_eq!(s.check("a", &code(&s, 1700), false, 1700), Check::Right);
        // Another device isn't refused by this one's mistakes.
        assert_eq!(s.check("b", &bad, false, 1700), Check::Wrong(4));
        // Old mistakes stop counting after ten minutes.
        assert_eq!(s.check("b", &bad, false, 1700 + 601), Check::Wrong(4));
    }

    #[test]
    fn twenty_overall_refuse_everyone() {
        let s = SecondFactorStore::in_memory();
        on(&s, 5000);
        let bad = wrong(&s, 5000);
        let mut last = Check::Right;
        for i in 0..20 {
            last = s.check(&format!("dev{i}"), &bad, false, 5000);
        }
        assert_eq!(last, Check::Refused);
        assert!(s.is_refused("someone new", 5001));
        assert!(!s.is_refused("someone new", 5000 + 601));
    }

    #[test]
    fn remember_expires_and_is_forgotten() {
        let s = SecondFactorStore::in_memory();
        on(&s, 1000);
        assert!(s.required_for("a", 1000));
        assert_eq!(s.check("a", &code(&s, 1000), true, 1000), Check::Right);
        assert!(!s.required_for("a", 1000 + REMEMBER_FOR - 1));
        assert!(s.required_for("a", 1000 + REMEMBER_FOR));
        assert!(s.required_for("b", 1000), "another device is still asked");
        assert_eq!(s.remembered(1000), 1);
        s.forget_remembered().unwrap();
        assert!(s.required_for("a", 1001));
        s.disable().unwrap();
        assert!(!s.required_for("a", 1001) && !s.enabled());
    }

    #[test]
    fn store_round_trip_and_old_state() {
        let d = std::env::temp_dir().join(format!("rm-2fa-{}", std::process::id()));
        let path = d.join("host/second_factor.json");
        assert!(!SecondFactorStore::load(&path).enabled(), "no file: off");
        let s = SecondFactorStore::load(&path);
        on(&s, 1000);
        assert_eq!(s.check("a", &code(&s, 1000), true, 1000), Check::Right);
        let again = SecondFactorStore::load(&path);
        assert!(again.enabled() && !again.required_for("a", 2000) && again.required_for("b", 2000));
        assert_eq!(again.secret(), s.secret());
        let _ = std::fs::remove_dir_all(&d);
    }
}
