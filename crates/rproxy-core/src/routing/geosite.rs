use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
};

use prost::{Message, Oneof};
use regex::Regex;
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeositeMatcher {
    Domain {
        value: String,
        attrs: Vec<Attribute>,
    },
    Full {
        value: String,
        attrs: Vec<Attribute>,
    },
    Keyword {
        value: String,
        attrs: Vec<Attribute>,
    },
    Regex {
        value: String,
        attrs: Vec<Attribute>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    key: String,
    value: AttributeValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum AttributeValue {
    Bool(bool),
    Int(i64),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GeositeSelector {
    category: String,
    attr_filters: Vec<AttrFilter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum AttrFilter {
    Has(String),
    Not(String),
    Equals(String, AttributeValue),
}

#[derive(Clone, PartialEq, Message)]
struct GeoSiteList {
    #[prost(message, repeated, tag = "1")]
    entry: Vec<GeoSite>,
}

#[derive(Clone, PartialEq, Message)]
struct GeoSite {
    #[prost(string, tag = "1")]
    country_code: String,
    #[prost(message, repeated, tag = "2")]
    domain: Vec<Domain>,
}

#[derive(Clone, PartialEq, Message)]
struct Domain {
    #[prost(enumeration = "DomainType", tag = "1")]
    r#type: i32,
    #[prost(string, tag = "2")]
    value: String,
    #[prost(message, repeated, tag = "3")]
    attribute: Vec<DomainAttribute>,
}

#[derive(Clone, PartialEq, Message)]
struct DomainAttribute {
    #[prost(string, tag = "1")]
    key: String,
    #[prost(oneof = "domain_attribute::TypedValue", tags = "2, 3")]
    typed_value: Option<domain_attribute::TypedValue>,
}

mod domain_attribute {
    use super::*;

    #[derive(Clone, PartialEq, Oneof)]
    pub enum TypedValue {
        #[prost(bool, tag = "2")]
        BoolValue(bool),
        #[prost(int64, tag = "3")]
        IntValue(i64),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
enum DomainType {
    Plain = 0,
    Regex = 1,
    Domain = 2,
    Full = 3,
}

pub fn load_category(
    path: &Path,
    selector: &GeositeSelector,
) -> Result<Vec<GeositeMatcher>, GeositeError> {
    let bytes = fs::read(path)?;
    let list = GeoSiteList::decode(bytes.as_slice())?;
    let Some(site) = list
        .entry
        .into_iter()
        .find(|site| site.country_code.eq_ignore_ascii_case(&selector.category))
    else {
        return Ok(Vec::new());
    };

    Ok(site
        .domain
        .into_iter()
        .filter_map(|domain| {
            let value = domain
                .value
                .trim()
                .trim_end_matches('.')
                .to_ascii_lowercase();
            if value.is_empty() {
                return None;
            }

            let attrs = domain
                .attribute
                .into_iter()
                .filter_map(Attribute::from_domain_attribute)
                .collect::<Vec<_>>();

            if !selector.matches_attrs(&attrs) {
                return None;
            }

            let matcher = match DomainType::try_from(domain.r#type).ok()? {
                DomainType::Plain => GeositeMatcher::Keyword { value, attrs },
                DomainType::Regex => GeositeMatcher::Regex { value, attrs },
                DomainType::Domain => GeositeMatcher::Domain { value, attrs },
                DomainType::Full => GeositeMatcher::Full { value, attrs },
            };
            Some(matcher)
        })
        .collect())
}

pub fn load_categories(
    path: Option<&str>,
    categories: impl IntoIterator<Item = String>,
) -> HashMap<String, Vec<GeositeMatcher>> {
    let categories = categories
        .into_iter()
        .filter_map(|category| GeositeSelector::parse(&category))
        .collect::<HashSet<_>>();

    if categories.is_empty() {
        return HashMap::new();
    }

    let Some(path) = path.filter(|path| !path.trim().is_empty()) else {
        return HashMap::new();
    };

    let path = Path::new(path);
    let mut result = HashMap::new();
    for selector in categories {
        let key = selector.key();
        match load_category(path, &selector) {
            Ok(matchers) if !matchers.is_empty() => {
                result.insert(key, matchers);
            }
            Ok(_) => {
                warn!(selector = %key, path = %path.display(), "geosite dat category is empty")
            }
            Err(error) => {
                warn!(selector = %key, path = %path.display(), %error, "failed to load geosite dat category")
            }
        }
    }

    result
}

pub fn load_builtin_cn() -> Vec<GeositeMatcher> {
    include_str!("../../data/geosite-cn.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_ascii_lowercase)
        .map(|value| GeositeMatcher::Domain {
            value,
            attrs: Vec::new(),
        })
        .collect()
}

pub fn load_cn_with_fallback(path: Option<&str>) -> Vec<GeositeMatcher> {
    if let Some(path) = path.filter(|path| !path.trim().is_empty()) {
        let selector = GeositeSelector::category_only("CN");
        match load_category(Path::new(path), &selector) {
            Ok(matchers) if !matchers.is_empty() => return matchers,
            Ok(_) => warn!(
                path,
                "geosite dat does not contain CN category; using built-in seed"
            ),
            Err(error) => warn!(path, %error, "failed to load geosite dat; using built-in seed"),
        }
    }

    load_builtin_cn()
}

pub fn normalize_category(category: &str) -> String {
    GeositeSelector::parse(category)
        .map(|selector| selector.key())
        .unwrap_or_default()
}

pub fn matches(host: &str, matcher: &GeositeMatcher) -> bool {
    match matcher {
        GeositeMatcher::Domain { value, .. } => {
            host == value || host.ends_with(&format!(".{value}"))
        }
        GeositeMatcher::Full { value, .. } => host == value,
        GeositeMatcher::Keyword { value, .. } => host.contains(value),
        GeositeMatcher::Regex { value, .. } => Regex::new(value)
            .map(|regex| regex.is_match(host))
            .unwrap_or(false),
    }
}

pub fn to_pac_expr(matcher: &GeositeMatcher) -> Option<String> {
    match matcher {
        GeositeMatcher::Domain { value, .. } => Some(format!(
            r#"(host == "{}" || dnsDomainIs(host, ".{}"))"#,
            escape_js(value),
            escape_js(value)
        )),
        GeositeMatcher::Full { value, .. } => Some(format!(r#"host == "{}""#, escape_js(value))),
        GeositeMatcher::Keyword { value, .. } => {
            Some(format!(r#"host.indexOf("{}") >= 0"#, escape_js(value)))
        }
        GeositeMatcher::Regex { value, .. } => {
            Some(format!(r#"/{}/.test(host)"#, escape_js_regex(value)))
        }
    }
}

fn escape_js(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn escape_js_regex(value: &str) -> String {
    value.replace('\\', "\\\\").replace('/', "\\/")
}

impl GeositeSelector {
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim().trim_start_matches("geosite:");
        let mut parts = value.split('@');
        let category = parts.next()?.trim().to_ascii_uppercase();
        if category.is_empty() {
            return None;
        }

        let attr_filters = parts
            .filter_map(|part| parse_attr_filter(part.trim()))
            .collect();

        Some(Self {
            category,
            attr_filters,
        })
    }

    fn category_only(category: &str) -> Self {
        Self {
            category: category.to_ascii_uppercase(),
            attr_filters: Vec::new(),
        }
    }

    pub fn key(&self) -> String {
        let mut key = self.category.clone();
        for filter in &self.attr_filters {
            key.push('@');
            match filter {
                AttrFilter::Has(attr) => key.push_str(attr),
                AttrFilter::Not(attr) => {
                    key.push('!');
                    key.push_str(attr);
                }
                AttrFilter::Equals(attr, value) => {
                    key.push_str(attr);
                    key.push('=');
                    match value {
                        AttributeValue::Bool(value) => key.push_str(&value.to_string()),
                        AttributeValue::Int(value) => key.push_str(&value.to_string()),
                    }
                }
            }
        }
        key
    }

    fn matches_attrs(&self, attrs: &[Attribute]) -> bool {
        self.attr_filters.iter().all(|filter| match filter {
            AttrFilter::Has(key) => attrs.iter().any(|attr| attr.key == *key),
            AttrFilter::Not(key) => attrs.iter().all(|attr| attr.key != *key),
            AttrFilter::Equals(key, value) => attrs
                .iter()
                .any(|attr| attr.key == *key && attr.value == *value),
        })
    }
}

fn parse_attr_filter(value: &str) -> Option<AttrFilter> {
    if value.is_empty() {
        return None;
    }

    if let Some(key) = value.strip_prefix('!').or_else(|| value.strip_prefix('-')) {
        let key = normalize_attr_key(key);
        return (!key.is_empty()).then_some(AttrFilter::Not(key));
    }

    if let Some((key, value)) = value.split_once('=') {
        let key = normalize_attr_key(key);
        let value = value.trim();
        if key.is_empty() {
            return None;
        }
        let value = match value {
            "true" => AttributeValue::Bool(true),
            "false" => AttributeValue::Bool(false),
            _ => AttributeValue::Int(value.parse().ok()?),
        };
        return Some(AttrFilter::Equals(key, value));
    }

    let key = normalize_attr_key(value);
    (!key.is_empty()).then_some(AttrFilter::Has(key))
}

fn normalize_attr_key(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

impl Attribute {
    fn from_domain_attribute(attribute: DomainAttribute) -> Option<Self> {
        let key = normalize_attr_key(&attribute.key);
        if key.is_empty() {
            return None;
        }

        let value = match attribute.typed_value {
            Some(domain_attribute::TypedValue::BoolValue(value)) => AttributeValue::Bool(value),
            Some(domain_attribute::TypedValue::IntValue(value)) => AttributeValue::Int(value),
            None => AttributeValue::Bool(true),
        };

        Some(Self { key, value })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GeositeError {
    #[error("failed to read geosite dat: {0}")]
    Read(#[from] std::io::Error),
    #[error("failed to decode geosite dat: {0}")]
    Decode(#[from] prost::DecodeError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_cn_matches_domain_suffix() {
        let matchers = load_builtin_cn();
        assert!(matches(
            "www.baidu.com",
            matchers
                .iter()
                .find(|matcher| matches!(matcher, GeositeMatcher::Domain { value, .. } if value == "baidu.com"))
                .unwrap()
        ));
    }

    #[test]
    fn parses_geosite_selector_attrs() {
        let selector = GeositeSelector::parse("geosite:google@ads@!cn").unwrap();
        assert_eq!(selector.key(), "GOOGLE@ads@!cn");
    }

    #[test]
    fn filters_attributes() {
        let selector = GeositeSelector::parse("google@ads").unwrap();
        let attrs = vec![Attribute {
            key: "ads".into(),
            value: AttributeValue::Bool(true),
        }];
        assert!(selector.matches_attrs(&attrs));
    }

    #[test]
    fn regex_matcher_uses_regex_syntax() {
        assert!(matches(
            "mail.google.com",
            &GeositeMatcher::Regex {
                value: r"^mail\.google\.com$".into(),
                attrs: Vec::new()
            }
        ));
    }
}
