use base64::{engine::general_purpose, Engine as _};
use rand::{thread_rng, Rng};
use reqwest::header::{HeaderMap, HeaderValue};
use crate::models::{
    lightning::{HTLCRequest, HTLCResponse, InvoiceRequest, InvoiceResponse, HtlcCancelRequest, PaymentRequest, PaymentResponse, HTLCSettleRequest },
    secrets::{MACAROON_PATH, REST_HOST}, 
};
use std::{sync::{Arc, Mutex}, fs};
use httparse::{Header, Request};
use futures_util::{StreamExt, SinkExt, stream::SplitSink};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio_tungstenite::{tungstenite::{protocol::{Message as WsMessage, CloseFrame, frame::coding::CloseCode}, Error as TungsteniteError}, WebSocketStream, Connector, connect_async_tls_with_config};

pub async fn get_invoice(value: u64) -> Result<InvoiceResponse, Box<dyn std::error::Error>> {
    let request_body = InvoiceRequest { value };
    // println!("request_body: {:?}", request_body);
    let mut headers = HeaderMap::new();
    headers.insert(
        "Grpc-Metadata-macaroon",
        HeaderValue::from_str(&hex::encode(fs::read(MACAROON_PATH)?))?,
        );
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .default_headers(headers)
        .build()?;
    let options = client
        .post(&format!("https://{}/v1/invoices", REST_HOST))
        .json(&request_body)
        .send()
        .await
        .expect("Failed to send request");

    let body = options.text().await.expect("Failed to get response body");
    let invoice_response: InvoiceResponse =
        serde_json::from_str(&body).expect("Failed to parse response body");
    Ok(invoice_response)
}

pub async fn get_htlc_invoice(
    value: u64,
    hash: String,
    expiry: u64,
    ) -> Result<HTLCResponse, Box<dyn std::error::Error>> {

    let request_body = HTLCRequest {
        value,
        hash,
        expiry,
    };

    // println!("request_body: {:?}", request_body);
    let mut headers = HeaderMap::new();
    headers.insert(
        "Grpc-Metadata-macaroon",
        HeaderValue::from_str(&hex::encode(fs::read(MACAROON_PATH)?))?,
        );
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .default_headers(headers)
        .build()?;
    let options = client
        .post(&format!("https://{}/v2/invoices/hodl", REST_HOST))
        .json(&request_body)
        .send()
        .await
        .expect("Failed to send request");

    match options.text().await {
        Ok(body) => match serde_json::from_str::<HTLCResponse>(&body) {
            Ok(htlc) => Ok(htlc),
            Err(_e) => {
                println!("Error parsing invoice response: {:?}", body);
                Err(Box::new(_e))
            }
        },
        Err(e) => {
            println!("Error getting invoice: {:?}", e);
            Err(Box::new(e))
        }
    }
}


pub async fn cancel_htlc_invoice(
    payment_hash: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
    let request_body = HtlcCancelRequest { payment_hash };

    let mut headers = HeaderMap::new();
    headers.insert(
        "Grpc-Metadata-macaroon",
         HeaderValue::from_str(&hex::encode(fs::read(MACAROON_PATH)?))?,
        );

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .default_headers(headers)
        .build()?;

    let response = client
        .post(&format!("https://{}/v2/invoices/cancel", REST_HOST))
        .json(&request_body)
        .send()
        .await?;

    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Request failed with {}: {}", status, text),
                    )))
    }
}

pub async fn pay_invoice(invoice: String) -> Result<String, Box<dyn std::error::Error + Send>> {
    let mut headers = HeaderMap::new();
    let request_body = PaymentRequest::new(invoice);
    headers.insert(
        "Grpc-Metadata-macaroon",
         HeaderValue::from_str(&hex::encode(fs::read(MACAROON_PATH).unwrap())).unwrap(),
        );

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .default_headers(headers)
        .build()
        .unwrap();

    let response = client
        .post(&format!("https://{}/v1/channels/transactions", REST_HOST))
        .json(&request_body)
        .send()
        .await
        .unwrap();

    match response.text().await {
        Ok(body) => match serde_json::from_str::<PaymentResponse>(&body){
            Ok(payment) => {
                println!("payment: {:?}", payment);
                Ok(payment.payment_preimage)
            },
            Err(_e) => {
                println!("Error parsing invoice response: {:?}", body);
                Err(Box::new(_e))
            }
        },
        Err(e) => {
            println!("Error getting invoice: {:?}", e);
            Err(Box::new(e))
        }
    }
}

