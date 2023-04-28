use std::collections::{HashMap, HashSet};

use std::path::Path;



use abstract_core::{
    objects::{
        PoolMetadata,
        PoolType,
        pool_id::UncheckedPoolAddress
    },
};
use abstract_core::objects::AssetEntry;
use abstract_core::objects::oracle::Oracle;

use astroport::{
    asset::AssetInfo,
    factory::{PairsResponse, PairType, QueryMsgFns}
};
use clap::Parser;
use cosmwasm_std::{Addr};
use cw20::TokenInfoResponse;
use cw_asset::AssetInfoUnchecked;
use cw20_base_orch::msg::{QueryMsg as Cw20QueryMsg};


use cw_orch::{ContractInstance, networks::{ChainInfo}, Daemon, networks::parse_network, queriers::DaemonQuerier, queriers::Ibc, Contract};

use reqwest::Error;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

use ibc_chain_registry::asset_list::{Asset, AssetList, DenomUnit};

use ibc_chain_registry::constants::ALL_CHAINS;

use ibc_chain_registry::fetchable::Fetchable;

pub const ABSTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Serialize, Deserialize, Debug)]
struct JSONResponse(HashMap<String, String>);

pub async fn fetch_astroport_address(url: &str, key: &str) -> Result<String, Error> {
    let response_text = reqwest::get(url).await?.text().await?;

    let lines = response_text.lines().collect::<Vec<_>>();
    let mut json_map = HashMap::new();

    // We parse the json manually because the astroport team does not ensure that their json is incorrect 🙃
    for line in lines {
        if line.trim().is_empty() || line.trim().starts_with('{') || line.trim().starts_with('}') {
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

const ASTROPORT_PHOENIX_ADDRS: &str = "https://raw.githubusercontent.com/astroport-fi/astroport-changelog/main/terra-2/phoenix-1/core_phoenix.json";
const ASTROPORT_PISCO_ADDRS: &str = "https://raw.githubusercontent.com/astroport-fi/astroport-changelog/main/terra-2/pisco-1/core_pisco.json";

const DEX_NAME: &str = "astroport";

async fn fetch_asset_lists() -> anyhow::Result<Vec<AssetList>> {
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
            let list: AssetList = serde_json::from_str(&json)?;
            lists.push(list);
            continue;
        }

        let list = AssetList::fetch(chain.to_string(), None).await.ok().unwrap();
        let json = serde_json::to_string(&list)?;
        std::fs::write(file_name, json)?;
        lists.push(list);
    }

    Ok(lists)
}

/// Script that registers the first Account in abstract (our Account)
pub fn astroport_ans(network: ChainInfo) -> anyhow::Result<()> {
    // let network = LOCAL_JUNO;
    let rt = Runtime::new()?;

    let chain = Daemon::builder()
        .chain(network.clone())
        .handle(rt.handle())
        .build()
        .unwrap();

    let chain_ans_prefix = match network.chain_id {
        "phoenix-1" => "terra2",
        "pisco-1" => "terra2",
        _ => panic!("Network not supported"),
    };

    let asset_lists = rt.block_on(fetch_asset_lists())?;

    let _ibc = Ibc::new(chain.state.grpc_channel.clone());

    let url = match network.chain_id {
        "phoenix-1" => ASTROPORT_PHOENIX_ADDRS,
        "pisco-1" => ASTROPORT_PISCO_ADDRS,
        _ => panic!("Network not supported"),
    };

    let factory_address = rt.block_on(fetch_astroport_address(url, "factory_address"))?;

    let mut astro_factory =
        astroport::factory::AstroportFactory::new("astroport:factory", chain.clone());
    astro_factory
        .as_instance_mut()
        .set_address(&Addr::unchecked(factory_address));

    let mut all_pairs = vec![];
    let mut start_after_pair = None;
    loop {
        let PairsResponse { mut pairs } = astro_factory.pairs(None, start_after_pair)?;
        if pairs.is_empty() {
            break;
        }
        all_pairs.append(&mut pairs);
        start_after_pair = all_pairs.last().map(|p| p.asset_infos.to_vec());
    }

    let all_asset_infos: HashSet<AssetInfo> = all_pairs.clone()
        .iter()
        .flat_map(|p| p.asset_infos.to_vec())
        .collect();

    let mut asset_info_to_name: HashMap<AssetInfo, String> = HashMap::new();

    let mut not_found_assets = vec![];

    let mut ans_assets_to_add = Vec::<(String, AssetInfoUnchecked)>::new();

    for asset_info in all_asset_infos {
        let (name, unchecked_info) = match &asset_info {
            AssetInfo::Token { contract_addr } => {
                continue;
                if let Ok(entry) = cw20_asset_entry(chain.clone(), chain_ans_prefix, contract_addr) {
                    (entry, AssetInfoUnchecked::cw20(contract_addr.clone()))
                } else {
                    not_found_assets.push(asset_info.clone());
                    continue;
                }
            }
            AssetInfo::NativeToken { denom } => {
                if let Some(entry) = rt.block_on(native_asset_entry(chain.clone(), denom.clone(), &asset_lists)) {
                    (entry, AssetInfoUnchecked::native(denom.clone()))
                } else {
                    not_found_assets.push(asset_info.clone());
                    continue;
                }
            }
        };

        asset_info_to_name.insert(asset_info.clone(), name.clone());
        ans_assets_to_add.push((name, unchecked_info));
    }

    // println!("Not found assets: {:?}", not_found_assets);
    println!("To add: {:?}", ans_assets_to_add);

    let mut ans_pools_to_add = Vec::<(UncheckedPoolAddress, PoolMetadata)>::new();
    let mut skipped_ans_pools = vec![];

    for pair in all_pairs {
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
            if let Some(name) = asset_info_to_name.get(asset_info) {
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
            dex: DEX_NAME.to_string(),
            pool_type,
            assets,
        };
        ans_pools_to_add.push((pool_id, pool_metadata));
    }


    Ok(())
}

fn cw20_asset_entry(chain: Daemon, chain_ans_prefix: &str, contract_addr: &Addr) -> anyhow::Result<String> {
    let cw20 = Contract::new(contract_addr.clone().as_str(), chain).with_address(Some(contract_addr));

    // get the name
    let info: TokenInfoResponse = cw20.query(&Cw20QueryMsg::TokenInfo {})?;

    let name = info.symbol.to_ascii_lowercase();
    Ok(format!("{}>{}", chain_ans_prefix, name))
}

async fn native_asset_entry(chain: Daemon, denom: String, asset_lists: &[AssetList]) -> Option<String> {
    let ibc = Ibc::new(chain.state.grpc_channel.clone());

    let denom_trace = ibc.denom_trace(denom.clone()).await.ok()?;

    log::info!("Denom trace for {}: {:?}", denom, denom_trace);

    let path = denom_trace.path;
    let mut path_parts = path.split('/');

    let port_id = path_parts.next().unwrap();

    if port_id != "transfer" {
        log::warn!("Denom trace path for {} is not transfer, but {}", denom, port_id);
        return None;
    }

    let base_denom = denom_trace.base_denom;

    log::info!("Base denom for {} is {}", denom, base_denom);

    for asset_list in asset_lists {
        let AssetList { chain_name, assets, .. } = asset_list;

        if let Some(matching_asset) = assets.iter().find(|asset| {
            asset.denom_units.iter().any(|denom_unit| denom_unit.denom == base_denom)
        }) {
            return Some(format!("{}>{}", chain_name.to_ascii_lowercase(), matching_asset.symbol.to_ascii_lowercase()));
        }
    }

    None
}


#[derive(Parser, Default, Debug)]
#[command(author, version, about, long_about = None)]
struct Arguments {
    /// Network Id to deploy on
    #[arg(short, long)]
    network_id: String,
}

fn main() {
    dotenv().ok();
    env_logger::init();

    use dotenv::dotenv;

    let args = Arguments::parse();

    let network = parse_network(&args.network_id);

    if let Err(ref err) = astroport_ans(network) {
        log::error!("{}", err);
        err.chain()
            .skip(1)
            .for_each(|cause| log::error!("because: {}", cause));

        // The backtrace is not always generated. Try to run this example
        // with `$env:RUST_BACKTRACE=1`.
        //    if let Some(backtrace) = e.backtrace() {
        //        log::debug!("backtrace: {:?}", backtrace);
        //    }

        ::std::process::exit(1);
    }
}
