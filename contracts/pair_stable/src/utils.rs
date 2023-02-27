use cosmwasm_std::{Decimal256, Deps, Env, StdResult, Storage, Uint128, Uint64};
use itertools::Itertools;
use std::cmp::Ordering;
use wyndex::oracle::PricePoint;

use wyndex::asset::{AssetInfoValidated, Decimal256Ext, DecimalAsset};
use wyndex::pair::TWAP_PRECISION;

use crate::math::calc_y;
use crate::state::{get_precision, Config};
use wyndex::pair::ContractError;

/// Select offer and ask pools based on given offer and ask infos.
/// This function works with pools with up to 5 assets. Returns (offer_pool, ask_pool) in case of success.
/// If it is impossible to define offer and ask pools, returns [`ContractError`].
///
/// * **offer_asset_info** - asset info of the offer asset.
///
/// * **ask_asset_info** - asset info of the ask asset.
///
/// * **pools** - list of pools.
pub(crate) fn select_pools(
    offer_asset_info: Option<&AssetInfoValidated>,
    ask_asset_info: Option<&AssetInfoValidated>,
    pools: &[DecimalAsset],
) -> Result<(DecimalAsset, DecimalAsset), ContractError> {
    if pools.len() == 2 {
        match (offer_asset_info, ask_asset_info) {
            (Some(offer_asset_info), _) => {
                let (offer_ind, offer_pool) = pools
                    .iter()
                    .find_position(|pool| pool.info.eq(offer_asset_info))
                    .ok_or(ContractError::AssetMismatch {})?;
                Ok((offer_pool.clone(), pools[(offer_ind + 1) % 2].clone()))
            }
            (_, Some(ask_asset_info)) => {
                let (ask_ind, ask_pool) = pools
                    .iter()
                    .find_position(|pool| pool.info.eq(ask_asset_info))
                    .ok_or(ContractError::AssetMismatch {})?;
                Ok((pools[(ask_ind + 1) % 2].clone(), ask_pool.clone()))
            }
            _ => Err(ContractError::VariableAssetMissed {}), // Should always be unreachable
        }
    } else if let (Some(offer_asset_info), Some(ask_asset_info)) =
        (offer_asset_info, ask_asset_info)
    {
        if ask_asset_info.eq(offer_asset_info) {
            return Err(ContractError::SameAssets {});
        }

        let offer_pool = pools
            .iter()
            .find(|pool| pool.info.eq(offer_asset_info))
            .ok_or(ContractError::AssetMismatch {})?;
        let ask_pool = pools
            .iter()
            .find(|pool| pool.info.eq(ask_asset_info))
            .ok_or(ContractError::AssetMismatch {})?;

        Ok((offer_pool.clone(), ask_pool.clone()))
    } else {
        Err(ContractError::VariableAssetMissed {}) // Should always be unreachable
    }
}

/// Compute the current pool amplification coefficient (AMP).
pub(crate) fn compute_current_amp(config: &Config, env: &Env) -> StdResult<Uint64> {
    let block_time = env.block.time.seconds();
    if block_time < config.next_amp_time {
        let elapsed_time: Uint128 = block_time.saturating_sub(config.init_amp_time).into();
        let time_range = config
            .next_amp_time
            .saturating_sub(config.init_amp_time)
            .into();
        let init_amp = Uint128::from(config.init_amp);
        let next_amp = Uint128::from(config.next_amp);

        if next_amp > init_amp {
            let amp_range = next_amp - init_amp;
            let res = init_amp + (amp_range * elapsed_time).checked_div(time_range)?;
            Ok(res.try_into()?)
        } else {
            let amp_range = init_amp - next_amp;
            let res = init_amp - (amp_range * elapsed_time).checked_div(time_range)?;
            Ok(res.try_into()?)
        }
    } else {
        Ok(Uint64::from(config.next_amp))
    }
}

/// Returns a value using a newly specified precision.
///
/// * **value** value that will have its precision adjusted.
///
/// * **current_precision** `value`'s current precision
///
/// * **new_precision** new precision to use when returning the `value`.
pub(crate) fn adjust_precision(
    value: Uint128,
    current_precision: u8,
    new_precision: u8,
) -> StdResult<Uint128> {
    Ok(match current_precision.cmp(&new_precision) {
        Ordering::Equal => value,
        Ordering::Less => value.checked_mul(Uint128::new(
            10_u128.pow((new_precision - current_precision) as u32),
        ))?,
        Ordering::Greater => value.checked_div(Uint128::new(
            10_u128.pow((current_precision - new_precision) as u32),
        ))?,
    })
}

