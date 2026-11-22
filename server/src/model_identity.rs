//! Model identity: how the gateway decides that two provider spellings name
//! the same model.
//!
//! Providers publish the same weights under different ids: SailResearch sends
//! `deepseek-ai/DeepSeek-V4.1-Flash-1017` while OpenRouter sends
//! `deepseek/deepseek-v4.1-flash-1017`. Exact, case-insensitive string equality
//! therefore cannot join them, and a bespoke mapping per model would rot.
//!
//! Two related rules live here, both derived from the spelling itself:
//!
//! * **Benchmark matching (D38)** — [`ModelIndex`] and [`normalize_model_key`]
//!   are deliberately fuzzy because a published score is model-level. They
//!   drop the path vendor, a leading hyphen vendor, tier markers (`-latest`,
//!   `-preview`, `-thinking`, …) and date/build stamps (`-1017`, `-2025-08-07`),
//!   so every dated snapshot of one name shares an identity.
//! * **Catalog identity** — [`catalog_identity`] is strict. It drops the path
//!   vendor and normalizes case/punctuation but keeps tier and date tokens,
//!   because `deepseek-v4-flash` and `deepseek-v4-flash-0731` are separate
//!   catalog entries with their own context limits and prices. It also
//!   requires a specific tail (a digit or at least two tokens), so a bare word
//!   never joins rows. There is no vendor or model name list in this rule.
//!   Discovery and the OpenRouter price sync use it to attach a new offering
//!   to an existing model when — and only when — the identity resolves to
//!   exactly one row.
//!
//! Neither rule rewrites a stored name: the first spelling that created the
//! model row stays. An ambiguous identity is never guessed; callers keep the
//! rows separate instead.

use std::collections::HashMap;

/// Vendor spellings that name the same organization, used **for matching
/// only** (`deepseek-ai` and `deepseek` publish the same weights under
/// different ids). Stored names are never rewritten.
const VENDOR_ALIASES: [(&str, &str); 20] = [
    ("deepseek-ai", "deepseek"),
    ("deepseek", "deepseek"),
    ("z-ai", "zai-org"),
    ("zai-org", "zai-org"),
    ("zai", "zai-org"),
    ("x-ai", "xai"),
    ("xai", "xai"),
    ("moonshotai", "moonshot"),
    ("moonshot-ai", "moonshot"),
    ("moonshot", "moonshot"),
    ("google-ai", "google"),
    ("google", "google"),
    ("meta-llama", "meta"),
    ("meta", "meta"),
    ("mistralai", "mistral"),
    ("mistral", "mistral"),
    ("openai", "openai"),
    ("anthropic", "anthropic"),
    ("alibaba", "qwen"),
    ("qwen", "qwen"),
];

/// Tails too generic to identify a model on their own: a benchmark row named
/// `flash` (or `pro`, or `auto`) must never attach to whichever model happens
/// to carry that word.
const GENERIC_TOKENS: [&str; 14] = [
    "flash", "pro", "auto", "mini", "lite", "turbo", "base", "chat", "instruct", "preview",
    "latest", "thinking", "small", "large",
];

/// Trailing tier markers stripped for the identity (fallback) key only.
const TIER_SUFFIXES: [&str; 9] = [
    "latest",
    "preview",
    "thinking",
    "nothinking",
    "xhigh",
    "high",
    "medium",
    "low",
    "exp",
];

/// Removes parenthetical tier groups, e.g. `Claude Sonnet 5.2 (xHigh)`.
fn strip_parentheticals(mut lowered: String) -> String {
    while let (Some(start), Some(end)) = (lowered.find('('), lowered.find(')')) {
        if end <= start {
            break;
        }
        lowered.replace_range(start..=end, " ");
    }
    lowered
}

/// Lower-cases are the caller's job; this collapses non-alphanumerics into
/// single dashes.
fn normalize_text(raw: &str) -> String {
    let mut key = String::with_capacity(raw.len());
    let mut last_dash = true;
    for character in raw.chars() {
        if character.is_ascii_alphanumeric() {
            key.push(character);
            last_dash = false;
        } else if !last_dash {
            key.push('-');
            last_dash = true;
        }
    }
    while key.ends_with('-') {
        key.pop();
    }
    key
}

