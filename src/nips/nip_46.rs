use crate::{
    notes::{Note, SignedNote},
    userkeys::{NostroError, UserKeys},
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone)]
pub enum Nip46Commands {
    Connect(String, String),
    Disconnect(String, String),
    Ping(String, String),
    SignEvent(String, String, Note),
    GetPublickKey(String, String),
    Nip04Encrypt(String, String, String, String),
    Nip04Decrypt(String, String, String, String),
    Nip44Encrypt(String, String, String, String),
    Nip44Decrypt(String, String, String, String),
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Nip46Response {
    id: String,
    result: String,
    error: Option<String>,
}

impl Nip46Response {
    pub fn get_response_note(signed_note: &SignedNote, user_keys: &UserKeys) -> SignedNote {
        let decrypted_note_response = user_keys
            .decrypt_nip_04_content(signed_note)
            .expect("Could not decrypt note");
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
        self_try.sign_request(client_keys, user_keys).unwrap()
    }

    pub fn sign_event_request(note_request: Note, client_keys: &UserKeys) -> SignedNote {
        let random_id = format!("nostro2-{}", crate::utils::get_unix_timestamp());
        let note_params = vec![note_request.to_string()];
        let self_try = Self {
            id: random_id,
            method: "sign_event".to_string(),
            params: note_params,
        };
        self_try
            .sign_request(client_keys, note_request.pubkey)
            .unwrap()
    }

    pub fn get_public_key_request(client_keys: &UserKeys, user_keys: String) -> SignedNote {
        let random_id = format!("nostro2-{}", crate::utils::get_unix_timestamp());
        let ping_params = vec!["get_public_key".to_string()];
        let self_try = Self {
            id: random_id,
            method: "get_public_key".to_string(),
            params: ping_params,
        };
        self_try.sign_request(client_keys, user_keys).unwrap()
    }

    fn sign_request(
        &self,
        client_keys: &UserKeys,
        user_keys: String,
    ) -> Result<SignedNote, NostroError> {
        let stringified_request = serde_json::to_string(&self).unwrap();
        let request_note = Note::new(&client_keys.get_public_key(), 24133, &stringified_request);
        client_keys.sign_nip_04_encrypted(request_note, user_keys)
    }

    fn decrypt_request(
        signed_note: &SignedNote,
        user_keys: &UserKeys,
    ) -> Result<Nip46Request, NostroError> {
        let nip_04_decrypted_note_request = user_keys.decrypt_nip_04_content(signed_note);
        let nip_44_decrypted_note_request = user_keys.decrypt_nip_44_content(signed_note);

        if nip_04_decrypted_note_request.is_err() && nip_44_decrypted_note_request.is_err() {
            return Err(NostroError::DecryptionError(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Could not decrypt note",
            ))));
        }

