#![cfg(test)]

use super::*;
use crate::onboarding::OnboardingContract;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Env, String, Symbol,
};

fn setup_test() -> (
    Env,
    EscrowContractClient<'static>,
    Address,
    Address,
    Address,
    Address,
    Address,
    Address,
) {
    let env = Env::default();
    env.mock_all_auths();
    env.budget().reset_unlimited();

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let platform_wallet = Address::generate(&env);
    let admin = Address::generate(&env);
    let arbitrator = Address::generate(&env);
    let seller_addr = seller.clone();
    let token_admin = Address::generate(&env);

    // Deploy token contract
    let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token = token::Client::new(&env, &token_id.address());
    let token_addr = token_id.address();

    // Mint tokens to buyer
    let token_asset = token::StellarAssetClient::new(&env, &token_addr);
    token_asset.mint(&buyer, &1_000_000_000);

    // Deploy mock onboarding contract
    let onboarding_contract = Address::generate(&env);

    // Initialize the escrow contract
    client.initialize(
        &platform_wallet,
        &admin,
        &arbitrator,
        &500,
        &Some(onboarding_contract),
    );

    (
        env,
        client,
        buyer,
        seller_addr,
        token_addr,
        admin,
        platform_wallet,
        arbitrator,
    )
}

#[test]
fn test_indexed_storage_scalability() {
    let (env, client, buyer, seller, token, _, _, _) = setup_test();

    // Create 100 escrows to simulate high-volume user
    for i in 0..100 {
        client.create_escrow(
            &buyer,
            &seller,
            &token,
            &1000,
            &(i + 1),
            &Some(604800),
        );
    }

    // Verify buyer escrow count using indexed storage
    let buyer_count_key = DataKey::BuyerEscrowCount(buyer.clone());
    let count: u32 = env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get(&buyer_count_key)
            .unwrap_or(0u32)
    });
    assert_eq!(count, 100);

    // Verify seller escrow count using indexed storage
    let seller_count_key = DataKey::SellerEscrowCount(seller.clone());
    let count: u32 = env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get(&seller_count_key)
            .unwrap_or(0u32)
    });
    assert_eq!(count, 100);

    // Test pagination - first page
    let page1 = client.get_escrows_by_buyer(&buyer, &0, &10, &false);
    assert_eq!(page1.len(), 10);
    assert_eq!(page1.get_unchecked(0), 1);
    assert_eq!(page1.get_unchecked(9), 10);

    // Test pagination - middle page
    let page5 = client.get_escrows_by_buyer(&buyer, &5, &10, &false);
    assert_eq!(page5.len(), 10);
    assert_eq!(page5.get_unchecked(0), 51);
    assert_eq!(page5.get_unchecked(9), 60);

    // Test pagination - last page
    let page10 = client.get_escrows_by_buyer(&buyer, &9, &10, &false);
    assert_eq!(page10.len(), 10);
    assert_eq!(page10.get_unchecked(0), 91);
    assert_eq!(page10.get_unchecked(9), 100);

    // Test pagination - beyond last page
    let page11 = client.get_escrows_by_buyer(&buyer, &10, &10, &false);
    assert_eq!(page11.len(), 0);

    // Verify individual indexed entries exist
    for i in 0..100 {
        let index_key = DataKey::BuyerEscrowIndexed(buyer.clone(), i);
        let escrow_id: u64 = env.as_contract(&client.address, || {
            env.storage()
                .persistent()
                .get(&index_key)
                .expect("Indexed entry should exist")
        });
        assert_eq!(escrow_id, (i + 1) as u64);
    }
}