/// Normalizes a model name or id for matching: lower case, parenthetical
/// tier groups removed, the part after the last `/` kept (vendor prefix
/// dropped), and every run of non-alphanumerics collapsed to `-`.
///
/// Variant suffixes (`:free`, `:batch`, `-preview`, `-latest`, dates, …) are
/// kept here so an exact offering id still identifies one offering;
/// [`identity_keys`] is what merges the same model across providers.
/// The result is empty when the input has no alphanumerics.
pub fn normalize_model_key(raw: &str) -> String {
    let lowered = strip_parentheticals(raw.trim().to_lowercase());
    let tail = lowered.rsplit('/').next().unwrap_or(&lowered);
    normalize_text(tail)
}

/// The normalized *full* id, vendor segment included
/// (`deepseek-ai/deepseek-v4-1-flash-1017` -> `deepseek-ai-deepseek-v4-1-flash-1017`).
/// Used only for an exact full-id match, which does not apply the generic-tail
/// guard.
fn normalize_full_key(raw: &str) -> String {
    normalize_text(&strip_parentheticals(raw.trim().to_lowercase()))
}

/// The canonical vendor spelling for matching, or the normalized input when
/// it is not a known alias.
pub fn canonical_vendor(vendor: &str) -> String {
    let normalized = normalize_text(&vendor.trim().to_lowercase());
    VENDOR_ALIASES
        .iter()
        .find(|(alias, _)| *alias == normalized)
        .map_or(normalized, |(_, canonical)| (*canonical).to_string())
}

/// Splits a leading hyphenated vendor token off a tail id, longest alias
/// first (`z-ai-glm-5` -> (`zai-org`, `glm-5`)). The returned vendor is
/// canonical; `None` when no known vendor prefixes the key.
fn split_leading_vendor(key: &str) -> Option<(String, String)> {
    let mut best: Option<(String, String)> = None;
    for (alias, _canonical) in VENDOR_ALIASES {
        let Some(rest) = key
            .strip_prefix(alias)
            .and_then(|rest| rest.strip_prefix('-'))
        else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let shorter = best
            .as_ref()
            .is_none_or(|(_, current)| rest.len() < current.len());
        if shorter {
            best = Some((canonical_vendor(alias), rest.to_string()));
        }
    }
    best
}

/// `:free` / `:batch` (normalized to `-free` / `-batch`) mark a distinct
/// offering, never a tier of the same one.
fn split_offering(key: &str) -> (&str, Option<&'static str>) {
    for tag in ["free", "batch"] {
        if let Some(base) = key.strip_suffix(&format!("-{tag}"))
            && !base.is_empty()
        {
            return (base, Some(tag));
        }
    }
    (key, None)
}

/// True when a trailing token is a date or build stamp: four to eight digits
/// (`-1017`, `-0731`, `-20250807`). One-to-three digit version parts
/// (`-4`, `-5`, `-70b`) are part of the model name and stay.
fn is_date_or_build(token: &str) -> bool {
    (4..=8).contains(&token.len()) && token.bytes().all(|byte| byte.is_ascii_digit())
}

/// Strips trailing tier markers and date/build stamps repeatedly, so
/// `deepseek-v4-flash-20250807-thinking` and `deepseek-v4-flash` share an
/// identity.
fn strip_variant_suffixes(mut key: String) -> String {
    loop {
        let before = key.len();
        if let Some((head, tail)) = key.rsplit_once('-')
            && TIER_SUFFIXES.contains(&tail)
        {
            key.truncate(head.len());
        }
        if let Some((head, tail)) = key.rsplit_once('-')
            && is_date_or_build(tail)
        {
            key.truncate(head.len());
        }
        // A dashed date: `-2025-08-07`.
        let parts: Vec<&str> = key.rsplitn(4, '-').collect();
        if parts.len() == 4
            && parts[0].len() == 2
            && parts[1].len() == 2
            && parts[2].len() == 4
            && parts[2].starts_with('2')
            && parts[..3]
                .iter()
                .all(|part| part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            key.truncate(key.len() - 11);
        }
        if key.len() == before {
            break;
        }
    }
    key
}

/// True when a stripped key still names something: it contains a digit, or a
/// token that is not on the generic list.
fn is_distinctive(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    if key.bytes().any(|byte| byte.is_ascii_digit()) {
        return true;
    }
    !key.split('-').all(|token| GENERIC_TOKENS.contains(&token))
}
/// True when a normalized key names a model specifically enough to join
/// catalog rows without a word list: it carries a digit (`gpt-4o`,
/// `glm-5-3`) or at least two tokens (`claude-opus`). A bare word
/// (`flash`, `auto`, `chat`) is never an identity; two providers using the
/// same bare word may mean different models.
fn is_specific(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    if key.bytes().any(|byte| byte.is_ascii_digit()) {
        return true;
    }
    key.split('-').filter(|token| !token.is_empty()).count() >= 2
}

