use serde::{Deserialize, Serialize };
use serde_json::{json, Value};

#[derive(Serialize, Debug)]
pub struct InvoiceRequest {
    pub value: u64,
}

#[derive(Serialize, Debug, Deserialize)]
pub struct InvoiceResponse {
    pub r_hash: String,
    pub payment_request: String,
}

#[derive(Serialize, Debug)]
pub struct HTLCRequest {
    pub value: u64,
    pub hash: String,
    pub expiry: u64,
}

#[derive(Serialize, Deserialize)]
pub struct HtlcCancelRequest {
    pub payment_hash: String,
}

#[derive(Serialize, Deserialize)]
pub struct HTLCResponse {
    pub payment_request: String,
    pub add_index: String,
    pub payment_addr: String,
}
#[derive(Serialize, Debug, Deserialize)]
pub struct CheckResponseBody {
    pub r_hash: String,
    pub payment_request: String,
    pub settled: bool,
}

#[derive(Serialize, Debug, Deserialize)]
pub struct PaymentRequest {
    pub payment_request: String,
    fee_limit: Value,
    allow_self_payment: bool,
} 
impl PaymentRequest {
    pub fn new(payment_request: String) -> Self {
        PaymentRequest {
            payment_request,
            fee_limit: json!({"fixed": 210 }),
            allow_self_payment: true,
        }
    }
}

#[derive(Serialize, Debug, Deserialize)]
pub struct PaymentResponse {
    pub payment_preimage: String,
}

#[derive(Serialize, Debug, Deserialize)]
pub struct HTLCStreamResult {
    pub result: HTLCStreamResponse,
}

#[derive(Serialize, Debug, Deserialize)]
pub struct HTLCSettleRequest {
    pub preimage: String,
}

#[derive(Serialize, Debug, Deserialize, Clone)]
pub struct HTLCStreamResponse {
    pub payment_request: String,
    pub add_index: String,
    pub payment_addr: String,
    pub state: String,
    pub settled: bool,
}

