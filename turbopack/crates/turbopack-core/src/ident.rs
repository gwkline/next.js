use std::{fmt::Write, mem::take};

use anyhow::Result;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use turbo_rcstr::RcStr;
use turbo_tasks::{NonLocalValue, TaskInput, Vc, trace::TraceRawVcs};
use turbo_tasks_fs::FileSystemPath;
use turbo_tasks_hash::{DeterministicHash, Xxh3Hash64Hasher, encode_hex, hash_xxh3_hash64};

use crate::resolve::ModulePart;

/// A layer identifies a distinct part of the module graph.
#[derive(
    Clone,
    TaskInput,
    Hash,
    Debug,
    DeterministicHash,
    Eq,
    PartialEq,
    TraceRawVcs,
    Serialize,
    Deserialize,
    NonLocalValue,
)]
pub struct Layer {
    name: RcStr,
    user_friendly_name: Option<RcStr>,
}

impl Layer {
    pub fn new(name: RcStr) -> Self {
        debug_assert!(!name.is_empty());
        Self {
            name,
            user_friendly_name: None,
        }
    }
    pub fn new_with_user_friendly_name(name: RcStr, user_friendly_name: RcStr) -> Self {
        debug_assert!(!name.is_empty());
        debug_assert!(!user_friendly_name.is_empty());
        Self {
            name,
            user_friendly_name: Some(user_friendly_name),
        }
    }

    /// Returns a user friendly name for this layer
    pub fn user_friendly_name(&self) -> &RcStr {
        self.user_friendly_name.as_ref().unwrap_or(&self.name)
    }

    pub fn name(&self) -> &RcStr {
        &self.name
    }
}

// TODO: In a large build there are many 10s of thousands of AssetIdents and they get cloned a lot
// on top of that. Most of the data is in RcStr instances which is cheap to clone but the raw struct
// is large.  Consider ways to 'compress' the size of the struct.
//
// * Eagerly flatten things like 'assets', 'modifiers', query, fragment into a single string.  Many
//   of these are 'write-only' so we can use that to our advantage.
// * model it as an Arc<AssetIdent> to make it cheaper to clone.
// * store the vecs as Option<ThinArc<T>> to make it smaller and cheaper to clone since they are
//   usually empty or you are just modifying one of them.

#[turbo_tasks::value(shared)]
#[derive(Clone, Debug, Hash, TaskInput)]
pub struct AssetIdent {
    /// The primary path of the asset
    pub path: FileSystemPath,
    /// The query string of the asset this is either the empty string or a query string that starts
    /// with a `?` (e.g. `?foo=bar`)
    pub query: RcStr,
    /// The fragment of the asset, this is either the empty string or a fragment string that starts
    /// with a `#` (e.g. `#foo`)
    pub fragment: RcStr,
    /// The assets that are nested in this asset
    /// Formatted as a sequence of `key => asset` pairs
    pub assets: RcStr,
    /// The modifiers of this asset (e.g. `client chunks`)
    pub modifiers: Vec<RcStr>,
    /// The parts of the asset that are (ECMAScript) modules
    pub parts: Vec<ModulePart>,
    /// The asset layer the asset was created from.
    pub layer: Option<Layer>,
    /// The MIME content type, if this asset was created from a data URL.
    pub content_type: Option<RcStr>,
}

impl AssetIdent {
    pub fn add_modifier(&mut self, modifier: RcStr) {
        debug_assert!(!modifier.is_empty(), "modifiers cannot be empty.");
        self.modifiers.push(modifier);
    }

    pub async fn add_asset(&mut self, key: RcStr, asset: &AssetIdent) -> Result<()> {
        let mut assets = take(&mut self.assets).into_owned();
        if !assets.is_empty() {
            assets.push_str(", ");
        }
        write!(
            assets,
            " {key} => {asset}",
            asset = asset.value_to_string().await?
        )
        .expect("failed to write to assets");

        self.assets = RcStr::from(assets);
        Ok(())
    }
    pub async fn add_assets(&mut self, items: Vec<(RcStr, Vc<AssetIdent>)>) -> Result<()> {
        debug_assert!(!items.is_empty(), "assets cannot be empty.");
        let mut assets = take(&mut self.assets).into_owned();
        for (key, asset) in items {
            if !assets.is_empty() {
                assets.push_str(", ");
            }
            write!(
                assets,
                " {key} => {asset}",
                asset = asset.await?.value_to_string().await?
            )
            .expect("failed to write to assets");
        }
        self.assets = RcStr::from(assets);
        Ok(())
    }