/// The identities of a normalized key, most specific first: offering tag
/// kept, tiers/dates/build stamps removed, and (for the hyphen vendor shape)
/// a leading vendor token removed too. Two catalog entries sharing any one of
/// these identities are the same model under different provider spellings.
/// Empty when every candidate is generic.
fn identity_keys(normalized_key: &str) -> Vec<String> {
    let (base, offering) = split_offering(normalized_key);
    let mut candidates = vec![base.to_string()];
    if let Some((_vendor, remainder)) = split_leading_vendor(base) {
        candidates.push(remainder);
    }
    let mut identities = Vec::new();
    for candidate in candidates {
        let base = strip_variant_suffixes(candidate);
        if !is_distinctive(&base) {
            continue;
        }
        let identity = match offering {
            Some(tag) => format!("{base}|{tag}"),
            None => base,
        };
        if !identities.contains(&identity) {
            identities.push(identity);
        }
    }
    identities
}

/// Splits an identity key into its base and offering tag.
fn split_identity(identity: &str) -> (&str, Option<&str>) {
    match identity.rsplit_once('|') {
        Some((base, tag)) => (base, Some(tag)),
        None => (identity, None),
    }
}

/// The strict catalog identity of an upstream model id, or `None` when the id
/// is too generic to join rows safely.
///
/// Unlike the benchmark identity, this keeps tier and date/build tokens: a
/// provider that publishes `deepseek-v4-flash` and one that publishes
/// `deepseek-v4-flash-0731` are separate catalog entries with their own
/// limits and prices. `:free` and `:batch` stay distinct for the same reason.
///
/// The join only applies to a specific tail (see [`is_specific`]): a digit or
/// at least two tokens. There is no vendor or model allowlist here, so a new
/// provider spelling needs no code change.
pub fn catalog_identity(raw: &str) -> Option<String> {
    let key = normalize_model_key(raw);
    let (base, offering) = split_offering(&key);
    if !is_specific(base) {
        return None;
    }
    Some(match offering {
        Some(tag) => format!("{base}|{tag}"),
        None => base.to_string(),
    })
}

/// How one hint resolved against the catalog. `Matched` lists **every** catalog
/// model that shares the hint's identity (one benchmark row per model, because
/// the published score is model-level); `Ambiguous` means several *distinct*
/// identities could be meant and nothing is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matching {
    Matched(Vec<String>),
    Unknown,
    Ambiguous,
}

/// A normalized index over `models.canonical_name` and
/// `offerings.upstream_model_id`.
///
/// `full` holds the normalized full id for an exact (precise) match. `identities`
/// holds the merge key: suffix-stripped tail, offering tag kept, vendor prefix
/// stripped (path or hyphen shape) and aliased. One identity maps to every
/// catalog model that uses it, which is exactly the multi-model attachment
/// rule.
#[derive(Debug, Default)]
pub struct ModelIndex {
    full: HashMap<String, Vec<String>>,
    identities: HashMap<String, Vec<String>>,
}

fn push_unique(ids: &mut Vec<String>, model_id: &str) {
    if !ids.iter().any(|id| id == model_id) {
        ids.push(model_id.to_string());
    }
}

impl ModelIndex {
    /// Builds an index from `(hint, model_id)` pairs. Duplicate ids for one
    /// key collapse; distinct ids under one identity are returned together.
    pub fn new(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut index = Self::default();
        for (hint, model_id) in pairs {
            let key = normalize_model_key(&hint);
            if key.is_empty() {
                continue;
            }
            let full = normalize_full_key(&hint);
            if full != key {
                push_unique(index.full.entry(full).or_default(), &model_id);
            }
            for identity in identity_keys(&key) {
                push_unique(index.identities.entry(identity).or_default(), &model_id);
            }
        }
        index
    }

    /// Every model id under any of the given identities, sorted. `None` when
    /// none of them is indexed.
    fn identity_match(&self, identities: &[String]) -> Option<Matching> {
        let mut ids: Vec<String> = Vec::new();
        for identity in identities {
            if let Some(found) = self.identities.get(identity) {
                for id in found {
                    push_unique(&mut ids, id);
                }
            }
        }
        if ids.is_empty() {
            return None;
        }
        ids.sort();
        Some(Matching::Matched(ids))
    }

