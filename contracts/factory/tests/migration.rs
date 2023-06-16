mod factory_helper;

use cosmwasm_std::{to_binary, Addr, Uint128};
use wyndex::asset::{Asset, AssetInfo};
use wyndex::factory::{MigrateMsg, PairType, PartialStakeConfig};

use wyndex_factory::error::ContractError;

use crate::factory_helper::{instantiate_token, FactoryHelper};
use cw_multi_test::{App, ContractWrapper, Executor};

use cw20::Cw20ExecuteMsg;

fn mock_app() -> App {
    App::default()
}

fn store_factory_210_code(app: &mut App) -> u64 {
    let factory_contract = Box::new(
        ContractWrapper::new_with_empty(
            wyndex_factory_2_1_0::contract::execute,
            wyndex_factory_2_1_0::contract::instantiate,
            wyndex_factory_2_1_0::contract::query,
        )
        .with_reply_empty(wyndex_factory_2_1_0::contract::reply)
        .with_migrate_empty(wyndex_factory_2_1_0::contract::migrate),
    );

    app.store_code(factory_contract)
}

#[test]
fn migrate_factory_and_setup_deposit() {
    let mut app = mock_app();

    let owner = Addr::unchecked("owner");

    // store old version of factory
    let factory_code_id = store_factory_210_code(&mut app);

    let mut helper = FactoryHelper::instantiate(&mut app, &owner, Some(factory_code_id));

    let wynd = instantiate_token(&mut app, helper.cw20_token_code_id, &owner, "WYND", None);
    // mint some tokens for random "someone" user
    app.execute_contract(
        owner.clone(),
        wynd.clone(),
        &Cw20ExecuteMsg::Mint {
            recipient: "someone".to_string(),
            amount: Uint128::from(1_000_001u128),
        },
        &[],
    )
    .unwrap();

    let token_instance0 =
        instantiate_token(&mut app, helper.cw20_token_code_id, &owner, "tokenX", None);
    let token_instance1 =
        instantiate_token(&mut app, helper.cw20_token_code_id, &owner, "tokenY", None);
    let token_instance2 =
        instantiate_token(&mut app, helper.cw20_token_code_id, &owner, "tokenZ", None);

    // store new factory
    let factory_contract = Box::new(
        ContractWrapper::new_with_empty(
            wyndex_factory::contract::execute,
            wyndex_factory::contract::instantiate,
            wyndex_factory::contract::query,
        )
        .with_reply_empty(wyndex_factory::contract::reply)
        .with_migrate_empty(wyndex_factory::contract::migrate),
    );
    let new_factory_code_id = app.store_code(factory_contract);

    // only admin can migrate contract
    helper
        .create_pair_with_addr(
            &mut app,
            &Addr::unchecked("someone"),
            PairType::Xyk {},
            [token_instance0.as_str(), token_instance1.as_str()],
            None,
        )
        .unwrap_err();

    // update factory so that everyone can create pools
    helper
        .update_config(&mut app, &owner, None, None, Some(false), None)
        .unwrap();

    // now anyone can create pairs
    helper
        .create_pair_with_addr(
            &mut app,
            &Addr::unchecked("someone"),
            PairType::Xyk {},
            [token_instance0.as_str(), token_instance1.as_str()],
            None,
        )
        .unwrap();

    // Migrate the contract and set the deposit
    app.migrate_contract(
        owner.clone(),
        helper.factory.clone(),
        &MigrateMsg::AddPermissionlessPoolDeposit(Asset {
            info: AssetInfo::Token(wynd.to_string()),
            amount: Uint128::new(1_000_000),
        }),
        new_factory_code_id,
    )
    .unwrap();

    // new version enforces deposit to be send
    let err = helper
        .create_pair_with_addr(
            &mut app,
            &Addr::unchecked("someone"),
            PairType::Xyk {},
            [token_instance1.as_str(), token_instance2.as_str()],
            None,
        )
        .unwrap_err();
    assert_eq!(
        ContractError::PermissionlessRequiresDeposit {},
        err.downcast().unwrap()
    );

    // sent amount is too small
    let err = app
        .execute_contract(
            Addr::unchecked("someone"),
            wynd.clone(),
            &Cw20ExecuteMsg::Send {
                contract: helper.factory.to_string(),
                amount: Uint128::new(1_000),
                msg: to_binary(&wyndex::factory::ExecuteMsg::CreatePair {
                    pair_type: PairType::Xyk {},
                    asset_infos: vec![
                        AssetInfo::Token(token_instance1.to_string()),
                        AssetInfo::Token(token_instance2.to_string()),
                    ],
                    init_params: None,
                    staking_config: PartialStakeConfig::default(),
                    total_fee_bps: None,
                })
                .unwrap(),
            },
            &[],
        )
        .unwrap_err();
    assert_eq!(
        ContractError::DepositRequired(Uint128::new(1_000_000), wynd.to_string()),
        err.downcast().unwrap()
    );

    // sent amount is too big
    let err = app
        .execute_contract(
            Addr::unchecked("someone"),
            wynd.clone(),
            &Cw20ExecuteMsg::Send {
                contract: helper.factory.to_string(),
                amount: Uint128::new(1_000_001),
                msg: to_binary(&wyndex::factory::ExecuteMsg::CreatePair {
                    pair_type: PairType::Xyk {},
                    asset_infos: vec![
                        AssetInfo::Token(token_instance1.to_string()),
                        AssetInfo::Token(token_instance2.to_string()),
                    ],
                    init_params: None,
                    staking_config: PartialStakeConfig::default(),
                    total_fee_bps: None,
                })
                .unwrap(),
            },
            &[],
        )
        .unwrap_err();
    assert_eq!(
        ContractError::DepositRequired(Uint128::new(1_000_000), wynd.to_string()),
        err.downcast().unwrap()
    );

    // creating a new pool works
    app.execute_contract(
        Addr::unchecked("someone"),
        wynd,
        &Cw20ExecuteMsg::Send {
            contract: helper.factory.to_string(),
            amount: Uint128::new(1_000_000),
            msg: to_binary(&wyndex::factory::ExecuteMsg::CreatePair {
                pair_type: PairType::Xyk {},
                asset_infos: vec![
                    AssetInfo::Token(token_instance1.to_string()),
                    AssetInfo::Token(token_instance2.to_string()),
                ],
                init_params: None,
                staking_config: PartialStakeConfig::default(),
                total_fee_bps: None,
            })
            .unwrap(),
        },
        &[],
    )
    .unwrap();
}