pub async fn settle_htlc(preimage: String) -> Result<(), Box<dyn std::error::Error + Send>> {
    let request_body = HTLCSettleRequest { preimage };

    let mut headers = HeaderMap::new();
    headers.insert(
        "Grpc-Metadata-macaroon",
         HeaderValue::from_str(&hex::encode(fs::read(MACAROON_PATH).unwrap())).unwrap(),
        );

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .default_headers(headers)
        .build()
        .unwrap();
    let response = client
        .post(&format!("https://{}/v2/invoices/settle", REST_HOST))
        .json(&request_body)
        .send()
        .await
        .unwrap();

    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Request failed with {}: {}", status, text),
                    )))
    }
}

fn generate_random_key() -> String {
    let mut rng = thread_rng();
    let mut key = [0u8; 16];
    rng.fill(&mut key);
    let new_key = general_purpose::STANDARD.encode(key);
    new_key
}


pub struct LndWebSocket {
    _url: Arc<str>,
    ws_write: Arc<Mutex<SplitSink<WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, WsMessage>>>,
    state_receiver: UnboundedReceiver<Result<WsMessage, TungsteniteError>>,
}

impl LndWebSocket {

    pub async fn new(r_hash: String) -> Self {
        // Prepare the websocket request URL
        let url = format!(
            "wss://{}/v2/invoices/subscribe/{}?method=GET",
            REST_HOST, r_hash
            );

        // Prepare the headers
        // let macaroon = MACAROON_PATH;
        let macaroon = hex::encode(fs::read(MACAROON_PATH).unwrap());
        let random_key = generate_random_key();

        let mut headers = [
            Header {
                name: "Grpc-Metadata-macaroon",
                value: macaroon.as_bytes(),
            },
            Header {
                name: "Sec-WebSocket-Key",
                value: random_key.as_bytes(),
            },
            Header {
                name: "Host",
                value: REST_HOST.as_bytes(),
            },
            Header {
                name: "Connection",
                value: "Upgrade".as_bytes(),
            },
            Header {
                name: "Upgrade",
                value: "websocket".as_bytes(),
            },
            httparse::Header {
                name: "Sec-WebSocket-Version",
                value: "13".as_bytes(),
            },
            ];
        let mut req = Request::new(&mut headers);
        req.method = Some("GET");
        req.path = Some(&url);
        req.version = Some(1);

        // Prepare the websocket connection with SSL
        let danger_conf = Some(tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
            max_send_queue: None,
            max_message_size: None,
            max_frame_size: None,
            accept_unmasked_frames: true,
        });

        let tls_connector = Some(Connector::NativeTls(
                native_tls::TlsConnector::builder()
                .danger_accept_invalid_certs(true)
                .build()
                .unwrap()

                ));

        if let Ok((ws_stream, _)) = connect_async_tls_with_config(req, danger_conf, tls_connector).await {
            let (ws_write, mut ws_read) = ws_stream.split();
            let (state_sender, state_receiver) = unbounded_channel();

            tokio::spawn(async move {
                while let Some(message) = ws_read.next().await {
                    match state_sender.send(message) {
                        Ok(_) => {}
                        Err(e) => {
                            println!("Error sending message: {:?}", e);
                            break;
                        }
                    }
                }
            });

            Self {
                _url: Arc::from(url),
                ws_write: Arc::new(Mutex::new(ws_write)),
                state_receiver,
            }
        } else {
            panic!("Error connecting to the websocket");
        }
    }

    pub async fn read_invoice_states(&mut self) -> Option<Result<WsMessage, TungsteniteError>> {
        self.state_receiver.recv().await
    }

    pub async fn close(&self) -> Result<(), Box<dyn std::error::Error>> {
        let ws_write = Arc::clone(&self.ws_write);
        let close_msg = WsMessage::Close(Some(CloseFrame {
            code: CloseCode::Normal,
            reason: "Bye bye".into(),
        }));
        tokio::task::spawn_blocking(move || {
            let mut write_guard = ws_write.lock().unwrap();
            match tokio::runtime::Handle::current().block_on(write_guard.send(close_msg)) {
                Ok(_) => (),
                Err(e) => {
                    println!("Error closing the connection: {:?}", e);
                }
            }
        });
        Ok(())
    }

}


