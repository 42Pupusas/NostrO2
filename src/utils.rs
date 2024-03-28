#[cfg(not(target_arch = "wasm32"))]
use std::time::SystemTime;

#[cfg(target_arch = "wasm32")]
use web_time::SystemTime;



use rand::{thread_rng, Rng};
use secp256k1::SecretKey;

#[cfg(target_arch = "wasm32")]
use rustls_pki_types::UnixTime;

pub fn new_keys() -> SecretKey {
    let mut rng = thread_rng();

    // Generate a random 256-bit integer as the private key
    let private_key: [u8; 32] = rng.gen();

    // Convert the private key to a secp256k1 SecretKey object
    let secret_key = SecretKey::from_slice(&private_key).unwrap();

    // Return the private key in hexadecimal format
    secret_key
}

pub fn get_unix_timestamp() -> u64 {
    let now = SystemTime::now();

    // Convert it to a duration since the Unix epoch
    let duration_since_epoch = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("Time went backwards");

    // Create a UnixTime instance representing the current time
    #[cfg(target_arch = "wasm32")]
    let current_unix_time = UnixTime::since_unix_epoch(duration_since_epoch);
    
    #[cfg(not(target_arch = "wasm32"))]
    let current_unix_time = duration_since_epoch;

    current_unix_time.as_secs()
}