    /// Resolves one hint. Order:
    ///
    /// 1. The exact full id (vendor included) united with the hint's
    ///    identities. The exact id is what makes a generic tail matchable;
    ///    the identity group is what attaches the row to every provider of the
    ///    same model.
    /// 2. A unique hyphen-boundary suffix match over stripped identities;
    ///    several distinct candidates stay [`Matching::Ambiguous`].
    pub fn lookup(&self, hint: &str) -> Matching {
        let key = normalize_model_key(hint);
        if key.is_empty() {
            return Matching::Unknown;
        }
        let full = normalize_full_key(hint);
        let identities = identity_keys(&key);
        let mut ids = self.full.get(&full).cloned().unwrap_or_default();
        if let Some(Matching::Matched(found)) = self.identity_match(&identities) {
            for id in found {
                push_unique(&mut ids, &id);
            }
        }
        if !ids.is_empty() {
            ids.sort();
            ids.dedup();
            return Matching::Matched(ids);
        }
        // Fuzzy: the hint's most-stripped identity ends with a catalog
        // identity. Only one distinct identity may answer; several means the
        // hint does not say which model it means.
        let Some(hint_identity) = identities.last() else {
            return Matching::Unknown;
        };
        let (hint_base, hint_offering) = split_identity(hint_identity);
        let mut candidates: Vec<&String> = Vec::new();
        for candidate in self.identities.keys() {
            let (base, offering) = split_identity(candidate);
            if offering != hint_offering || base == hint_base {
                continue;
            }
            if hint_base.ends_with(&format!("-{base}")) {
                candidates.push(candidate);
            }
        }
        match candidates.as_slice() {
            [identity] => {
                let identity = (*identity).clone();
                self.identity_match(std::slice::from_ref(&identity))
                    .unwrap_or(Matching::Unknown)
            }
            [] => Matching::Unknown,
            _ => Matching::Ambiguous,
        }
    }
}

/// How one upstream id resolved against the catalog for a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityMatch {
    /// Same canonical name, ignoring ASCII case.
    Exact(String),
    /// One catalog model carries the same strict identity.
    Unique(String),
    /// Several catalog models carry the identity; the operator keeps them
    /// separate on purpose, so a write must not guess.
    Ambiguous,
    /// No catalog model matches.
    Unknown,
}
/// One external identity source: lower-cased published id -> canonical keys.
///
/// The sources are consulted in a fixed order (`models_dev`, then
/// `openrouter`) and their answers are united. A source that answers with
/// several canonical keys for one id is ambiguous and is skipped; if no source
/// answers, the caller falls back to [`catalog_identity`]. Canonical keys are
/// stored and compared lower-cased, so case never splits a group.
#[derive(Debug, Default, Clone)]
pub struct Registry {
    models_dev: HashMap<String, Vec<String>>,
    openrouter: HashMap<String, Vec<String>>,
}

impl Registry {
    /// Builds the registry from `(alias, canonical)` pairs per source.
    pub fn new(
        models_dev: impl IntoIterator<Item = (String, String)>,
        openrouter: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        let mut registry = Self::default();
        for (alias, canonical) in models_dev {
            push_registry(&mut registry.models_dev, &alias, &canonical);
        }
        for (alias, canonical) in openrouter {
            push_registry(&mut registry.openrouter, &alias, &canonical);
        }
        registry
    }

    /// Every unique external canonical key for `id`, models.dev first and then
    /// OpenRouter.
    ///
    /// A source with several keys for the alias cannot decide, so that source
    /// is skipped; the other source may still answer. An empty result means no
    /// source knows the id. Keys are lower-cased on read as well as on write.
    pub fn keys_for(&self, id: &str) -> Vec<String> {
        let alias = id.trim().to_ascii_lowercase();
        let mut keys = Vec::new();
        for source in [&self.models_dev, &self.openrouter] {
            let Some(canonicals) = source.get(&alias) else {
                continue;
            };
            let mut unique: Vec<String> = Vec::new();
            for canonical in canonicals {
                let canonical = canonical.trim().to_ascii_lowercase();
                if !canonical.is_empty() && !unique.contains(&canonical) {
                    unique.push(canonical);
                }
            }
            match unique.as_slice() {
                [canonical] => {
                    if !keys.contains(canonical) {
                        keys.push(canonical.clone());
                    }
                }
                // Several keys: this source is ambiguous for the alias.
                _ => continue,
            }
        }
        keys
    }
}