/// Structure for internal use which represents swap result.
pub(crate) struct SwapResult {
    pub return_amount: Uint128,
    pub spread_amount: Uint128,
}

/// Returns the result of a swap in form of a [`SwapResult`] object.
///
/// * **offer_asset** asset that is being offered.
///
/// * **offer_pool** pool of offered asset.
///
/// * **ask_pool** asked asset.
///
/// * **pools** array with assets available in the pool.
pub(crate) fn compute_swap(
    storage: &dyn Storage,
    env: &Env,
    config: &Config,
    offer_asset: &DecimalAsset,
    offer_pool: &DecimalAsset,
    ask_pool: &DecimalAsset,
    pools: &[DecimalAsset],
) -> Result<SwapResult, ContractError> {
    let token_precision = get_precision(storage, &ask_pool.info)?;

    let new_ask_pool = calc_y(
        offer_asset,
        &ask_pool.info,
        offer_pool.amount + offer_asset.amount,
        pools,
        compute_current_amp(config, env)?,
        token_precision,
    )?;

    let return_amount = ask_pool.amount.to_uint128_with_precision(token_precision)? - new_ask_pool;
    let offer_asset_amount = offer_asset
        .amount
        .to_uint128_with_precision(token_precision)?;

    // We consider swap rate 1:1 in stable swap thus any difference is considered as spread.
    let spread_amount = offer_asset_amount.saturating_sub(return_amount);

    Ok(SwapResult {
        return_amount,
        spread_amount,
    })
}

/// Accumulate token prices for the assets in the pool.
/// Returns the array of new prices for the asset combinations in the pool.
/// Empty if the config is still up to date.
///
/// * **pools** array with assets available in the pool *before* the operation.
pub fn accumulate_prices(
    deps: Deps,
    env: &Env,
    config: &mut Config,
    pools: &[DecimalAsset],
) -> Result<bool, ContractError> {
    let block_time = env.block.time.seconds();
    if block_time <= config.block_time_last {
        return Ok(false);
    }

    let time_elapsed = Uint128::from(block_time - config.block_time_last);

    if pools.iter().all(|pool| !pool.amount.is_zero()) {
        let immut_config = config.clone();
        for (from, to, value) in config.cumulative_prices.iter_mut() {
            let offer_asset = DecimalAsset {
                info: from.clone(),
                amount: Decimal256::one(),
            };

            let (offer_pool, ask_pool) = select_pools(Some(from), Some(to), pools)?;
            let SwapResult { return_amount, .. } = compute_swap(
                deps.storage,
                env,
                &immut_config,
                &offer_asset,
                &offer_pool,
                &ask_pool,
                pools,
            )?;

            *value = value.wrapping_add(time_elapsed.checked_mul(adjust_precision(
                return_amount,
                get_precision(deps.storage, &ask_pool.info)?,
                TWAP_PRECISION,
            )?)?);
        }
    }

    config.block_time_last = block_time;

    Ok(true)
}

/// Calculates new prices for the assets in the pool.
/// Returns the array of new prices for the different combinations of assets in the pool or
/// an empty vector if one of the pools is empty.
///
/// * **pools** array with assets available in the pool *after* the latest operation.
pub fn calc_new_prices(
    deps: Deps,
    env: &Env,
    config: &Config,
    pools: &[DecimalAsset],
) -> Result<Vec<PricePoint>, ContractError> {
    if pools.iter().all(|pool| !pool.amount.is_zero()) {
        let mut prices = Vec::with_capacity(config.cumulative_prices.len());
        for (from, to, _) in &config.cumulative_prices {
            let offer_asset = DecimalAsset {
                info: from.clone(),
                amount: Decimal256::one(),
            };

            let (offer_pool, ask_pool) = select_pools(Some(from), Some(to), pools)?;
            let SwapResult { return_amount, .. } = compute_swap(
                deps.storage,
                env,
                config,
                &offer_asset,
                &offer_pool,
                &ask_pool,
                pools,
            )?;

            prices.push(PricePoint::new(from.clone(), to.clone(), return_amount));
        }

        Ok(prices)
    } else {
        Ok(vec![])
    }
}
