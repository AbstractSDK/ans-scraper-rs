use std::collections::{HashMap, HashSet};

use abstract_core::objects::pool_id::UncheckedPoolAddress;
use abstract_core::objects::{AssetEntry, PoolMetadata, PoolType};
use astroport::asset::{AssetInfo, PairInfo};
use astroport::factory::{AstroportFactory, PairType, PairsResponse, QueryMsgFns};
use cosmwasm_std::Addr;
use cw20::{Cw20QueryMsg, TokenInfoResponse};
use cw_asset::AssetInfoUnchecked;
use cw_orch::{queriers::DaemonQuerier, Contract, ContractInstance, CwEnv, Daemon};
use reqwest::Error;

use crate::helpers::chain_registry::ChainRegistry;
use crate::traits::dex::{AssetSource, DexId, DexScraper};

const ASTROPORT_PHOENIX_ADDRS: &str = "https://raw.githubusercontent.com/astroport-fi/astroport-changelog/main/terra-2/phoenix-1/core_phoenix.json";
const ASTROPORT_PISCO_ADDRS: &str = "https://raw.githubusercontent.com/astroport-fi/astroport-changelog/main/terra-2/pisco-1/core_pisco.json";

const ASTROPORT_DEX: &str = "astroport";

pub struct AstroportScraper<Chain: CwEnv> {
    chain: Chain,
    chain_ans_prefix: String,
    chain_registry: ChainRegistry,
    factory: AstroportFactory<Chain>,
    _all_pairs: Vec<PairInfo>,
    asset_info_to_name: HashMap<AssetInfo, String>,
}

impl<T: cw_orch::TxHandler> DexId for AstroportScraper<T> {
    fn dex_id(&self) -> &'static str {
        ASTROPORT_DEX
    }
}

impl AstroportScraper<Daemon> {
    pub async fn new(chain: Daemon, chain_ans_prefix: &str) -> Self {
        let factory_address =
            Self::fetch_deployment_address(chain.state.chain_id.as_str(), "factory_address")
                .await
                .unwrap();

        let mut factory =
            astroport::factory::AstroportFactory::new("astroport:factory", chain.clone());
        factory
            .as_instance_mut()
            .set_address(&Addr::unchecked(factory_address));

        Self {
            chain,
            // TODO: elsewhere
            chain_ans_prefix: chain_ans_prefix.to_string(),
            chain_registry: ChainRegistry::new().await.unwrap(),
            factory,
            _all_pairs: vec![],
            asset_info_to_name: HashMap::new(),
        }
    }

    fn all_pairs(&mut self) -> anyhow::Result<Vec<PairInfo>> {
        // Fetch pairs if not already done
        if self._all_pairs.is_empty() {
            let mut all_pairs = vec![];
            let mut start_after_pair = None;
            loop {
                let PairsResponse { mut pairs } = self.factory.pairs(None, start_after_pair)?;
                if pairs.is_empty() {
                    break;
                }
                all_pairs.append(&mut pairs);
                start_after_pair = all_pairs.last().map(|p| p.asset_infos.to_vec());
            }
            self._all_pairs = all_pairs;
        }

        Ok(self._all_pairs.clone())
    }

    fn all_asset_infos(&mut self) -> anyhow::Result<HashSet<AssetInfo>> {
        Ok(self
            .all_pairs()?
            .iter()
            .flat_map(|p| p.asset_infos.to_vec())
            .collect())
    }

    async fn fetch_deployment_address(chain_id: &str, key: &str) -> Result<String, Error> {
        let url = match chain_id {
            "phoenix-1" => ASTROPORT_PHOENIX_ADDRS,
            "pisco-1" => ASTROPORT_PISCO_ADDRS,
            _ => panic!("Network not supported"),
        };

        let response_text = reqwest::get(url).await?.text().await?;

        let lines = response_text.lines().collect::<Vec<_>>();
        let mut json_map = HashMap::new();

        // We parse the json manually because the astroport team does not ensure that their json is incorrect 🙃
        for line in lines {
            if line.trim().is_empty()
                || line.trim().starts_with('{')
                || line.trim().starts_with('}')
            {
                continue;
            }

            let parts = line.split(':').collect::<Vec<_>>();
            if parts.len() == 2 {
                let key = parts[0].trim().trim_matches('"').to_string();
                let value = parts[1]
                    .trim()
                    .trim_matches(',')
                    .trim_matches('"')
                    .to_string();
                json_map.insert(key, value);
            }
        }

        let key_address = json_map
            .get(key)
            .unwrap_or_else(|| panic!("{} not found in JSON", key));

        Ok(key_address.to_string())
    }
}