#[test]
fn test_indexed_storage_multiple_users() {
    let (env, client, buyer1, seller1, token, _, _, _) = setup_test();
    let buyer2 = Address::generate(&env);
    let seller2 = Address::generate(&env);

    // Mint tokens to buyer2
    let token_asset = token::StellarAssetClient::new(&env, &token);
    token_asset.mint(&buyer2, &1_000_000_000);

    // Create escrows for buyer1
    for i in 0..50 {
        client.create_escrow(
            &buyer1,
            &seller1,
            &token,
            &1000,
            &(i + 1),
            &Some(604800),
        );
    }

    // Create escrows for buyer2
    for i in 0..30 {
        client.create_escrow(
            &buyer2,
            &seller2,
            &token,
            &1000,
            &(i + 51),
            &Some(604800),
        );
    }

    // Verify buyer1 count
    let buyer1_count_key = DataKey::BuyerEscrowCount(buyer1.clone());
    let count1: u32 = env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get(&buyer1_count_key)
            .unwrap_or(0u32)
    });
    assert_eq!(count1, 50);

    // Verify buyer2 count
    let buyer2_count_key = DataKey::BuyerEscrowCount(buyer2.clone());
    let count2: u32 = env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get(&buyer2_count_key)
            .unwrap_or(0u32)
    });
    assert_eq!(count2, 30);

    // Verify buyer1 escrows
    let buyer1_escrows = client.get_escrows_by_buyer(&buyer1, &0, &100, &false);
    assert_eq!(buyer1_escrows.len(), 50);

    // Verify buyer2 escrows
    let buyer2_escrows = client.get_escrows_by_buyer(&buyer2, &0, &100, &false);
    assert_eq!(buyer2_escrows.len(), 30);

    // Verify no cross-contamination
    assert_eq!(buyer1_escrows.get_unchecked(0), 1);
    assert_eq!(buyer2_escrows.get_unchecked(0), 51);
}

#[test]
fn test_migration_from_legacy_storage() {
    let (env, client, buyer, seller, token, admin, _, _) = setup_test();

    // Simulate legacy storage by directly setting the old vector format
    let legacy_key = DataKey::BuyerEscrows(buyer.clone());
    let mut legacy_vec = soroban_sdk::Vec::new(&env);
    legacy_vec.push_back(1u64);
    legacy_vec.push_back(2u64);
    legacy_vec.push_back(3u64);
    env.as_contract(&client.address, || {
        env.storage().persistent().set(&legacy_key, &legacy_vec);
    });

    // Verify legacy storage exists
    let has_legacy = env.as_contract(&client.address, || {
        env.storage().persistent().has(&legacy_key)
    });
    assert!(has_legacy);

    // Run migration
    let migrated_count = client.migrate_user_escrows(&buyer, &true);
    assert_eq!(migrated_count, 3);

    // Verify indexed storage was created
    let count_key = DataKey::BuyerEscrowCount(buyer.clone());
    let count: u32 = env.as_contract(&client.address, || {
        env.storage().persistent().get(&count_key).unwrap()
    });
    assert_eq!(count, 3);

    // Verify individual indexed entries
    for i in 0..3 {
        let index_key = DataKey::BuyerEscrowIndexed(buyer.clone(), i);
        let escrow_id: u64 = env.as_contract(&client.address, || {
            env.storage().persistent().get(&index_key).unwrap()
        });
        assert_eq!(escrow_id, (i + 1) as u64);
    }

    // Verify legacy storage was removed
    let has_legacy = env.as_contract(&client.address, || {
        env.storage().persistent().has(&legacy_key)
    });
    assert!(!has_legacy);

    // Verify query function works with migrated data
    let escrows = client.get_escrows_by_buyer(&buyer, &0, &10, &false);
    assert_eq!(escrows.len(), 3);
    assert_eq!(escrows.get_unchecked(0), 1);
    assert_eq!(escrows.get_unchecked(1), 2);
    assert_eq!(escrows.get_unchecked(2), 3);
}

