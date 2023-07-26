use std::sync::Arc;
use serde_json::{from_str, json};
use tokio_tungstenite::tungstenite::Message;
use utils::{models::{nostr::{SignedNote, NostrRelay}, server::ServerBot}, roadrunner::RideContract};

#[tokio::main]
async fn main() {

    let nostr_ws = Arc::new(NostrRelay::new().await);    // Create prompt filters and send them to the server
    let prompt_filters = json!({ "kinds": [20020]});
    nostr_ws.subscribe(prompt_filters).await.expect("Failed to subscribe");

    loop {
        match nostr_ws.read_notes().await {
            Some(Ok(Message::Text(message))) => {
                if let Ok((_type, _id, note)) = 
                    from_str::<(String, String, SignedNote)>(&message) {
                                let nostr_ws_clone = Arc::clone(&nostr_ws);
                                tokio::spawn(async move {
                                    println!("Passenger wants to prepay");
                                    let server_bot = ServerBot::new();
                                    let retrieved_contract = 
                                        RideContract::find_contract(note.content).await;
                                    match retrieved_contract.check_for_htlc_prepay().await {
                                        Ok(_) => {
                                            println!("Passenger has prepaid");
                                            let accepted_notice = server_bot
                                                .sign_nostr_event(
                                                    retrieved_contract.get_nostr_note("accepted"));
                                            nostr_ws_clone.send_note(accepted_notice).await;
                                        },
                                        Err(_) => {
                                            println!("Passenger has not prepaid");
                                        }
                                    } 
                                });

                    }
                // ERROR AND NOTICE HANDLERS
                else if let Ok((notice, id)) = from_str::<(String, String)>(&message) {
                    println!("Received notice: {} {}", notice, id);
                }
                else  {
                    println!("Received relay message");
                }
            }
            _ => {
                println!("Something went wrong");
            }
        }
    }
}
