extern crate nostro2;
use nostro2::notes::Note;
use nostro2::relays::{NostrRelay, RelayEvents};
use nostro2::userkeys::UserKeys;
use nostro2::utils::new_keys;
use serde_json::json;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_relay_events() {
        let mut relay_connection = NostrRelay::new("wss://relay.arrakis.lat").unwrap();

        let mut msg_count = 0;

        relay_connection
            .subscribe(json!({
                "kinds": [1],
                "limit": 10,
            }))
            .unwrap();
        while let Ok(event) = relay_connection.read_relay_events() {
            match event {
                RelayEvents::EVENT(_event, _id, _signed_note) => {
                    msg_count += 1;
                }
                RelayEvents::EOSE(_event, _notice) => {
                    break;
                }
                _ => {}
            }
        }
        assert!(msg_count > 0);
    }

    #[test]
    fn send_note() {
        let mut relay_connection = NostrRelay::new("wss://relay.arrakis.lat").unwrap();

        let user_keys = hex::encode(&new_keys()[..]);
        let keypair = UserKeys::new(&user_keys).unwrap();

        let note = Note::new(&keypair.get_public_key(), 1, "Hello, World!");

        let signednote = keypair.sign_nostr_event(note);

        relay_connection.send_note(signednote).unwrap();
        while let Ok(event) = relay_connection.read_relay_events() {
            match event {
                RelayEvents::OK(_event, _id, success, _notice) => {
                    assert_eq!(success, true);
                    break;
                }
                _ => {}
            }
        }
    }

    use std::sync::Mutex;
    #[test]
    fn use_relay_on_threads() {
        use std::thread;
        let relay_connection = Arc::new(Mutex::new(
            NostrRelay::new("wss://relay.arrakis.lat").unwrap(),
        ));

        let relay_clone2 = relay_connection.clone();

        let handle2 = thread::spawn(move || {
            println!("THREAD 2");
            relay_clone2
                .lock()
                .unwrap()
                .subscribe(json!({
                    "kinds": [1],
                    "limit": 10,
                }))
                .unwrap();
            while let Ok(event) = relay_clone2.lock().unwrap().read_relay_events() {
                match event {
                    RelayEvents::EVENT(_event, _id, _signed_note) => {
                        println!("EVENT {}", _signed_note);
                    }
                    RelayEvents::EOSE(_, _) => {
                        println!("End of THREAD 2");
                        break;
                    }
                    _ => {}
                }
            }
            return;
        });

        let relay_clone = relay_connection.clone();
        let handle = thread::spawn(move || {
            println!("THREAD 1");
            relay_clone.lock().unwrap().subscribe(json!({
                "kinds": [3],
                "limit": 10,
            })).unwrap();
            while let Ok(event) = relay_clone.lock().unwrap().read_relay_events() {
                match event {
                    RelayEvents::EVENT(_event, _id, _signed_note) => {
                        println!("EVENT 2 {}", _signed_note.get_kind());
                    }
                    RelayEvents::EOSE(_, _) => {
                        println!("End of THREAD 1");
                        break;
                    }
                    _ => {}
                }
            }
            return;
        });

        handle.join().unwrap();
        handle2.join().unwrap();
        assert!(true);
    }
}