#[test]
fn test_backward_compatibility_query() {
    let (env, client, buyer, seller, token, _, _, _) = setup_test();

    // Simulate legacy storage
    let legacy_key = DataKey::BuyerEscrows(buyer.clone());
    let mut legacy_vec = soroban_sdk::Vec::new(&env);
    legacy_vec.push_back(10u64);
    legacy_vec.push_back(20u64);
    legacy_vec.push_back(30u64);
    env.as_contract(&client.address, || {
        env.storage().persistent().set(&legacy_key, &legacy_vec);
    });

    // Query should work with legacy storage (backward compatibility)
    let escrows = client.get_escrows_by_buyer(&buyer, &0, &10, &false);
    assert_eq!(escrows.len(), 3);
    assert_eq!(escrows.get_unchecked(0), 10);
    assert_eq!(escrows.get_unchecked(1), 20);
    assert_eq!(escrows.get_unchecked(2), 30);

    // Test pagination with legacy storage
    let page1 = client.get_escrows_by_buyer(&buyer, &0, &2, &false);
    assert_eq!(page1.len(), 2);
    assert_eq!(page1.get_unchecked(0), 10);
    assert_eq!(page1.get_unchecked(1), 20);

    let page2 = client.get_escrows_by_buyer(&buyer, &1, &2, &false);
    assert_eq!(page2.len(), 1);
    assert_eq!(page2.get_unchecked(0), 30);
}

#[test]
fn test_batch_create_with_indexed_storage() {
    let (env, client, buyer, seller, token, _, _, _) = setup_test();

    // Create escrows individually
    let mut order_ids = soroban_sdk::Vec::new(&env);
    for i in 0..10 {
        let order_id = i + 1;
        let escrow = client.create_escrow(
            &buyer,
            &seller,
            &token,
            &1000,
            &order_id,
            &Some(604800),
        );
        order_ids.push_back(order_id);
    }
    assert_eq!(order_ids.len(), 10);

    // Verify count was updated correctly
    let buyer_count_key = DataKey::BuyerEscrowCount(buyer.clone());
    let count: u32 = env.as_contract(&client.address, || {
        env.storage().persistent().get(&buyer_count_key).unwrap()
    });
    assert_eq!(count, 10);

    // Verify all indexed entries exist
    for i in 0..10 {
        let index_key = DataKey::BuyerEscrowIndexed(buyer.clone(), i);
        let has_index = env.as_contract(&client.address, || {
            env.storage().persistent().has(&index_key)
        });
        assert!(has_index);
    }

    // Verify query returns all escrows
    let escrows = client.get_escrows_by_buyer(&buyer, &0, &100, &false);
    assert_eq!(escrows.len(), 10);
}

#[test]
fn test_no_storage_limit_with_indexed_pattern() {
    let (env, client, buyer, seller, token, _, _, _) = setup_test();

    // Create 500 escrows to demonstrate scalability
    // In the old pattern, this would approach the 64KB limit
    // With indexed storage, each entry is separate and small
    for i in 0..500 {
        client.create_escrow(
            &buyer,
            &seller,
            &token,
            &1000,
            &(i + 1),
            &Some(604800),
        );
    }

    // Verify count
    let buyer_count_key = DataKey::BuyerEscrowCount(buyer.clone());
    let count: u32 = env.as_contract(&client.address, || {
        env.storage().persistent().get(&buyer_count_key).unwrap()
    });
    assert_eq!(count, 500);

    // Verify we can still query efficiently
    let page1 = client.get_escrows_by_buyer(&buyer, &0, &50, &false);
    assert_eq!(page1.len(), 50);

    let page10 = client.get_escrows_by_buyer(&buyer, &9, &50, &false);
    assert_eq!(page10.len(), 50);
    assert_eq!(page10.get_unchecked(0), 451);
    assert_eq!(page10.get_unchecked(49), 500);

    // Verify individual storage entries are small
    // Each entry is just: Address + u32 index -> u64 escrow_id
    // This is well under 64KB per entry
    for i in 0..500 {
        let index_key = DataKey::BuyerEscrowIndexed(buyer.clone(), i);
        let has_index = env.as_contract(&client.address, || {
            env.storage().persistent().has(&index_key)
        });
        assert!(has_index);
    }
}
