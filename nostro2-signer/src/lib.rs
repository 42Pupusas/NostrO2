#![warn(
    clippy::all,
    clippy::style,
    clippy::unseparated_literal_suffix,
    clippy::pedantic,
    clippy::nursery
)]
pub mod keypair;
pub mod nip_04;
pub mod nip_44;
pub extern crate nostro2;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {}
}
