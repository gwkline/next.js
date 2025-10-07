use anyhow::{Result, bail};
use rustc_hash::FxHashMap;
use turbo_tasks::{ReadRef, Vc};
use turbo_tasks_hash::hash_xxh3_hash64;

use super::ModuleId;
use crate::ident::AssetIdent;

#[turbo_tasks::value(shared)]
pub enum ModuleIdStrategy {
    Named,
    Global(FxHashMap<AssetIdent, u64>),
}

#[turbo_tasks::value_impl]
impl ModuleIdStrategy {
    #[turbo_tasks::function]
    pub async fn get_module_id(&self, ident: ReadRef<AssetIdent>) -> Result<Vc<ModuleId>> {
        match self {
            ModuleIdStrategy::Named => {
                Ok(ModuleId::String(ident.value_to_string().owned().await?).cell())
            }
            ModuleIdStrategy::Global(module_id_map) => {
                if let Some(module_id) = module_id_map.get(&*ident) {
                    const JS_MAX_SAFE_INTEGER: u64 = (1u64 << 53) - 1;
                    if *module_id > JS_MAX_SAFE_INTEGER {
                        bail!("Numeric module id is too large: {}", module_id);
                    }
                    return Ok(ModuleId::Number(*module_id).cell());
                }

                let ident_string = ident.value_to_string().owned().await?;
                if ident_string.ends_with("[app-client] (ecmascript, next/dynamic entry)") {
                    // TODO: This shouldn't happen, but is a temporary workaround to ignore
                    // next/dynamic imports of a server component from another
                    // server component.
                    return Ok(
                        ModuleId::String(hash_xxh3_hash64(ident_string).to_string().into()).cell(),
                    );
                }

                bail!("ModuleId not found for ident: {ident_string:?}");
            }
        }
    }
}
