use std::sync::Arc;
use serde_json::{from_str, json};
use tokio_tungstenite::tungstenite::Message;
use utils::{models::{
    nostr::{SignedNote, NostrRelay},
    server::ServerBot,
}, roadrunner::{DriverOffer, RideContract}};

#[tokio::main]
async fn main() {

    let relay_ws = Arc::new(NostrRelay::new().await);
    let prompt_filters = json!({
        "kinds": [20010]
    });
    relay_ws.subscribe(prompt_filters).await.expect("Failed to subscribe");

    loop {
        match relay_ws.read_notes().await {
            Some(Ok(Message::Text(message))) => {
                if let Ok((_type, _id, note)) = from_str::<(String, String, SignedNote)>(&message) {
                    match from_str::<DriverOffer>(&note.content) {
                        Ok(driver_offer) => {
                            let relay_ws_clone = Arc::clone(&relay_ws);

                            tokio::spawn(async move {
                                let mut ride_contract = RideContract::new(
                                    note.tags[0][1].clone(),
                                    driver_offer.passenger,
                                    note.pubkey,
                                    driver_offer.invoice,
                                    None,
                                    None,
                                    );
                                match ride_contract.create_htlc().await {
                                    Ok(()) => {
                                        let contract_note = ride_contract.get_nostr_note("offered");
                                        let contract_note =
                                            ServerBot::new().sign_nostr_event(contract_note);
                                        relay_ws_clone.send_note(contract_note).await;
                                    }
                                    _ => {
                                        println!("Error creating HTLC");
                                    }
                                }
                            });
                        }
                        Err(e) => {
                            println!("Error getting invoice: {:?}", e);
                        }
                    }
                } 
                else if let Ok((notice, id)) = from_str::<(String, String)>(&message) {
                    println!("Received notice: {} {}", notice, id);

                }
                else {
                    println!("Received relay message");
                }
            }
            Some(Err(e)) => {
                println!("Error reading message: {:?}", e);
            }
            _ => {
                println!("Received empty message");
            }
        }
    }
}