fn push_registry(map: &mut HashMap<String, Vec<String>>, alias: &str, canonical: &str) {
    let alias = alias.trim().to_ascii_lowercase();
    let canonical = canonical.trim().to_ascii_lowercase();
    if alias.is_empty() || canonical.is_empty() {
        return;
    }
    push_unique(map.entry(alias).or_default(), &canonical);
}

/// A write-time index over `models.canonical_name`, keyed by the lower-cased
/// name and by [`catalog_identity`]. One index is built per discovery or sync
/// transaction; new models are inserted into it as they are created.
#[derive(Debug, Default)]
pub struct CatalogIndex {
    by_name: HashMap<String, String>,
    by_identity: HashMap<String, Vec<String>>,
    by_registry: HashMap<String, Vec<String>>,
    registry: Registry,
}

impl CatalogIndex {
    /// Builds the index with an external registry, which is consulted
    /// before the spelling identity.
    pub fn with_registry(
        models: impl IntoIterator<Item = (String, String)>,
        registry: Registry,
    ) -> Self {
        let mut index = Self {
            registry,
            ..Self::default()
        };
        for (model_id, canonical_name) in models {
            index.insert(&model_id, &canonical_name);
        }
        index
    }

    /// Adds a model row to the index.
    pub fn insert(&mut self, model_id: &str, canonical_name: &str) {
        let name = canonical_name.trim().to_ascii_lowercase();
        self.by_name
            .entry(name)
            .or_insert_with(|| model_id.to_string());
        if let Some(identity) = catalog_identity(canonical_name) {
            push_unique(self.by_identity.entry(identity).or_default(), model_id);
        }
        for key in self.registry.keys_for(canonical_name) {
            push_unique(self.by_registry.entry(key).or_default(), model_id);
        }
    }

