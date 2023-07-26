use std::sync::Arc;
use serde_json::{json, from_str};
use tokio_tungstenite::tungstenite::Message;
use utils::{models::{nostr::{NostrRelay, SignedNote}, server::ServerBot}, lnd::{pay_invoice, settle_htlc}, roadrunner::RideContract};

#[tokio::main]
async fn main() {

    let relay = Arc::new(NostrRelay::new().await);    // Create prompt filters and send them to the server
    let prompt_filters = json!({ "kinds": [20022, 20023]});
    relay.subscribe(prompt_filters).await.expect("Failed to subscribe to prompts");

    loop {
        match relay.read_notes().await {
            Some(Ok(Message::Text(message))) => {
                if let Ok((_type, _id, note)) = 
                    from_str::<(String, String, SignedNote)>(&message) {
                        match note.kind {
                            20022 => {
                                let relay_clone = Arc::clone(&relay);
                                // TODO Check that the note is from the passenger on the contract
                                tokio::spawn(async move {
                                    let server_bot = ServerBot::new();
                                    println!("User wants to pay: {}", note.content);
                                    let ride_contract = RideContract::find_contract(note.content).await;
                                    match pay_invoice(ride_contract.get_invoice().to_string()).await {
                                        Ok(preimage) => {
                                            println!("Paid invoice: {}", preimage);
                                            match settle_htlc(preimage).await {
                                                Ok(_) => {
                                                    let settle_note = server_bot.
                                                        sign_nostr_event(ride_contract.get_nostr_note("settled"));
                                                    relay_clone.send_note(settle_note).await;
                                                },
                                                Err(_) => {
                                                    println!("Failed to settle htlc");
                                                }
                                            }

                                        },
                                        Err(_) => {
                                        }
                                    }
                                });

                            },
                            20023 => {
                                let relay_clone = Arc::clone(&relay);
                                
                                tokio::spawn(async move {
                                    let server_bot = ServerBot::new();
                                    println!("User wants to cancel: {}", note.content);
                                    let ride_contract = RideContract::find_contract(note.content).await; 
                                    // TODO Check that the note is from the passenger or driver on the contract
                                    ride_contract.cancel_htlc().await;
                                    let cancel_note = server_bot.
                                        sign_nostr_event(ride_contract.get_nostr_note("canceled"));
                                    relay_clone.send_note(cancel_note).await;
                                });

                            },
                            _ => println!("Received another kind"),
                        }
                    }
            }
            _ => println!("Received something else"),
        }
    }
}
