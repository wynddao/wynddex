use anyhow::Result as AnyResult;

use cosmwasm_std::{assert_approx_eq, to_binary, Addr, Decimal, StdResult, Uint128};
use cw20::{Cw20Coin, Cw20ExecuteMsg, MinterResponse};
use cw20_base::msg::InstantiateMsg as Cw20InstantiateMsg;

use cw_multi_test::{App, AppResponse, ContractWrapper, Executor};
use wynd_curve_utils::{Curve, SaturatingLinear};
use wyndex::{
    asset::{AssetInfo, AssetInfoExt, AssetInfoValidated, AssetValidated},
    stake::{InstantiateMsg, UnbondingPeriod},
};

use crate::msg::{
    ExecuteMsg, MigrateMsg, QueryMsg, ReceiveDelegationMsg, WithdrawableRewardsResponse,
};

pub const SEVEN_DAYS: u64 = 604800;

fn contract_stake_v112(app: &mut App) -> u64 {
    let contract = Box::new(
        ContractWrapper::new_with_empty(
            wyndex_stake_v112::contract::execute,
            wyndex_stake_v112::contract::instantiate,
            wyndex_stake_v112::contract::query,
        )
        .with_migrate_empty(wyndex_stake_v112::contract::migrate),
    );

    app.store_code(contract)
}

fn contract_stake(app: &mut App) -> u64 {
    let contract = Box::new(
        ContractWrapper::new_with_empty(
            crate::contract::execute,
            crate::contract::instantiate,
            crate::contract::query,
        )
        .with_migrate_empty(crate::contract::migrate),
    );

    app.store_code(contract)
}

pub(super) fn contract_token(app: &mut App) -> u64 {
    let contract = Box::new(ContractWrapper::new_with_empty(
        cw20_base::contract::execute,
        cw20_base::contract::instantiate,
        cw20_base::contract::query,
    ));

    app.store_code(contract)
}

#[derive(Debug)]
pub struct SuiteBuilder {
    pub cw20_contract: String,
    pub tokens_per_power: Uint128,
    pub min_bond: Uint128,
    pub unbonding_periods: Vec<UnbondingPeriod>,
    pub admin: Option<String>,
    pub initial_balances: Vec<Cw20Coin>,
    pub canlab_initial_balances: Vec<Cw20Coin>,
}

impl SuiteBuilder {
    pub fn new() -> Self {
        Self {
            cw20_contract: "".to_owned(),
            tokens_per_power: Uint128::new(1000),
            min_bond: Uint128::new(5000),
            unbonding_periods: vec![SEVEN_DAYS],
            admin: None,
            initial_balances: vec![],
            canlab_initial_balances: vec![],
        }
    }

    pub fn with_admin(mut self, admin: &str) -> Self {
        self.admin = Some(admin.to_owned());
        self
    }

    pub fn with_initial_balances(mut self, balances: Vec<(&str, u128)>) -> Self {
        let initial_balances = balances
            .into_iter()
            .map(|(address, amount)| Cw20Coin {
                address: address.to_owned(),
                amount: amount.into(),
            })
            .collect::<Vec<Cw20Coin>>();
        self.initial_balances = initial_balances;
        self
    }

    pub fn with_canlab_initial_balances(mut self, balances: Vec<(&str, u128)>) -> Self {
        let initial_balances = balances
            .into_iter()
            .map(|(address, amount)| Cw20Coin {
                address: address.to_owned(),
                amount: amount.into(),
            })
            .collect::<Vec<Cw20Coin>>();
        self.canlab_initial_balances = initial_balances;
        self
    }

    pub fn with_unbonding_periods(mut self, unbonding_periods: Vec<UnbondingPeriod>) -> Self {
        self.unbonding_periods = unbonding_periods;
        self
    }

    #[track_caller]
    pub fn build(self) -> Suite {
        let mut app: App = App::default();
        let admin = Addr::unchecked("admin");

        let token_id = contract_token(&mut app);

        // Instantiate CANLAB token contract
        let canlab_token_contract = app
            .instantiate_contract(
                token_id,
                admin.clone(),
                &Cw20InstantiateMsg {
                    name: "canlab".to_owned(),
                    symbol: "CANLAB".to_owned(),
                    decimals: 3,
                    initial_balances: self.canlab_initial_balances,
                    mint: Some(MinterResponse {
                        minter: "minter".to_owned(),
                        cap: None,
                    }),
                    marketing: None,
                },
                &[],
                "canlab",
                None,
            )
            .unwrap();

        let token_contract = app
            .instantiate_contract(
                token_id,
                admin.clone(),
                &Cw20InstantiateMsg {
                    name: "vesting".to_owned(),
                    symbol: "VEST".to_owned(),
                    decimals: 9,
                    initial_balances: self.initial_balances,
                    mint: Some(MinterResponse {
                        minter: "minter".to_owned(),
                        cap: None,
                    }),
                    marketing: None,
                },
                &[],
                "vesting",
                None,
            )
            .unwrap();

        // Instantiate original staking contract
        let stake_id = contract_stake_v112(&mut app);
        let stake_contract = app
            .instantiate_contract(
                stake_id,
                admin.clone(),
                &InstantiateMsg {
                    cw20_contract: token_contract.to_string(),
                    tokens_per_power: self.tokens_per_power,
                    min_bond: self.min_bond,
                    unbonding_periods: self.unbonding_periods,
                    admin: self.admin,
                    unbonder: Some("unbonder".to_owned()),
                    max_distributions: 6,
                },
                &[],
                "stake",
                Some(admin.to_string()),
            )
            .unwrap();

        Suite {
            app,
            stake_contract,
            token_contract,
            canlab_token_contract,
        }
    }
}

