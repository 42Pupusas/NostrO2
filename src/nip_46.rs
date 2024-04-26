use crate::{
    notes::{Note, SignedNote},
    userkeys::UserKeys,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug)]
pub enum Nip46Commands {
    Ping(String, String),
    SignEvent(String, String, Note),
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Nip46Response {
    id: String,
    result: String,
}

impl Nip46Response {
    pub fn get_response_note(signed_note: &SignedNote, user_keys: &UserKeys) -> SignedNote {
        let decrypted_note_response = user_keys.decrypt_note_content(signed_note);
        let response_note =
            serde_json::from_str::<Nip46Response>(&decrypted_note_response).unwrap();
        let parsed_note = serde_json::from_str::<SignedNote>(&response_note.result).unwrap();
        parsed_note
    }
}

impl ToString for Nip46Response {
    fn to_string(&self) -> String {
        serde_json::to_string(self).unwrap()
    }
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Nip46Request {
    id: String,
    method: String,
    params: Vec<String>,
}

impl Nip46Request {
    pub fn ping_request(client_keys: &UserKeys, user_keys: String) -> SignedNote {
        let random_id = format!("nostro2-{}", crate::utils::get_unix_timestamp());
        let ping_params = vec!["ping".to_string()];
        let self_try = Self {
            id: random_id,
            method: "ping".to_string(),
            params: ping_params,
        };
        self_try.sign_request(client_keys, user_keys)
    }

    pub fn sign_event_request(note_request: Note, client_keys: &UserKeys) -> SignedNote {
        let random_id = format!("nostro2-{}", crate::utils::get_unix_timestamp());
        let note_params = vec![note_request.to_string()];
        let self_try = Self {
            id: random_id,
            method: "sign_event".to_string(),
            params: note_params,
        };
        self_try.sign_request(client_keys, note_request.pubkey)
    }

    fn sign_request(&self, client_keys: &UserKeys, user_keys: String) -> SignedNote {
        let stringified_request = serde_json::to_string(&self).unwrap();
        let mut request_note =
            Note::new(&client_keys.get_public_key(), 24133, &stringified_request);
        request_note.add_pubkey_tag(&user_keys);
        client_keys.sign_encrypted_nostr_event(request_note, user_keys)
    }

    pub fn get_request_command(signed_note: &SignedNote, user_keys: &UserKeys) -> Nip46Commands {
        let command_pubkey = signed_note.get_pubkey().to_string();
        let decrypted_note_request = user_keys.decrypt_note_content(signed_note);
        let signed_request_note =
            serde_json::from_str::<Nip46Request>(&decrypted_note_request).unwrap();
        let command_id = signed_request_note.id;
        match signed_request_note.method.as_str() {
            "ping" => Nip46Commands::Ping(command_pubkey, command_id),
            "sign_event" => {
                let response_note =
                    serde_json::from_str::<Note>(&signed_request_note.params[0]).unwrap();
                Nip46Commands::SignEvent(command_pubkey, command_id, response_note)
            }
            _ => panic!("Unknown command"),
        }
    }

    pub fn respond_to_command(
        user_keys: &UserKeys,
        command: Nip46Commands,
    ) -> SignedNote {
        match command {
            Nip46Commands::Ping(pubkey, id) => {
                let response = Nip46Response {
                    id,
                    result: "pong".to_string(),
                };
                let response_note = Note::new(&user_keys.get_public_key(), 24133, &response.to_string());
                user_keys.sign_encrypted_nostr_event(response_note, pubkey)
            }
            Nip46Commands::SignEvent(pubkey, id, note) => {
                let signed_response = user_keys.sign_nostr_event(note);
                let response = Nip46Response {
                    id,
                    result: signed_response.to_string(),
                };
                let response_note = Note::new(&user_keys.get_public_key(), 24133, &response.to_string());
                user_keys.sign_encrypted_nostr_event(response_note, pubkey)
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::Note;
    use crate::userkeys::UserKeys;

    #[test]
    fn test_nip46_request() {
        let user_keys = UserKeys::generate();
        let client_keys = UserKeys::generate();
        let note_request = Note::new(&user_keys.get_public_key(), 24133, "test");
        let nip46_request = Nip46Request::sign_event_request(note_request, &client_keys);
        assert_eq!(nip46_request.get_kind(), 24133);
        assert_ne!(nip46_request.get_content(), "test");
    }

    #[test]
    fn test_nip46_ping_request() {
        let user_keys = UserKeys::generate();
        let client_keys = UserKeys::generate();
        let ping_request = Nip46Request::ping_request(&client_keys, user_keys.get_public_key());
        assert_eq!(ping_request.get_kind(), 24133);

        let nip46_command = Nip46Request::get_request_command(&ping_request, &user_keys);
        if let Nip46Commands::Ping(pubkey, _id) = &nip46_command {
            assert_eq!(pubkey, &client_keys.get_public_key());
        } else {
            panic!("Not a ping command");
        }
        let signed_note = Nip46Request::respond_to_command(&user_keys, nip46_command);
        assert_eq!(signed_note.verify(), true);
        let decrypted_note = client_keys.decrypt_note_content(&signed_note);
        let parsed_response = serde_json::from_str::<Nip46Response>(&decrypted_note).unwrap();
        assert_eq!(parsed_response.result, "pong");

    }

    #[test]
    fn test_nip46_sign_event() {
        // Client the user wants to log in to secureely 
        let client_keys = UserKeys::generate();
        // the user keys on the remote signer
        let user_keys = UserKeys::generate();

        // client builds this note to be signed
        let note_request = Note::new(&user_keys.get_public_key(), 24133, "test");
        // and builds the request note
        let nip46_request = Nip46Request::sign_event_request(note_request, &client_keys);

        // users bunker receives the request note and parses the command
        let nip46_command = Nip46Request::get_request_command(&nip46_request, &user_keys);
        if let Nip46Commands::SignEvent(pubkey, _id, note) = &nip46_command {
            assert_eq!(pubkey, &client_keys.get_public_key());
            assert_eq!(note.kind, 24133);
        } else {
            panic!("Not a sign_event command");
        }

        // the user bunker signs the event and sends it back
        let signed_note = Nip46Request::respond_to_command(&user_keys, nip46_command);
        assert_eq!(signed_note.verify(), true);

        // the client bunker receives the signed note and parses the response
        let response_note = Nip46Response::get_response_note(&signed_note, &client_keys);
        assert_eq!(response_note.get_content(), "test");
    }
}