    pub async fn rename_as_ref(&mut self, pattern: &str) -> Result<()> {
        let root = self.path.root().await?;
        self.path = root.join(&pattern.replace('*', &self.path.path))?;
        Ok(())
    }
    /// Creates an [AssetIdent] from a [FileSystemPath]
    pub fn from_path(path: FileSystemPath) -> Self {
        AssetIdent {
            path,
            query: RcStr::default(),
            fragment: RcStr::default(),
            assets: RcStr::default(),
            modifiers: Vec::new(),
            parts: Vec::new(),
            layer: None,
            content_type: None,
        }
    }

    pub async fn path(self: Vc<Self>) -> Result<FileSystemPath> {
        Ok(self.await?.path.clone())
    }
}

impl AssetIdent {
    /// Computes a unique output asset name for the given asset identifier.
    /// TODO(alexkirsz) This is `turbopack-browser` specific, as
    /// `turbopack-nodejs` would use a content hash instead. But for now
    /// both are using the same name generation logic.
    pub async fn output_name(
        &self,
        context_path: FileSystemPath,
        prefix: Option<RcStr>,
        expected_extension: RcStr,
    ) -> Result<String> {
        debug_assert!(
            expected_extension.starts_with("."),
            "the extension should include the leading '.', got '{expected_extension}'"
        );
        // TODO(PACK-2140): restrict character set to A–Za–z0–9-_.~'()
        // to be compatible with all operating systems + URLs.

        // For clippy -- This explicit deref is necessary
        let path = &self.path;
        let mut name = if let Some(inner) = context_path.get_path_to(path) {
            clean_separators(inner)
        } else {
            clean_separators(&self.path.value_to_string().await?)
        };
        let removed_extension = name.ends_with(&*expected_extension);
        if removed_extension {
            name.truncate(name.len() - expected_extension.len());
        }
        // This step ensures that leading dots are not preserved in file names. This is
        // important as some file servers do not serve files with leading dots (e.g.
        // Next.js).
        let mut name = clean_additional_extensions(&name);
        if let Some(prefix) = prefix {
            name = format!("{prefix}-{name}");
        }

        let default_modifier = match expected_extension.as_str() {
            ".js" => Some("ecmascript"),
            ".css" => Some("css"),
            _ => None,
        };

        let mut hasher = Xxh3Hash64Hasher::new();
        let mut has_hash = false;
        let AssetIdent {
            path: _,
            query,
            fragment,
            assets,
            modifiers,
            parts,
            layer,
            content_type,
        } = self;
        if !query.is_empty() {
            0_u8.deterministic_hash(&mut hasher);
            query.deterministic_hash(&mut hasher);
            has_hash = true;
        }
        if !fragment.is_empty() {
            1_u8.deterministic_hash(&mut hasher);
            fragment.deterministic_hash(&mut hasher);
            has_hash = true;
        }
        if !self.assets.is_empty() {
            2_u8.deterministic_hash(&mut hasher);
            assets.deterministic_hash(&mut hasher);
            has_hash = true;
        }
        for modifier in modifiers.iter() {
            if let Some(default_modifier) = default_modifier
                && *modifier == default_modifier
            {
                continue;
            }
            3_u8.deterministic_hash(&mut hasher);
            modifier.deterministic_hash(&mut hasher);
            has_hash = true;
        }
        for part in parts.iter() {
            4_u8.deterministic_hash(&mut hasher);
            match part {
                ModulePart::Evaluation => {
                    1_u8.deterministic_hash(&mut hasher);
                }
                ModulePart::Export(export) => {
                    2_u8.deterministic_hash(&mut hasher);
                    export.deterministic_hash(&mut hasher);
                }
                ModulePart::RenamedExport {
                    original_export,
                    export,
                } => {
                    3_u8.deterministic_hash(&mut hasher);
                    original_export.deterministic_hash(&mut hasher);
                    export.deterministic_hash(&mut hasher);
                }
                ModulePart::RenamedNamespace { export } => {
                    4_u8.deterministic_hash(&mut hasher);
                    export.deterministic_hash(&mut hasher);
                }
                ModulePart::Internal(id) => {
                    5_u8.deterministic_hash(&mut hasher);
                    id.deterministic_hash(&mut hasher);
                }
                ModulePart::Locals => {
                    6_u8.deterministic_hash(&mut hasher);
                }
                ModulePart::Exports => {
                    7_u8.deterministic_hash(&mut hasher);
                }
                ModulePart::Facade => {
                    8_u8.deterministic_hash(&mut hasher);
                }
            }

            has_hash = true;
        }
        if let Some(layer) = layer {
            5_u8.deterministic_hash(&mut hasher);
            layer.deterministic_hash(&mut hasher);
            has_hash = true;
        }
        if let Some(content_type) = content_type {
            6_u8.deterministic_hash(&mut hasher);
            content_type.deterministic_hash(&mut hasher);
            has_hash = true;
        }

        if has_hash {
            let hash = encode_hex(hasher.finish());
            let truncated_hash = &hash[..8];
            write!(name, "_{truncated_hash}")?;
        }

        // Location in "path" where hashed and named parts are split.
        // Everything before i is hashed and after i named.
        let mut i = 0;
        static NODE_MODULES: &str = "_node_modules_";
        if let Some(j) = name.rfind(NODE_MODULES) {
            i = j + NODE_MODULES.len();
        }
        const MAX_FILENAME: usize = 80;
        if name.len() - i > MAX_FILENAME {
            i = name.len() - MAX_FILENAME;
            if let Some(j) = name[i..].find('_')
                && j < 20
            {
                i += j + 1;
            }
        }
        if i > 0 {
            let hash = encode_hex(hash_xxh3_hash64(&name.as_bytes()[..i]));
            let truncated_hash = &hash[..5];
            name = format!("{}_{}", truncated_hash, &name[i..]);
        }
        // We need to make sure that `.json` and `.json.js` doesn't end up with the same
        // name. So when we add an extra extension when want to mark that with a "._"
        // suffix.
        if !removed_extension {
            name += "._";
        }
        name += &expected_extension;
        Ok(name)
    }

