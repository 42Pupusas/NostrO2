use rand::{thread_rng, Rng};
use secp256k1::SecretKey;
use std::{time::{SystemTime, UNIX_EPOCH}};

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
    // Get the current time as a SystemTime object
    let current_time = SystemTime::now();

    // Get the duration between the current time and the Unix epoch
    let duration_since_epoch = current_time.duration_since(UNIX_EPOCH).unwrap();

    // Get the number of seconds since the Unix epoch as a u64 value
    let unix_timestamp = duration_since_epoch.as_secs();

    unix_timestamp
}
