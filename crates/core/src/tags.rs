//! Tag generation: lindera (ipadic) morphological analysis -> noun
//! extraction -> Zenn tag matching (best-effort). Falls back to raw noun
//! frequency when the Zenn API is unavailable.

use std::collections::HashMap;
use std::io::Read;

use lindera::dictionary::DictionaryKind;
use lindera::tokenizer::TokenizerBuilder;

const MAX_TAGS: usize = 10;
const MAX_WORD_CHARS: usize = 20;
const ZENN_TAGS_URL: &str = "https://zenn.dev/api/tags";

/// A noun token with its reading for `token_analysis`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NounToken {
    pub token: String,
    pub base_form: String,
    pub reading: String,
}

fn build_tokenizer() -> Result<lindera::tokenizer::Tokenizer, String> {
    let mut builder = TokenizerBuilder::new().map_err(|e| e.to_string())?;
    builder.set_segmenter_dictionary_kind(&DictionaryKind::IPADIC);
    builder.build().map_err(|e| e.to_string())
}

/// Tokenize one page and return noun tokens (POS starts with 名詞).
pub fn analyze_page(text: &str) -> Result<Vec<NounToken>, String> {
    let tokenizer = build_tokenizer()?;
    let tokens = tokenizer.tokenize(text).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for mut token in tokens {
        let surface = token.text.to_string();
        let details: Vec<&str> = token.details();
        if details.first().is_none_or(|pos| !pos.starts_with("名詞")) {
            continue;
        }
        let base = details.get(6).copied().unwrap_or("");
        let base_form = if base.is_empty() || base == "*" {
            surface.clone()
        } else {
            base.to_string()
        };
        let reading = details
            .get(7)
            .copied()
            .filter(|r| *r != "*")
            .map(str::to_string)
            .unwrap_or_default();
        out.push(NounToken {
            token: surface,
            base_form,
            reading,
        });
    }
    Ok(out)
}

/// Noun frequencies across pages, descending, excluding words in
/// `excluded` (author/circle names) and words longer than 20 chars.
/// The key is the base form when available, otherwise the surface form.
pub fn extract_nouns(text: &str, excluded: &[&str]) -> Vec<(String, usize)> {
    let Ok(tokens) = analyze_page(text) else {
        return Vec::new();
    };
    let mut frequencies: HashMap<String, usize> = HashMap::new();
    for token in tokens {
        let word = if token.base_form.is_empty() {
            token.token
        } else {
            token.base_form
        };
        if word.chars().count() > MAX_WORD_CHARS {
            continue;
        }
        if excluded.iter().any(|e| *e == word) {
            continue;
        }
        *frequencies.entry(word).or_insert(0) += 1;
    }
    let mut ranked: Vec<(String, usize)> = frequencies.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked
}

/// Zenn API lookup failure — never fatal; tag generation falls back.
#[derive(Debug)]
pub struct ZennError;

/// Fetch the Zenn tag list (best-effort; `Err` when unavailable).
pub fn fetch_zenn_tags() -> Result<Vec<String>, ZennError> {
    // 起動時に 1 回だけ取得し、プロセス内でキャッシュする
    // （取り込みのたびにネットワーク待ちで止まらないようにする）
    static CACHE: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);
    if let Ok(Some(tags)) = CACHE.lock().map(|guard| guard.clone()) {
        return Ok(tags);
    }
    // タイムアウト付きで取得（ハングすると取り込みが止まるため）
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout_read(std::time::Duration::from_secs(10))
        .build();
    let response = agent
        .get(ZENN_TAGS_URL)
        .set("User-Agent", "thundoku-shelf/0.1")
        .call()
        .map_err(|_| ZennError)?;
    if response.status() != 200 {
        return Err(ZennError);
    }
    let mut body = String::new();
    response
        .into_reader()
        .read_to_string(&mut body)
        .map_err(|_| ZennError)?;
    let payload: serde_json::Value = serde_json::from_str(&body).map_err(|_| ZennError)?;

    let tags: Vec<String> = payload
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .get("name")
                        .or_else(|| entry.get("id"))
                        .and_then(serde_json::Value::as_str)
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();
    if tags.is_empty() {
        Err(ZennError)
    } else {
        if let Ok(mut guard) = CACHE.lock() {
            *guard = Some(tags.clone());
        }
        Ok(tags)
    }
}

/// Generate up to 10 tags from page texts. When Zenn tags are available,
/// only nouns matching the Zenn list become tags; otherwise the raw
/// frequency-ranked nouns are used.
pub fn generate_tags(texts: &[&str], excluded: &[&str], zenn_tags: &[String]) -> Vec<String> {
    let mut frequencies: HashMap<String, usize> = HashMap::new();
    for text in texts {
        for (word, count) in extract_nouns(text, excluded) {
            *frequencies.entry(word).or_insert(0) += count;
        }
    }
    let mut ranked: Vec<(String, usize)> = frequencies.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    if !zenn_tags.is_empty() {
        let zenn: std::collections::HashSet<&str> = zenn_tags.iter().map(String::as_str).collect();
        ranked.retain(|(word, _)| zenn.contains(word.as_str()));
    }
    ranked
        .into_iter()
        .take(MAX_TAGS)
        .map(|(word, _)| word)
        .collect()
}