impl AssetSource for AstroportScraper<Daemon> {
    fn fetch_asset_infos(&mut self) -> anyhow::Result<Vec<(String, AssetInfoUnchecked)>> {
        let mut not_found_assets = vec![];

        let mut ans_assets_to_add = Vec::<(String, AssetInfoUnchecked)>::new();

        for asset_info in self.all_asset_infos()? {
            let (name, unchecked_info) = match &asset_info {
                AssetInfo::Token { contract_addr } => {
                    if let Ok(entry) = cw20_asset_entry(
                        self.chain.clone(),
                        self.chain_ans_prefix.as_str(),
                        contract_addr,
                    ) {
                        (entry, AssetInfoUnchecked::cw20(contract_addr.clone()))
                    } else {
                        not_found_assets.push(asset_info.clone());
                        continue;
                    }
                }
                AssetInfo::NativeToken { denom } => {
                    if let Some(entry) = self.chain.rt_handle.block_on(
                        self.chain_registry
                            .resolve_native_asset(self.chain.clone(), denom.clone()),
                    ) {
                        (entry, AssetInfoUnchecked::native(denom.clone()))
                    } else {
                        not_found_assets.push(asset_info.clone());
                        continue;
                    }
                }
            };

            self.asset_info_to_name
                .insert(asset_info.clone(), name.clone());
            ans_assets_to_add.push((name, unchecked_info));
        }

        Ok(ans_assets_to_add)
    }
}

impl DexScraper for AstroportScraper<Daemon> {
    fn fetch_staking_contracts(&mut self) -> anyhow::Result<Vec<(String, Addr)>> {
        Ok(vec![])
    }

    fn fetch_dex_pools(&mut self) -> anyhow::Result<Vec<(UncheckedPoolAddress, PoolMetadata)>> {
        let mut ans_pools_to_add = Vec::<(UncheckedPoolAddress, PoolMetadata)>::new();
        let mut skipped_ans_pools = vec![];

        for pair in self.all_pairs()? {
            let pool_id = UncheckedPoolAddress::contract(pair.contract_addr);

            let pool_type = match pair.pair_type {
                PairType::Stable {} => PoolType::Stable,
                PairType::Xyk {} => PoolType::ConstantProduct,
                PairType::Concentrated {} => PoolType::Weighted,
                PairType::Custom(_) => panic!("Custom pair type not supported"),
            };

            let mut assets = vec![];
            let mut missing_asset = false;

            for asset_info in &pair.asset_infos {
                if let Some(name) = self.asset_info_to_name.get(asset_info) {
                    assets.push(AssetEntry::from(name.clone()));
                } else {
                    missing_asset = true;
                    break;
                }
            }

            if missing_asset {
                skipped_ans_pools.push(pool_id.clone());
                continue;
            }

            let pool_metadata = PoolMetadata {
                dex: ASTROPORT_DEX.to_string(),
                pool_type,
                assets,
            };
            ans_pools_to_add.push((pool_id, pool_metadata));
        }

        Ok(ans_pools_to_add)
    }
}

/// Fetch a given cw20 asset entry for the chain.
/// TODO: move somewhere
fn cw20_asset_entry(
    chain: Daemon,
    chain_ans_prefix: &str,
    contract_addr: &Addr,
) -> anyhow::Result<String> {
    let cw20 =
        Contract::new(contract_addr.clone().as_str(), chain).with_address(Some(contract_addr));

    // get the name
    let info: TokenInfoResponse = cw20.query(&Cw20QueryMsg::TokenInfo {})?;

    let name = info.symbol.to_ascii_lowercase();
    Ok(format!("{}>{}", chain_ans_prefix, name))
}
