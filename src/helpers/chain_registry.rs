use cw_orch::queriers::{DaemonQuerier, Ibc};
use cw_orch::Daemon;
use ibc_chain_registry::asset_list::{
    Asset as ChainRegistryAsset, AssetList as ChainRegistryAssetList,
};
use ibc_chain_registry::constants::ALL_CHAINS;
use ibc_chain_registry::fetchable::Fetchable;
use std::path::Path;

/// THe chain registry somewhat acts like a singleton by caching all its data locally.
pub struct ChainRegistry {
    asset_lists: Vec<ChainRegistryAssetList>,
}

impl ChainRegistry {
    // TOOD: new name? ??
    pub async fn new() -> anyhow::Result<Self> {
        let asset_lists = Self::fetch_asset_lists().await?;
        Ok(Self { asset_lists })
    }

    async fn fetch_asset_lists() -> anyhow::Result<Vec<ChainRegistryAssetList>> {
        log::info!("Fetching asset lists from the chain registry");
        // check for cache dir
        if !Path::new("cache/asset_lists").exists() {
            std::fs::create_dir_all("cache/asset_lists")?;
        }

        let mut lists = Vec::with_capacity(ALL_CHAINS.len());
        for chain in ALL_CHAINS {
            // check cache
            let file_name = format!("cache/asset_lists/{}.json", chain);
            if Path::new(&file_name).exists() {
                let json = std::fs::read_to_string(file_name)?;
                let list: ChainRegistryAssetList = serde_json::from_str(&json)?;
                lists.push(list);
                continue;
            }

            let list = ChainRegistryAssetList::fetch(chain.to_string(), None)
                .await
                .ok()
                .unwrap();
            let json = serde_json::to_string(&list)?;
            std::fs::write(file_name, json)?;
            lists.push(list);
        }

        Ok(lists)
    }

    /// Get the asset lists from the chain registry.
    pub fn get_asset_lists(&self) -> &[ChainRegistryAssetList] {
        &self.asset_lists
    }

    pub async fn resolve_native_asset(&self, chain: Daemon, denom: String) -> Option<String> {
        let ibc = Ibc::new(chain.state.grpc_channel.clone());

        let denom_trace = ibc.denom_trace(denom.clone()).await.ok()?;

        log::info!("Denom trace for {}: {:?}", denom, denom_trace);

        let path = denom_trace.path;
        let mut path_parts = path.split('/');

        let port_id = path_parts.next().unwrap();

        if port_id != "transfer" {
            log::warn!(
                "Denom trace path for {} is not transfer, but {}",
                denom,
                port_id
            );
            return None;
        }

        let base_denom = denom_trace.base_denom;

        log::info!("Base denom for {} is {}", denom, base_denom);

        for asset_list in self.asset_lists.clone() {
            let ChainRegistryAssetList {
                chain_name, assets, ..
            } = asset_list;

            if let Some(matching_asset) = assets.iter().find(|asset| {
                asset
                    .denom_units
                    .iter()
                    .any(|denom_unit| denom_unit.denom == base_denom)
            }) {
                return Some(format!(
                    "{}>{}",
                    chain_name.to_ascii_lowercase(),
                    matching_asset.symbol.to_ascii_lowercase()
                ));
            }
        }

        None
    }

    pub fn asset_by_denom(&self, denom: String) -> Option<ChainRegistryAsset> {
        for asset_list in self.asset_lists.clone() {
            let ChainRegistryAssetList { assets, .. } = asset_list;

            if let Some(matching_asset) = assets.iter().find(|asset| {
                asset
                    .denom_units
                    .iter()
                    .any(|denom_unit| denom_unit.denom == denom)
            }) {
                return Some(matching_asset.clone());
            }
        }

        None
    }
}
