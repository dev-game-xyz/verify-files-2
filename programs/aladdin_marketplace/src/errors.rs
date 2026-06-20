use anchor_lang::prelude::*;

/// All custom errors the program can throw. Keeping them explicit makes the
/// security story auditable: every `require!` points at a named reason.
#[error_code]
pub enum MarketError {
    #[msg("Only the marketplace owner (the game's wallet) may call this.")]
    NotOwner,

    #[msg("This SPL token mint is not on the allowed-items list.")]
    ItemNotAllowed,

    #[msg("The listing id must be 1-32 ASCII alphanumeric characters.")]
    InvalidListingId,

    #[msg("The username must be 1-10 lowercase letters or digits (a-z, 0-9).")]
    InvalidUsername,

    #[msg("Price must be a whole number greater than zero (no decimals).")]
    InvalidPrice,

    #[msg("A listing must hold exactly one whole token unit.")]
    InvalidAmount,

    #[msg("The provided token mint does not match the listing.")]
    MintMismatch,

    #[msg("The seller token account owner does not match the signer.")]
    SellerMismatch,

    #[msg("The destination owner does not match the requested new owner.")]
    NewOwnerMismatch,

    #[msg("This wallet has already claimed a username (usernames are permanent).")]
    UsernameAlreadyBound,

    #[msg("Arithmetic overflow.")]
    Overflow,

    #[msg("The new owner address is invalid (must not be the system program / zero).")]
    InvalidNewOwner,
}
