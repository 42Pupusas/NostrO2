use base64::{engine::general_purpose, Engine};
use lightning_invoice::Invoice;
use serde::{Deserialize, Serialize};
use serde_json::{json, from_str};
use tokio_tungstenite::tungstenite::Message;

use crate::{lnd::{get_htlc_invoice, LndWebSocket, cancel_htlc_invoice}, models::{
    nostr::{SignedNote, Note, NostrRelay}, 
    server::ServerBot, 
    lightning::HTLCStreamResult}};

#[derive(Serialize, Deserialize)]
pub struct DriverOffer {
    pub invoice: String,
    pub htlc: Option<String>,
    pub passenger: String,
}

#[derive(Serialize, Deserialize)]
pub struct PassengerRequest{
    pub invoice: String,
    pub htlc: String,
    pub status: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RideContract {
    ride: String,
    passenger: String,
    driver: String,

    invoice: String,
    htlc: Option<String>,
    id: Option<String>,
}

impl RideContract {
    pub fn new(
        ride: String,
        passenger: String,
        driver: String,
        invoice: String,
        htlc: Option<String>,
        id: Option<String>,
        ) -> Self {
        RideContract {
            ride,
            passenger,
            driver,
            invoice,
            htlc,
            id,
        }
    }

    pub fn get_invoice(&self) -> &str {
        &self.invoice
    }

    fn get_invoice_sat_amount(&self) -> Result<u64, &str> {
        match str::parse::<Invoice>(self.get_invoice()) {
            Ok(invoice) => match invoice.amount_milli_satoshis() {
                Some(amount) => Ok(amount / 1000),
                _ => Err("Error getting Invoice Amount"),
            },
            Err(e) => {
                println!("Error getting invoice: {:?}", e);
                Err("Error getting Invoice Amount")
            }
        }
    }

    fn get_invoice_base64(&self) -> String {
        let bolt11_invoice = str::parse::<Invoice>(&self.invoice).unwrap();
        let payment_hash = bolt11_invoice.payment_hash();
        let base64_invoice = general_purpose::URL_SAFE.encode(payment_hash);
            base64_invoice
        
    }

    pub async fn create_htlc(&mut self) -> Result<(), &str> {
        if let Ok(invoice_amount) = self.get_invoice_sat_amount() {
            let invoice_base64 = self.get_invoice_base64();
            match get_htlc_invoice(invoice_amount + 420, invoice_base64, 21000).await {
                Ok(htlc) => {
                    self.htlc = Some(htlc.payment_request);
                    Ok(())
                }
                Err(_e) => Err("Error getting HTLC"),
            }
        } else {
            Err("Error getting Invoice Amount")
        }
    }

    pub async fn cancel_htlc(&self) {
        let invoice_base_64 = self.get_invoice_base64();
        println!("Cancelling HTLC: {}", invoice_base_64);
        match cancel_htlc_invoice(invoice_base_64).await {
            Ok(_e) => println!("HTLC Cancelled"),
            Err(_e) => println!("Error cancelling HTLC"),
        }

    }

    pub async fn pay_invoice_and_settle_htlc(&self) {
        
    }

    pub fn get_nostr_note(&self, status: &str) -> Note {
        let offer_object = json!({
            "htlc": self.htlc.as_ref(),
            "invoice": self.invoice,
            "status": status,
        });

        let nostr_note = Note::new(
            ServerBot::new().get_public_key(),
            vec![
            vec!['p'.to_string(), self.passenger.clone()],
            vec!['p'.to_string(), self.driver.clone()],
            vec!['e'.to_string(), self.ride.clone()],
            ],
            4200 as u32,
            offer_object.to_string(),
            );
        nostr_note
    }

    pub async fn find_contract(id: String) -> Self {
        let contract_ws = NostrRelay::new().await;
        let prompt_filters = json!({ "ids": [id] });
        contract_ws.subscribe(prompt_filters).await.expect("Failed to subscribe");
        match contract_ws.read_notes().await {
            Some(message) => match message {
                Ok(Message::Text(message)) => {
                    if let Ok((_type, _id, note)) = from_str::<(String, String, SignedNote)>(&message) {
                        println!("Found nostr contract: {}", note.id);
                        match from_str::<PassengerRequest>(&note.content) {
                            Ok(passenger_note) => {
                                match contract_ws.close().await {

                                    Ok(_) => {
                                        println!("Closed contract websocket");
                                        return  RideContract::new(
                                            note.tags[2][1].clone(),
                                            note.tags[1][1].clone(),
                                            note.tags[0][1].clone(),
                                            passenger_note.invoice,
                                            Some(passenger_note.htlc),
                                            Some(note.id.clone()),
                                            );
                                    }
                                    
                                    Err(_e) => {
                                        println!("Error closing websocket");
                                        panic!();
                                    }
                                }

                            }
                            Err(_e) => {
                                println!("Error reading Driver Offer");
                                panic!();
                            }
                        }
                    } else {
                        println!("Error reading message");
                        panic!();
                    }
                }
                _ => {
                    println!("Error reading message");
                    panic!();
                }
            },
            _ => {
                println!("Error reading message");
                panic!();
            }
        }
    }


    pub async fn check_for_htlc_prepay(&self) -> Result<HTLCStreamResult, Box<dyn std::error::Error + Send>> {
        let mut heartbeat_count = 0;

        let mut contract_ws = LndWebSocket::new(self.get_invoice_base64()).await;
        loop {
            match contract_ws.read_invoice_states().await {
                Some(Ok(Message::Text(result))) => {
                    match from_str::<HTLCStreamResult>(&result) {
                        Ok(htlc_result) => {
                            match htlc_result.result.state.as_str() {
                                "ACCEPTED" => {
                                    println!("HTLC Accepted");
                                    return Ok(htlc_result);
                                }
                                "SETTLED" => {
                                    println!("HTLC Settled");
                                    contract_ws.close().await.unwrap();
                                    return Err(Box::new(std::io::Error::new(std::io::ErrorKind::Other, "HTLC Settled")));
                                }
                                "CANCELED" => {
                                    println!("HTLC Canceled");
                                    contract_ws.close().await.unwrap();
                                    return Err(Box::new(std::io::Error::new(std::io::ErrorKind::Other, "HTLC Canceled")));
                                }
                                _ => {
                                    println!("HTLC State: {}", htlc_result.result.state);
                                    continue;
                                }
                            }
                        }
                        Err(e) => {
                            println!("Error reading invoice result: {:?}", e);
                            continue;
                        }
                    }
                }
                Some(Ok(_)) => {
                    println!("Heartbeat received");
                    heartbeat_count += 1;
                    if heartbeat_count >= 5 { 
                        println!("Three heartbeats received, closing connection");
                        contract_ws.close().await.unwrap();
                        return Err(Box::new(std::io::Error::new(std::io::ErrorKind::Other, "Three heartbeats received")));
                    }
                    continue;
                }
                Some(Err(_e)) => {
                    return Err(Box::new(std::io::Error::new(std::io::ErrorKind::Other, "LND Error")));
                }
                _ => {
                    return Err(Box::new(std::io::Error::new(std::io::ErrorKind::Other, "LND error")));
                }
            }
        }
    }
}