    /// Resolves an upstream id: the exact name wins (matching SQLite's
    /// ASCII `NOCASE`), then a unique model under any external registry key,
    /// then a unique strict identity.
    ///
    /// When the registry keys cover several catalog rows (an external source
    /// is coarser than the catalog, e.g. it groups a serving variant with the
    /// base model), the spelling identity decides between them — but only
    /// among those registry candidates. If it cannot, the result is
    /// [`IdentityMatch::Ambiguous`] — the registry knew the family but not the
    /// row, so a write must not guess. With no registry answer the spelling
    /// identity behaves exactly as before.
    pub fn resolve(&self, upstream_id: &str) -> IdentityMatch {
        let name = upstream_id.trim().to_ascii_lowercase();
        if let Some(model_id) = self.by_name.get(&name) {
            return IdentityMatch::Exact(model_id.clone());
        }
        let mut registry_ids: Vec<String> = Vec::new();
        for key in self.registry.keys_for(upstream_id) {
            if let Some(found) = self.by_registry.get(&key) {
                for id in found {
                    push_unique(&mut registry_ids, id);
                }
            }
        }
        if registry_ids.len() == 1 {
            return IdentityMatch::Unique(registry_ids[0].clone());
        }
        let identity_match = catalog_identity(upstream_id)
            .and_then(|identity| self.by_identity.get(&identity))
            .map(Vec::as_slice);
        if registry_ids.is_empty() {
            return match identity_match {
                Some([model_id]) => IdentityMatch::Unique(model_id.clone()),
                Some(_) => IdentityMatch::Ambiguous,
                None => IdentityMatch::Unknown,
            };
        }
        // The registry named several candidates; the spelling identity may
        // only choose among them.
        match identity_match {
            Some(ids) => {
                let common: Vec<&String> = ids
                    .iter()
                    .filter(|model_id| registry_ids.contains(model_id))
                    .collect();
                match common.as_slice() {
                    [model_id] => IdentityMatch::Unique((*model_id).clone()),
                    _ => IdentityMatch::Ambiguous,
                }
            }
            None => IdentityMatch::Ambiguous,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_identity_joins_provider_spellings_and_keeps_snapshots_apart() {
        // The live pair that motivated the rule: same weights, different path
        // vendor and case.
        assert_eq!(
            catalog_identity("deepseek-ai/DeepSeek-V4.1-Flash-1017"),
            catalog_identity("deepseek/deepseek-v4.1-flash-1017")
        );
        assert_eq!(
            catalog_identity("zai-org/GLM-5.3"),
            catalog_identity("z-ai/glm-5.3")
        );
        assert_eq!(
            catalog_identity("Qwen/Qwen3.6-35B-A3B"),
            catalog_identity("qwen/qwen3.6-35b-a3b")
        );
        assert_eq!(
            catalog_identity("google/gemma-4-31B-it"),
            catalog_identity("google/gemma-4-31b-it")
        );

        // A build stamp distinguishes snapshots, unlike the benchmark identity.
        assert_ne!(
            catalog_identity("deepseek-ai/DeepSeek-V4-Flash-0731"),
            catalog_identity("deepseek/deepseek-v4-flash")
        );
        assert_eq!(
            catalog_identity("deepseek-ai/DeepSeek-V4-Flash-0731"),
            catalog_identity("deepseek/deepseek-v4-flash-0731")
        );

        // Offering tags stay distinct.
        assert_ne!(
            catalog_identity("openai/gpt-4o:free"),
            catalog_identity("openai/gpt-4o")
        );

        // Specificity is structural: a digit or two tokens. A bare word never
        // joins, without any word list; a multi-token name does.
        assert_eq!(catalog_identity("flash"), None);
        assert_eq!(catalog_identity("auto"), None);
        assert_eq!(catalog_identity("chat"), None);
        assert_eq!(catalog_identity("mixtral"), None, "one bare token");
        assert!(catalog_identity("claude-opus").is_some());
        assert_eq!(
            catalog_identity("vendor/Claude-Opus"),
            catalog_identity("other/claude-opus")
        );
        assert_eq!(catalog_identity("   "), None);
        assert_eq!(catalog_identity("!!!"), None);
    }

    #[test]
    fn catalog_index_prefers_the_exact_name_then_a_unique_identity() {
        let mut index = CatalogIndex::with_registry(
            vec![
                (
                    "model-sail".to_string(),
                    "deepseek-ai/DeepSeek-V4.1-Flash-1017".to_string(),
                ),
                (
                    "model-openrouter".to_string(),
                    "deepseek/deepseek-v4.1-flash".to_string(),
                ),
            ],
            Registry::default(),
        );
        assert_eq!(
            index.resolve("deepseek-ai/deepseek-v4.1-flash-1017"),
            IdentityMatch::Exact("model-sail".to_string()),
            "case-only difference still reuses the row"
        );
        assert_eq!(
            index.resolve("openrouter/deepseek-v4.1-flash"),
            IdentityMatch::Unique("model-openrouter".to_string())
        );
        assert_eq!(index.resolve("unknown/model"), IdentityMatch::Unknown);

        // Two rows under one identity: a write must not pick one.
        index.insert("model-duplicate", "vendor/deepseek-v4.1-flash");
        assert_eq!(
            index.resolve("deepseek-ai/DeepSeek-V4.1-Flash-1017"),
            IdentityMatch::Exact("model-sail".to_string()),
            "the exact name still wins over an ambiguous identity"
        );
        assert_eq!(
            index.resolve("deepseek/deepseek-v4.1-flash"),
            IdentityMatch::Exact("model-openrouter".to_string()),
            "its own exact name resolves it"
        );
        assert_eq!(
            index.resolve("another/deepseek-v4.1-flash"),
            IdentityMatch::Ambiguous
        );
    }

    #[test]
    fn the_external_registry_wins_over_spelling_and_never_guesses() {
        // The spelling identity would pick `spell-id`, but the registry says
        // the id belongs with `registry-id`. The registry wins.
        let registry = Registry::new(
            vec![
                ("incoming/foo-7b".to_string(), "bar-key".to_string()),
                ("other/bar-9x".to_string(), "bar-key".to_string()),
            ],
            Vec::new(),
        );
        let index = CatalogIndex::with_registry(
            vec![
                ("spell-id".to_string(), "vendor/foo-7b".to_string()),
                ("registry-id".to_string(), "other/bar-9x".to_string()),
            ],
            registry,
        );
        assert_eq!(
            index.resolve("incoming/foo-7b"),
            IdentityMatch::Unique("registry-id".to_string())
        );

        // models.dev reports two canonicals for this alias; that source is
        // skipped and OpenRouter answers instead.
        let registry = Registry::new(
            vec![
                ("conflict/model".to_string(), "one".to_string()),
                ("conflict/model".to_string(), "two".to_string()),
            ],
            vec![
                ("conflict/model".to_string(), "or-key".to_string()),
                ("some/path-or-key".to_string(), "or-key".to_string()),
            ],
        );
        let index = CatalogIndex::with_registry(
            vec![("or-id".to_string(), "some/path-or-key".to_string())],
            registry,
        );
        assert_eq!(
            index.resolve("conflict/model"),
            IdentityMatch::Unique("or-id".to_string()),
            "the unambiguous source answers"
        );

        // One registry key pointing at two catalog rows is ambiguous, and the
        // spelling fallback must not override that.
        let registry = Registry::new(
            Vec::new(),
            vec![
                ("amb/model".to_string(), "key".to_string()),
                ("a/model".to_string(), "key".to_string()),
                ("b/model".to_string(), "key".to_string()),
            ],
        );
        let index = CatalogIndex::with_registry(
            vec![
                ("id-a".to_string(), "a/model".to_string()),
                ("id-b".to_string(), "b/model".to_string()),
            ],
            registry,
        );
        assert_eq!(index.resolve("amb/model"), IdentityMatch::Ambiguous);

        // The registry groups two catalog rows (it is coarser than the
        // catalog), but the spelling identity can still choose one of them.
        let registry = Registry::new(
            Vec::new(),
            vec![
                ("vendor/model-7b".to_string(), "key".to_string()),
                ("alpha/model-7b".to_string(), "key".to_string()),
                ("beta/model-9x".to_string(), "key".to_string()),
            ],
        );
        let index = CatalogIndex::with_registry(
            vec![
                ("id-a".to_string(), "alpha/model-7b".to_string()),
                ("id-b".to_string(), "beta/model-9x".to_string()),
            ],
            registry,
        );
        assert_eq!(
            index.resolve("vendor/model-7b"),
            IdentityMatch::Unique("id-a".to_string()),
            "the finer spelling identity breaks the registry tie"
        );
    }

    #[test]
    fn registry_keys_from_every_source_can_join_a_pair() {
        // models.dev knows only the incoming id, OpenRouter knows only the
        // existing model's canonical name; both publish the same key.
        let registry = Registry::new(
            vec![("incoming/new-id".to_string(), "shared-key".to_string())],
            vec![("sail/old-spelling".to_string(), "shared-key".to_string())],
        );
        let index = CatalogIndex::with_registry(
            vec![("model-a".to_string(), "sail/old-spelling".to_string())],
            registry,
        );
        assert_eq!(
            index.resolve("incoming/new-id"),
            IdentityMatch::Unique("model-a".to_string())
        );

        // The regression the multi-key lookup fixes: both sources know the
        // incoming id, but only OpenRouter's key covers the existing row. The
        // union of keys finds it, while the old first-source lookup missed.
        let registry = Registry::new(
            vec![("incoming/new-id".to_string(), "k-dev".to_string())],
            vec![
                ("incoming/new-id".to_string(), "k-or".to_string()),
                ("sail/old-spelling".to_string(), "k-or".to_string()),
            ],
        );
        let index = CatalogIndex::with_registry(
            vec![("model-a".to_string(), "sail/old-spelling".to_string())],
            registry,
        );
        assert_eq!(
            index.resolve("incoming/new-id"),
            IdentityMatch::Unique("model-a".to_string()),
            "a key from the second source still reaches the row"
        );
    }

    #[test]
    fn several_registry_candidates_stay_ambiguous() {
        let registry = Registry::new(
            vec![
                ("incoming/a-7b".to_string(), "dev-key".to_string()),
                ("incoming/a-7b".to_string(), "dev-key-2".to_string()),
            ],
            vec![
                ("incoming/a-7b".to_string(), "or-key".to_string()),
                ("alpha/a-7b".to_string(), "or-key".to_string()),
                ("beta/a-7b".to_string(), "or-key".to_string()),
            ],
        );
        // models.dev is ambiguous for the alias, so only OpenRouter answers;
        // its key covers two catalog rows and the spelling cannot choose.
        let index = CatalogIndex::with_registry(
            vec![
                ("id-a".to_string(), "alpha/a-7b".to_string()),
                ("id-b".to_string(), "beta/a-7b".to_string()),
            ],
            registry,
        );
        assert_eq!(index.resolve("incoming/a-7b"), IdentityMatch::Ambiguous);
    }
}
