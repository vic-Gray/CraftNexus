#![no_std]
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, Address, Env, Symbol,
    token,
};

mod test;

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
    Resolved = 4,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Resolution {
    ReleaseToSeller = 0,
    RefundToBuyer = 1,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Escrow {
    pub buyer: Address,
    pub seller: Address,
    pub token: Address,
    pub amount: i128,
    pub status: EscrowStatus,
    pub created_at: u64,
    pub release_window: u64, // Time in seconds before auto-release
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
pub struct EscrowResolvedEvent {
    pub escrow_id: u64,
    pub resolution: Resolution,
}

/// Platform configuration data
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformConfig {
    pub platform_fee_bps: u32,      // Platform fee in basis points (500 = 5%)
    pub platform_wallet: Address,    // Wallet address to receive fees
    pub admin: Address,              // Admin address for management
    pub arbitrator: Address,         // Arbitrator for dispute resolution
}

#[contract]
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    /// Initialize the contract with platform configuration
    /// 
    /// # Arguments
    /// * `platform_wallet` - Address that will receive platform fees
    /// * `admin` - Admin address for managing platform settings
    /// * `platform_fee_bps` - Platform fee in basis points (default 500 = 5%)
    pub fn initialize(env: Env, platform_wallet: Address, admin: Address, arbitrator: Address, platform_fee_bps: u32) {
        admin.require_auth();

        // Validate fee is within bounds
        assert!(platform_fee_bps <= MAX_PLATFORM_FEE_BPS, "Fee too high");

        let config = PlatformConfig {
            platform_fee_bps,
            platform_wallet: platform_wallet.clone(),
            admin: admin.clone(),
            arbitrator: arbitrator.clone(),
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
        release_window: Option<u64>,
    ) -> Escrow {
        buyer.require_auth();
        
        // Validate amount is positive
        assert!(amount > 0, "Amount must be positive");
        
        // Validate buyer and seller are different
        assert!(buyer != seller, "Buyer and seller must be different");
        
        // Default to 7 days if not specified
        let window = release_window.unwrap_or(604800u64);
        let created_at = env.ledger().timestamp();

        let escrow = Escrow {
            buyer: buyer.clone(),
            seller: seller.clone(),
            token: token.clone(),
            amount,
            status: EscrowStatus::Pending,
            created_at,
            release_window: window,
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
            release_window: window as u32,
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

    fn get_arbitrator(env: &Env) -> Address {
        Self::get_platform_config(env).arbitrator
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
        let elapsed = current_time - escrow.created_at;

        assert!(
            elapsed >= escrow.release_window,
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

    fn release_funds_to_seller(env: &Env, escrow: &Escrow) {
        let config = Self::get_platform_config(env);
        let fee_amount = Self::calculate_fee(escrow.amount, config.platform_fee_bps);
        let seller_amount = escrow.amount - fee_amount;

        let token_client = token::Client::new(env, &escrow.token);
        if fee_amount > 0 {
            token_client.transfer(
                &env.current_contract_address(),
                &config.platform_wallet,
                &fee_amount,
            );
            let mut total_fees: i128 = env.storage().persistent().get(&TOTAL_FEES).unwrap_or(0);
            total_fees += fee_amount;
            env.storage().persistent().set(&TOTAL_FEES, &total_fees);
        }

        token_client.transfer(&env.current_contract_address(), &escrow.seller, &seller_amount);
    }

    fn refund_funds_to_buyer(env: &Env, escrow: &Escrow) {
        let token_client = token::Client::new(env, &escrow.token);
        token_client.transfer(&env.current_contract_address(), &escrow.buyer, &escrow.amount);
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
        let elapsed = current_time - escrow.created_at;

        elapsed >= escrow.release_window
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

    /// Resolve disputed escrow (arbitrator only)
    pub fn resolve_dispute(env: Env, order_id: u32, resolution: Resolution) {
        let arbitrator = Self::get_arbitrator(&env);
        arbitrator.require_auth();

        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&(ESCROW, order_id))
            .expect("Escrow not found");

        assert!(escrow.status == EscrowStatus::Disputed, "Escrow not in dispute");

        match resolution {
            Resolution::ReleaseToSeller => {
                Self::release_funds_to_seller(&env, &escrow);
            }
            Resolution::RefundToBuyer => {
                Self::refund_funds_to_buyer(&env, &escrow);
            }
        }

        escrow.status = EscrowStatus::Resolved;
        env.storage().persistent().set(&(ESCROW, order_id), &escrow);

        env.events().publish(
            (Symbol::new(&env, "escrow_resolved"), order_id as u64),
            EscrowResolvedEvent {
                escrow_id: order_id as u64,
                resolution,
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
            arbitrator: config.arbitrator,
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
            arbitrator: config.arbitrator,
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