        if nip_04_decrypted_note_request.is_ok() {
            let decrypted_note_request = nip_04_decrypted_note_request.unwrap();
            return Ok(serde_json::from_str::<Nip46Request>(&decrypted_note_request).unwrap());
        } else {
            let decrypted_note_request = nip_44_decrypted_note_request.unwrap();
            return Ok(serde_json::from_str::<Nip46Request>(&decrypted_note_request).unwrap());
        }
    }

    pub fn get_request_command(
        signed_note: &SignedNote,
        user_keys: &UserKeys,
    ) -> Result<Nip46Commands, NostroError> {
        let command_pubkey = signed_note.get_pubkey().to_string();
        let request_note = Self::decrypt_request(signed_note, user_keys)?;
        let command_id = request_note.id;
        match request_note.method.as_str() {
            "ping" => Ok(Nip46Commands::Ping(command_pubkey, command_id)),
            "sign_event" => {
                let response_note = serde_json::from_str::<Note>(&request_note.params[0]).unwrap();
                Ok(Nip46Commands::SignEvent(
                    command_pubkey,
                    command_id,
                    response_note,
                ))
            }
            "connect" => Ok(Nip46Commands::Connect(command_pubkey, command_id)),
            "disconnect" => Ok(Nip46Commands::Disconnect(command_pubkey, command_id)),
            "get_public_key" => Ok(Nip46Commands::GetPublickKey(command_pubkey, command_id)),
            "nip04_encrypt" => Ok(Nip46Commands::Nip04Encrypt(
                command_pubkey,
                command_id,
                request_note.params[1].to_string(),
                request_note.params[0].to_string(),
            )),
            "nip04_decrypt" => Ok(Nip46Commands::Nip04Decrypt(
                command_pubkey,
                command_id,
                request_note.params[1].to_string(),
                request_note.params[0].to_string(),
            )),
            "nip44_encrypt" => Ok(Nip46Commands::Nip44Encrypt(
                command_pubkey,
                command_id,
                request_note.params[1].to_string(),
                request_note.params[0].to_string(),
            )),
            "nip44_decrypt" => Ok(Nip46Commands::Nip44Decrypt(
                command_pubkey,
                command_id,
                request_note.params[1].to_string(),
                request_note.params[0].to_string(),
            )),
            _ => Err(NostroError::UnknownCommand),
        }
    }

    pub fn respond_to_command(user_keys: &UserKeys, command: Nip46Commands) -> SignedNote {
        match command {
            Nip46Commands::Connect(pubkey, id) => {
                let response = Nip46Response {
                    id,
                    result: "ack".to_string(),
                    error: None,
                };
                let response_note =
                    Note::new(&user_keys.get_public_key(), 24133, &response.to_string());
                user_keys
                    .sign_nip_04_encrypted(response_note, pubkey)
                    .unwrap()
            }
            Nip46Commands::Disconnect(pubkey, id) => {
                let response = Nip46Response {
                    id,
                    result: "ack".to_string(),
                    error: None,
                };
                let response_note =
                    Note::new(&user_keys.get_public_key(), 24133, &response.to_string());
                user_keys
                    .sign_nip_04_encrypted(response_note, pubkey)
                    .unwrap()
            }
            Nip46Commands::Ping(pubkey, id) => {
                let response = Nip46Response {
                    id,
                    result: "pong".to_string(),
                    error: None,
                };
                let response_note =
                    Note::new(&user_keys.get_public_key(), 24133, &response.to_string());
                user_keys
                    .sign_nip_04_encrypted(response_note, pubkey)
                    .unwrap()
            }
            Nip46Commands::SignEvent(pubkey, id, note) => {
                let signed_response = user_keys.sign_nostr_event(note);
                let response = Nip46Response {
                    id,
                    result: signed_response.to_string(),
                    error: None,
                };
                let response_note =
                    Note::new(&user_keys.get_public_key(), 24133, &response.to_string());
                user_keys
                    .sign_nip_04_encrypted(response_note, pubkey)
                    .unwrap()
            }
            Nip46Commands::GetPublickKey(pubkey, id) => {
                let response = Nip46Response {
                    id,
                    result: user_keys.get_public_key(),
                    error: None,
                };
                let response_note =
                    Note::new(&user_keys.get_public_key(), 24133, &response.to_string());
                user_keys
                    .sign_nip_04_encrypted(response_note, pubkey)
                    .unwrap()
            }
            Nip46Commands::Nip04Encrypt(pubkey, id, content, key) => {
                let encrypted_content = user_keys.encrypt_nip_04_plaintext(content, key).unwrap();
                let response = Nip46Response {
                    id,
                    result: encrypted_content,
                    error: None,
                };
                let response_note =
                    Note::new(&user_keys.get_public_key(), 24133, &response.to_string());
                user_keys
                    .sign_nip_04_encrypted(response_note, pubkey)
                    .unwrap()
            }
            Nip46Commands::Nip04Decrypt(pubkey, id, content, key) => {
                let decrypted_content = user_keys.decrypt_nip_04_plaintext(content, key).unwrap();
                let response = Nip46Response {
                    id,
                    result: decrypted_content,
                    error: None,
                };
                let response_note =
                    Note::new(&user_keys.get_public_key(), 24133, &response.to_string());
                user_keys
                    .sign_nip_04_encrypted(response_note, pubkey)
                    .unwrap()
            }
            Nip46Commands::Nip44Encrypt(pubkey, id, content, key) => {
                let encrypted_content = user_keys.encrypt_nip_44_plaintext(content, key).unwrap();
                let response = Nip46Response {
                    id,
                    result: encrypted_content,
                    error: None,
                };
                let response_note =
                    Note::new(&user_keys.get_public_key(), 24133, &response.to_string());
                user_keys
                    .sign_nip_04_encrypted(response_note, pubkey)
                    .unwrap()
            }
            Nip46Commands::Nip44Decrypt(pubkey, id, content, key) => {
                let decrypted_content = user_keys.decrypt_nip_44_plaintext(content, key).unwrap();
                let response = Nip46Response {
                    id,
                    result: decrypted_content,
                    error: None,
                };
                let response_note =
                    Note::new(&user_keys.get_public_key(), 24133, &response.to_string());
                user_keys
                    .sign_nip_04_encrypted(response_note, pubkey)
                    .unwrap()
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
        if let Ok(Nip46Commands::Ping(pubkey, _id)) = &nip46_command {
            assert_eq!(pubkey, &client_keys.get_public_key());
        } else {
            panic!("Not a ping command");
        }
        let signed_note = Nip46Request::respond_to_command(&user_keys, nip46_command.unwrap());
        assert_eq!(signed_note.verify(), true);
        let decrypted_note = client_keys
            .decrypt_nip_04_content(&signed_note)
            .expect("Could not decrypt note");
        let parsed_response = serde_json::from_str::<Nip46Response>(&decrypted_note).unwrap();
        assert_eq!(parsed_response.result, "pong");
    }

    #[test]
    fn test_nip46_sign_event() {
        // Client the user wants to log in to secureely
        let client_keys = UserKeys::generate();
        println!("Client key {}", client_keys.get_public_key());

        // the user keys on the remote signer
        let user_keys = UserKeys::generate();
        println!("User key {}", user_keys.get_public_key());

        // client builds this note to be signed
        let note_request = Note::new(&user_keys.get_public_key(), 42, "sing_me_please");
        // and builds the request note
        let nip46_request = Nip46Request::sign_event_request(note_request, &client_keys);

        // users bunker receives the request note and parses the command
        let nip46_command = Nip46Request::get_request_command(&nip46_request, &user_keys);
        if let Ok(Nip46Commands::SignEvent(pubkey, _id, note)) = &nip46_command {
            assert_eq!(pubkey, &client_keys.get_public_key());
            assert_eq!(note.kind, 42);
        } else {
            panic!("Not a sign_event command");
        }

        // the user bunker signs the event and sends it back
        let signed_note = Nip46Request::respond_to_command(&user_keys, nip46_command.unwrap());
        assert_eq!(signed_note.verify(), true);

        // the client bunker receives the signed note and parses the response
        let response_note = Nip46Response::get_response_note(&signed_note, &client_keys);
        assert_eq!(response_note.get_content(), "sing_me_please");
    }

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::*;

    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test]
    fn test_nip46_ping_request_wasm() {
        let user_keys = UserKeys::generate();
        let client_keys = UserKeys::generate();
        let ping_request = Nip46Request::ping_request(&client_keys, user_keys.get_public_key());
        assert_eq!(ping_request.get_kind(), 24133);

        let nip46_command = Nip46Request::get_request_command(&ping_request, &user_keys);
        if let Ok(Nip46Commands::Ping(pubkey, _id)) = &nip46_command {
            assert_eq!(pubkey, &client_keys.get_public_key());
        } else {
            panic!("Not a ping command");
        }
        let signed_note = Nip46Request::respond_to_command(&user_keys, nip46_command.unwrap());
        assert_eq!(signed_note.verify(), true);
        let decrypted_note = client_keys.decrypt_nip_04_content(&signed_note).unwrap();
        let parsed_response = serde_json::from_str::<Nip46Response>(&decrypted_note).unwrap();
        assert_eq!(parsed_response.result, "pong");
    }
}