    /// Mimics `ValueToString::to_string`.
    pub fn value_to_string(&self) -> Vc<RcStr> {
        value_to_string(self.clone())
    }
}

#[turbo_tasks::function]

async fn value_to_string(ident: AssetIdent) -> Result<Vc<RcStr>> {
    let mut s = ident.path.value_to_string().owned().await?.into_owned();

    // The query string is either empty or non-empty starting with `?` so we can just concat
    s.push_str(&ident.query);
    // ditto for fragment
    s.push_str(&ident.fragment);

    if !ident.assets.is_empty() {
        s.push_str(" {");
        s.push_str(&ident.assets);

        s.push_str(" }");
    }

    if let Some(layer) = &ident.layer {
        write!(s, " [{}]", layer.name)?;
    }

    if !ident.modifiers.is_empty() {
        s.push_str(" (");

        for (i, modifier) in ident.modifiers.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }

            s.push_str(modifier);
        }

        s.push(')');
    }

    if let Some(content_type) = &ident.content_type {
        write!(s, " <{content_type}>")?;
    }

    if !ident.parts.is_empty() {
        for part in ident.parts.iter() {
            if !matches!(part, ModulePart::Facade) {
                // facade is not included in ident as switching between facade and non-facade
                // shouldn't change the ident
                write!(s, " <{part}>")?;
            }
        }
    }

    Ok(Vc::cell(s.into()))
}

fn clean_separators(s: &str) -> String {
    static SEPARATOR_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"[/#?]").unwrap());
    SEPARATOR_REGEX.replace_all(s, "_").to_string()
}

fn clean_additional_extensions(s: &str) -> String {
    s.replace('.', "_")
}