pub struct Suite {
    pub app: App,
    stake_contract: Addr,
    token_contract: Addr,
    canlab_token_contract: Addr,
}

impl Suite {
    // update block's time to simulate passage of time
    pub fn update_seconds(&mut self, time_update: u64) {
        let mut block = self.app.block_info();
        block.time = block.time.plus_seconds(time_update);
        self.app.set_block(block);
    }

    // create a new distribution flow for staking
    pub fn create_distribution_flow(
        &mut self,
        sender: &str,
        manager: &str,
        asset: AssetInfo,
        rewards: Vec<(UnbondingPeriod, Decimal)>,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(sender),
            self.stake_contract.clone(),
            &ExecuteMsg::CreateDistributionFlow {
                manager: manager.to_string(),
                asset,
                rewards,
            },
            &[],
        )
    }

    // call to staking contract by sender
    pub fn execute_fund_distribution_with_cw20(
        &mut self,
        executor: &str,
        funds: AssetValidated,
        curve: Curve,
        token: Addr,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(executor),
            token,
            &Cw20ExecuteMsg::Send {
                contract: self.stake_contract.to_string(),
                amount: funds.amount,
                msg: to_binary(&ReceiveDelegationMsg::Fund { curve })?,
            },
            &[],
        )
    }

    pub fn increase_allowance_for_stake_contract(
        &mut self,
        token: &str,
        sender: &str,
        amount: u128,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(sender),
            Addr::unchecked(token),
            &Cw20ExecuteMsg::IncreaseAllowance {
                spender: self.stake_contract.to_string(),
                amount: amount.into(),
                expires: None,
            },
            &[],
        )
    }

    pub fn distribute_rewards(&mut self, sender: &str) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(sender),
            self.stake_contract.clone(),
            &ExecuteMsg::DistributeRewards { sender: None },
            &[],
        )
    }

    // call to staking contract by sender
    pub fn delegate(
        &mut self,
        sender: &str,
        amount: u128,
        unbonding_period: u64,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(sender),
            self.token_contract.clone(),
            &Cw20ExecuteMsg::Send {
                contract: self.stake_contract.to_string(),
                amount: amount.into(),
                msg: to_binary(&ReceiveDelegationMsg::Delegate {
                    unbonding_period,
                    delegate_as: Some(sender.to_owned()),
                })?,
            },
            &[],
        )
    }

    // create a new distribution flow for staking
    pub fn migrate(
        &mut self,
        sender: &str,
        contract: Addr,
        code_id: u64,
        canlab_token_contract: &str,
    ) -> AnyResult<AppResponse> {
        self.app.migrate_contract(
            Addr::unchecked(sender),
            contract,
            &MigrateMsg {
                canlab_token_contract: canlab_token_contract.to_owned(),
            },
            code_id,
        )
    }

    pub fn query_withdrawable_rewards(&self, owner: &str) -> StdResult<Vec<AssetValidated>> {
        let resp: WithdrawableRewardsResponse = self.app.wrap().query_wasm_smart(
            self.stake_contract.clone(),
            &QueryMsg::WithdrawableRewards {
                owner: owner.to_owned(),
            },
        )?;
        Ok(resp.rewards)
    }
}

#[test]
fn migrate_existing_distribution_curve() {
    let sponsor = "sponsor".to_owned();
    let user = "user".to_owned();
    let unbonding_period = 1000u64;
    let distribution = 50_000_000u128;

    let mut suite = SuiteBuilder::new()
        .with_admin("admin")
        .with_unbonding_periods(vec![unbonding_period])
        // simulates user having tokens to stake
        .with_initial_balances(vec![(&user, 100_000)])
        .with_canlab_initial_balances(vec![(&sponsor, distribution)])
        .build();

    let canlab_token_contract = suite.canlab_token_contract.clone();

    suite
        .increase_allowance_for_stake_contract(
            canlab_token_contract.as_str(),
            &sponsor,
            distribution,
        )
        .unwrap();
    suite
        .create_distribution_flow(
            "admin",
            &sponsor,
            AssetInfo::Token(canlab_token_contract.to_string()),
            vec![(unbonding_period, Decimal::one())],
        )
        .unwrap();

    suite.delegate(&user, 100_000, unbonding_period).unwrap();

    // Fund both distribution flows
    suite
        .execute_fund_distribution_with_cw20(
            &sponsor,
            AssetInfoValidated::Token(canlab_token_contract.clone()).with_balance(distribution),
            Curve::SaturatingLinear(SaturatingLinear {
                min_x: 1676329200,
                min_y: Uint128::new(distribution),
                max_x: 1707865200,
                max_y: Uint128::zero(),
            }),
            canlab_token_contract.clone(),
        )
        .unwrap();

    // 10% of seconds in year
    suite.update_seconds(3_153_600);

    // because of incorrect curve, no rewards are being distributed
    suite.distribute_rewards(&sponsor).unwrap();
    assert_eq!(
        suite.query_withdrawable_rewards(&user).unwrap(),
        vec![AssetInfoValidated::Token(canlab_token_contract.clone()).with_balance(0u128)]
    );

    // store new version
    let new_code_id = contract_stake(&mut suite.app);
    // it maps current configuration on mainnet
    suite
        .migrate(
            "admin",
            suite.stake_contract.clone(),
            new_code_id,
            canlab_token_contract.as_str(),
        )
        .unwrap();

    // Now curve is fixed and started distributing rewards
    suite.update_seconds(3_153_600);

    suite.distribute_rewards(&sponsor).unwrap();
    // some rewards started to get distributed
    assert_approx_eq!(
        suite.query_withdrawable_rewards(&user).unwrap()[0].amount,
        Uint128::new(5_000_000u128),
        "0.00001",
        "10% of whole distribution, more or less"
    );
}
