#![no_std]
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, Address, Bytes, Env, String, Symbol,
    token,
};

mod test;
// Onboarding is a separate logical contract; only one `#[contract]` may be linked per WASM
// artifact. Keep it in this crate for host tests (`cargo test`) but omit from guest builds.
#[cfg(not(target_family = "wasm"))]
pub mod onboarding;

const ESCROW: Symbol = symbol_short!("ESCROW");
const PLATFORM_FEE: Symbol = symbol_short!("PLAT_FEE");
const PLATFORM_WALLET: Symbol = symbol_short!("PLAT_WAL");
const TOTAL_FEES: Symbol = symbol_short!("TOT_FEES");
const ADMIN: Symbol = symbol_short!("ADMIN");

/// Default platform fee in basis points (500 = 5%)
const DEFAULT_PLATFORM_FEE_BPS: u32 = 500;
/// Maximum platform fee in basis points (10000 = 100%)
const MAX_PLATFORM_FEE_BPS: u32 = 1000; // 10% max

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EscrowStatus {
    Pending = 0,
    Released = 1,
    Refunded = 2,
    Disputed = 3,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Escrow {
    pub id: u64,
    pub buyer: Address,
    pub seller: Address,
    pub token: Address,
    pub amount: i128,
    pub status: EscrowStatus,
    pub release_window: u32, // Time in seconds before auto-release
    pub created_at: u32,
    pub ipfs_hash: Option<String>,
    pub metadata_hash: Option<Bytes>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowCreatedEvent {
    pub escrow_id: u64,
    pub buyer: Address,
    pub seller: Address,
    pub amount: i128,
    pub token: Address,
    pub release_window: u32,
    pub ipfs_hash: Option<String>,
    pub metadata_hash: Option<Bytes>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FundsReleasedEvent {
    pub escrow_id: u64,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FundsRefundedEvent {
    pub escrow_id: u64,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowDisputedEvent {
    pub escrow_id: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowMetadata {
    pub ipfs_hash: Option<String>,
    pub metadata_hash: Option<Bytes>,
}

/// Platform configuration data
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformConfig {
    pub platform_fee_bps: u32,      // Platform fee in basis points (500 = 5%)
    pub platform_wallet: Address,    // Wallet address to receive fees
    pub admin: Address,              // Admin address for management
}

#[contract]
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    fn validate_ipfs_cid(cid: &String) -> bool {
        let len = cid.len() as usize;
        if len == 0 || len > 128 {
            return false;
        }

        let mut buf = [0u8; 128];
        cid.copy_into_slice(&mut buf[0..len]);
        let cid_bytes = &buf[0..len];

        let is_v0 = len == 46
            && cid_bytes[0] == b'Q'
            && cid_bytes[1] == b'm'
            && cid_bytes.iter().all(|b| {
                matches!(
                    *b,
                    b'1'..=b'9'
                        | b'A'..=b'H'
                        | b'J'..=b'N'
                        | b'P'..=b'Z'
                        | b'a'..=b'k'
                        | b'm'..=b'z'
                )
            });

        let is_v1 = len >= 3
            && cid_bytes[0] == b'b'
            && cid_bytes[1..]
                .iter()
                .all(|b| matches!(*b, b'a'..=b'z' | b'2'..=b'7'));

        is_v0 || is_v1
    }

    fn validate_optional_ipfs_hash(ipfs_hash: &Option<String>) {
        if let Some(cid) = ipfs_hash {
            assert!(Self::validate_ipfs_cid(cid), "Invalid IPFS CID");
        }
    }

    fn validate_optional_metadata_hash(metadata_hash: &Option<Bytes>) {
        if let Some(hash) = metadata_hash {
            assert!(hash.len() == 32, "metadata_hash must be 32 bytes");
        }
    }

    /// Initialize the contract with platform configuration
    /// 
    /// # Arguments
    /// * `platform_wallet` - Address that will receive platform fees
    /// * `admin` - Admin address for managing platform settings
    /// * `platform_fee_bps` - Platform fee in basis points (default 500 = 5%)
    pub fn initialize(env: Env, platform_wallet: Address, admin: Address, platform_fee_bps: u32) {
        admin.require_auth();
        
        // Validate fee is within bounds
        assert!(platform_fee_bps <= MAX_PLATFORM_FEE_BPS, "Fee too high");
        
        let config = PlatformConfig {
            platform_fee_bps,
            platform_wallet: platform_wallet.clone(),
            admin: admin.clone(),
        };
        
        env.storage().persistent().set(&PLATFORM_FEE, &config);
        env.storage().persistent().set(&PLATFORM_WALLET, &platform_wallet);
        env.storage().persistent().set(&ADMIN, &admin);
        
        // Initialize total fees to 0
        let zero: i128 = 0;
        env.storage().persistent().set(&TOTAL_FEES, &zero);
    }
    /// Create a new escrow for an order
    /// 
    /// # Arguments
    /// * `buyer` - Address of the buyer
    /// * `seller` - Address of the seller
    /// * `token` - Token contract address (USDC)
    /// * `amount` - Amount to escrow
    /// * `order_id` - Unique order identifier
    /// * `release_window` - Time in seconds before auto-release (default 7 days = 604800)
    pub fn create_escrow(
        env: Env,
        buyer: Address,
        seller: Address,
        token: Address,
        amount: i128,
        order_id: u32,
        release_window: Option<u32>,
    ) -> Escrow {
        Self::create_escrow_with_metadata(
            env,
            buyer,
            seller,
            token,
            amount,
            order_id,
            release_window,
            None,
            None,
        )
    }

    /// Create a new escrow for an order and attach off-chain metadata.
    pub fn create_escrow_with_metadata(
        env: Env,
        buyer: Address,
        seller: Address,
        token: Address,
        amount: i128,
        order_id: u32,
        release_window: Option<u32>,
        ipfs_hash: Option<String>,
        metadata_hash: Option<Bytes>,
    ) -> Escrow {
        buyer.require_auth();
        
        // Validate amount is positive
        assert!(amount > 0, "Amount must be positive");
        
        // Validate buyer and seller are different
        assert!(buyer != seller, "Buyer and seller must be different");
        
        // Default to 7 days if not specified
        let window = release_window.unwrap_or(604800u32);
        let created_at_u64 = env.ledger().timestamp();
        assert!(created_at_u64 <= u32::MAX as u64, "Ledger timestamp overflow");
        let created_at = created_at_u64 as u32;
        Self::validate_optional_ipfs_hash(&ipfs_hash);
        Self::validate_optional_metadata_hash(&metadata_hash);

        let escrow = Escrow {
            id: order_id as u64,
            buyer: buyer.clone(),
            seller: seller.clone(),
            token: token.clone(),
            amount,
            status: EscrowStatus::Pending,
            release_window: window,
            created_at,
            ipfs_hash: ipfs_hash.clone(),
            metadata_hash: metadata_hash.clone(),
        };

        // Store escrow by order_id
        env.storage()
            .persistent()
            .set(&(ESCROW, order_id), &escrow);

        // Transfer funds from buyer to contract
        let client = token::Client::new(&env, &token);
        client.transfer(&buyer, &env.current_contract_address(), &amount);

        let event = EscrowCreatedEvent {
            escrow_id: order_id as u64,
            buyer: buyer.clone(),
            seller: seller.clone(),
            amount,
            token: token.clone(),
            release_window: window,
            ipfs_hash,
            metadata_hash,
        };
        env.events().publish((Symbol::new(&env, "escrow_created"), order_id as u64), event);

        escrow
    }

    /// Get platform configuration
    fn get_platform_config(env: &Env) -> PlatformConfig {
        env.storage()
            .persistent()
            .get(&PLATFORM_FEE)
            .expect("Platform not initialized")
    }

    /// Calculate platform fee for a given amount
    fn calculate_fee(amount: i128, fee_bps: u32) -> i128 {
        (amount * (fee_bps as i128)) / 10000
    }

    /// Release funds to seller with platform fee deduction
    /// 
    /// # Arguments
    /// * `order_id` - Order identifier
    pub fn release_funds(env: Env, order_id: u32) {
        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&(ESCROW, order_id))
            .expect("Escrow not found");

        // Only buyer can release funds
        escrow.buyer.require_auth();
        
        assert!(
            escrow.status == EscrowStatus::Pending,
            "Escrow already processed"
        );

        // Get platform config
        let config = Self::get_platform_config(&env);
        
        // Calculate platform fee
        let fee_amount = Self::calculate_fee(escrow.amount, config.platform_fee_bps);
        let seller_amount = escrow.amount - fee_amount;

        // Update status
        escrow.status = EscrowStatus::Released;
        env.storage()
            .persistent()
            .set(&(ESCROW, order_id), &escrow);

        // Transfer platform fee to platform wallet
        let token_client = token::Client::new(&env, &escrow.token);
        if fee_amount > 0 {
            token_client.transfer(
                &env.current_contract_address(),
                &config.platform_wallet,
                &fee_amount,
            );
            
            // Update total fees collected
            let mut total_fees: i128 = env
                .storage()
                .persistent()
                .get(&TOTAL_FEES)
                .unwrap_or(0);
            total_fees += fee_amount;
            env.storage().persistent().set(&TOTAL_FEES, &total_fees);
        }
        
        // Transfer remaining funds to seller
        token_client.transfer(&env.current_contract_address(), &escrow.seller, &seller_amount);

        env.events().publish(
            (Symbol::new(&env, "funds_released"), order_id as u64),
            FundsReleasedEvent {
                escrow_id: order_id as u64,
                amount: escrow.amount,
            },
        );
    }

    /// Auto-release funds after release window (seller can call)
    /// 
    /// # Arguments
    /// * `order_id` - Order identifier
    pub fn auto_release(env: Env, order_id: u32) {
        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&(ESCROW, order_id))
            .expect("Escrow not found");

        assert!(
            escrow.status == EscrowStatus::Pending,
            "Escrow already processed"
        );

        let current_time = env.ledger().timestamp();
        let elapsed = current_time - (escrow.created_at as u64);

        assert!(
            elapsed >= escrow.release_window as u64,
            "Release window not yet elapsed"
        );

        // Get platform config
        let config = Self::get_platform_config(&env);
        
        // Calculate platform fee
        let fee_amount = Self::calculate_fee(escrow.amount, config.platform_fee_bps);
        let seller_amount = escrow.amount - fee_amount;

        // Update status
        escrow.status = EscrowStatus::Released;
        env.storage()
            .persistent()
            .set(&(ESCROW, order_id), &escrow);

        // Transfer platform fee to platform wallet
        let token_client = token::Client::new(&env, &escrow.token);
        if fee_amount > 0 {
            token_client.transfer(
                &env.current_contract_address(),
                &config.platform_wallet,
                &fee_amount,
            );
            
            // Update total fees collected
            let mut total_fees: i128 = env
                .storage()
                .persistent()
                .get(&TOTAL_FEES)
                .unwrap_or(0);
            total_fees += fee_amount;
            env.storage().persistent().set(&TOTAL_FEES, &total_fees);
        }
        
        // Transfer remaining funds to seller
        token_client.transfer(&env.current_contract_address(), &escrow.seller, &seller_amount);

        env.events().publish(
            (Symbol::new(&env, "funds_released"), order_id as u64),
            FundsReleasedEvent {
                escrow_id: order_id as u64,
                amount: escrow.amount,
            },
        );
    }

    /// Refund funds to buyer (for disputes or cancellations)
    /// 
    /// # Arguments
    /// * `order_id` - Order identifier
    /// * `authorized_address` - Address authorized to refund (platform or buyer)
    pub fn refund(env: Env, order_id: u32, authorized_address: Address) {
        authorized_address.require_auth();
        
        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&(ESCROW, order_id))
            .expect("Escrow not found");

        // Only buyer or platform can refund
        assert!(
            escrow.buyer == authorized_address || 
            authorized_address == env.current_contract_address(), // Platform check
            "Not authorized to refund"
        );

        assert!(
            escrow.status == EscrowStatus::Pending,
            "Escrow already processed"
        );

        // Update status
        escrow.status = EscrowStatus::Refunded;
        env.storage()
            .persistent()
            .set(&(ESCROW, order_id), &escrow);

        // Refund to buyer
        let client = token::Client::new(&env, &escrow.token);
        client.transfer(&env.current_contract_address(), &escrow.buyer, &escrow.amount);

        env.events().publish(
            (Symbol::new(&env, "funds_refunded"), order_id as u64),
            FundsRefundedEvent {
                escrow_id: order_id as u64,
                amount: escrow.amount,
            },
        );
    }

    /// Get escrow details
    /// 
    /// # Arguments
    /// * `order_id` - Order identifier
    pub fn get_escrow(env: Env, order_id: u32) -> Escrow {
        env.storage()
            .persistent()
            .get(&(ESCROW, order_id))
            .expect("Escrow not found")
    }

    /// Get escrow metadata fields only.
    pub fn get_escrow_metadata(env: Env, order_id: u32) -> EscrowMetadata {
        let escrow = Self::get_escrow(env, order_id);
        EscrowMetadata {
            ipfs_hash: escrow.ipfs_hash,
            metadata_hash: escrow.metadata_hash,
        }
    }

    /// Check if escrow can be auto-released
    /// 
    /// # Arguments
    /// * `order_id` - Order identifier
    pub fn can_auto_release(env: Env, order_id: u32) -> bool {
        let escrow: Escrow = env
            .storage()
            .persistent()
            .get(&(ESCROW, order_id))
            .expect("Escrow not found");

        if escrow.status != EscrowStatus::Pending {
            return false;
        }

        let current_time = env.ledger().timestamp();
        let elapsed = current_time - (escrow.created_at as u64);

        elapsed >= escrow.release_window as u64
    }

    /// Dispute an escrow
    /// 
    /// # Arguments
    /// * `order_id` - Order identifier
    /// * `authorized_address` - Address authorized to dispute (buyer or admin)
    pub fn dispute_escrow(env: Env, order_id: u32, authorized_address: Address) {
        authorized_address.require_auth();

        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&(ESCROW, order_id))
            .expect("Escrow not found");

        let config = Self::get_platform_config(&env);

        // Allow buyer or admin to dispute
        assert!(
            escrow.buyer == authorized_address || authorized_address == config.admin,
            "Not authorized to dispute"
        );

        assert!(
            escrow.status == EscrowStatus::Pending,
            "Escrow already processed"
        );

        escrow.status = EscrowStatus::Disputed;
        env.storage()
            .persistent()
            .set(&(ESCROW, order_id), &escrow);

        env.events().publish(
            (Symbol::new(&env, "escrow_disputed"), order_id as u64),
            EscrowDisputedEvent {
                escrow_id: order_id as u64,
            },
        );
    }

    /// Update platform fee percentage (admin only)
    /// 
    /// # Arguments
    /// * `new_fee_bps` - New fee in basis points
    pub fn update_platform_fee(env: Env, new_fee_bps: u32) {
        let config = Self::get_platform_config(&env);
        config.admin.require_auth();
        
        assert!(new_fee_bps <= MAX_PLATFORM_FEE_BPS, "Fee too high");
        
        let new_config = PlatformConfig {
            platform_fee_bps: new_fee_bps,
            platform_wallet: config.platform_wallet,
            admin: config.admin,
        };
        
        env.storage().persistent().set(&PLATFORM_FEE, &new_config);
    }

    /// Update platform wallet address (admin only)
    /// 
    /// # Arguments
    /// * `new_wallet` - New platform wallet address
    pub fn update_platform_wallet(env: Env, new_wallet: Address) {
        let config = Self::get_platform_config(&env);
        config.admin.require_auth();
        
        let new_config = PlatformConfig {
            platform_fee_bps: config.platform_fee_bps,
            platform_wallet: new_wallet,
            admin: config.admin,
        };
        
        env.storage().persistent().set(&PLATFORM_FEE, &new_config);
    }

    /// Get current platform fee percentage
    pub fn get_platform_fee(env: Env) -> u32 {
        let config = Self::get_platform_config(&env);
        config.platform_fee_bps
    }

    /// Get platform wallet address
    pub fn get_platform_wallet(env: Env) -> Address {
        let config = Self::get_platform_config(&env);
        config.platform_wallet
    }

    /// Get total fees collected by platform
    pub fn get_total_fees_collected(env: Env) -> i128 {
        env.storage()
            .persistent()
            .get(&TOTAL_FEES)
            .unwrap_or(0)
    }

    /// Calculate the fee for a given amount (for display purposes)
    /// 
    /// # Arguments
    /// * `amount` - The escrow amount
    pub fn calculate_fee_for_amount(env: Env, amount: i128) -> i128 {
        let config = Self::get_platform_config(&env);
        Self::calculate_fee(amount, config.platform_fee_bps)
    }

    /// Calculate net amount seller will receive
    /// 
    /// # Arguments
    /// * `amount` - The escrow amount
    pub fn calculate_seller_net_amount(env: Env, amount: i128) -> i128 {
        let fee = Self::calculate_fee_for_amount(env, amount);
        amount - fee
    }
}
