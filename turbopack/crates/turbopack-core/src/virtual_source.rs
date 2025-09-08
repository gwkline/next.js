use turbo_tasks::{ResolvedVc, Vc};
use turbo_tasks_fs::FileSystemPath;

use crate::{
    asset::{Asset, AssetContent},
    ident::AssetIdent,
    source::Source,
};

/// A [Source] that is created from some passed source code.
#[turbo_tasks::value]
pub struct VirtualSource {
    pub ident: ResolvedVc<AssetIdent>,
    pub content: ResolvedVc<AssetContent>,
}
impl VirtualSource {
    pub fn new(path: FileSystemPath, content: Vc<AssetContent>) -> Vc<Self> {
        VirtualSource::new_with_ident(AssetIdent::from_path(path), content)
    }
}

#[turbo_tasks::value_impl]
impl VirtualSource {
    #[turbo_tasks::function]
    pub fn new_with_ident(ident: AssetIdent, content: ResolvedVc<AssetContent>) -> Vc<Self> {
        Self::cell(VirtualSource {
            ident: ident.resolved_cell(),
            content,
        })
    }
}

#[turbo_tasks::value_impl]
impl Source for VirtualSource {
    #[turbo_tasks::function]
    fn ident(&self) -> Vc<AssetIdent> {
        *self.ident
    }
}

#[turbo_tasks::value_impl]
impl Asset for VirtualSource {
    #[turbo_tasks::function]
    fn content(&self) -> Vc<AssetContent> {
        *self.content
    }
}
