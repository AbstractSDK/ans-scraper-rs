use abstract_core::objects::pool_id::UncheckedPoolAddress;
use abstract_core::objects::PoolMetadata;
use cosmwasm_std::Addr;
use cw_asset::AssetInfo;

pub trait AssetSource {
    fn fetch_asset_infos(&mut self) -> anyhow::Result<Vec<AssetInfo>>;
}

pub trait DexId {
    fn dex_id(&self) -> &'static str;
}

pub trait DexScraper: DexId + AssetSource {
    fn fetch_staking_contracts(&mut self) -> anyhow::Result<Vec<(String, Addr)>>;
    fn fetch_dex_pools(&mut self) -> anyhow::Result<Vec<(UncheckedPoolAddress, PoolMetadata)>>;
}
