//! What OE-core's CVE check reads from a recipe's metadata, and the per-recipe JSON it
//! writes. Used by `recipe show=cve` (docs/mcp-tools.md). Nothing here runs anything.
//!
//! The rules are OE-core's `lib/oe/cve_check.py` (see "CVE metadata" in
//! docs/bitbake-semantics.md). They changed between releases, and which ones apply is read
//! from the `lib/oe/cve_check.py` of the workspace ([`CveRules::from_lib`]), so a release
//! between the three supported ones gets the rules its OE-core has:
//!
//! - `CVE_STATUS` (and `CVE_CHECK_STATUSMAP`, `CVE_STATUS_GROUPS`) exists where
//!   `decode_cve_status` does (Nanbield and later; Kirkstone has `CVE_CHECK_IGNORE` only);
//! - the `detail: cpe:vendor:product:description` form of a status and the product match
//!   (`has_cve_product_match`) exist where `decode_cve_status` compares a part with `"cpe"`;
//! - patch contents are read line by line (`parse_cves_from_patch_contents`: every id on a
//!   line starting with `CVE:`) where that function exists; before, the regular expression
//!   `CVE:( CVE-\d{4}-\d+)+` was matched anywhere in the text.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::error::{Error, Result};

/// Which of OE-core's CVE rules the workspace's `lib/oe/cve_check.py` has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CveRules {
    /// `decode_cve_status` exists: `CVE_STATUS`, `CVE_CHECK_STATUSMAP`, `CVE_STATUS_GROUPS`.
    pub status: bool,
    /// `decode_cve_status` reads `detail: cpe:vendor:product:description` and
    /// `get_patched_cves` keeps only entries whose vendor and product match `CVE_PRODUCT`.
    pub cpe: bool,
    /// `parse_cves_from_patch_contents`: ids on lines starting with `CVE:`; file-name ids
    /// need at least four digits after the year.
    pub line_rule: bool,
}

impl CveRules {
    /// The rules of the newest OE-core (Wrynose), used when no `lib/oe/cve_check.py` is found.
    pub const NEWEST: CveRules = CveRules {
        status: true,
        cpe: true,
        line_rule: true,
    };

    /// The rules the text of an OE-core `lib/oe/cve_check.py` implements: the presence of
    /// `def decode_cve_status`, of the `== "cpe"` comparison in it, and of
    /// `def parse_cves_from_patch_contents`.
    pub fn from_lib(text: &str) -> CveRules {
        CveRules {
            status: text.contains("def decode_cve_status"),
            cpe: text.contains("== \"cpe\""),
            line_rule: text.contains("def parse_cves_from_patch_contents"),
        }
    }
}

struct Patterns {
    name_new: Regex,
    name_old: Regex,
    id_new: Regex,
    tag_old: Regex,
    id: Regex,
}

static PATTERNS: LazyLock<Option<Patterns>> = LazyLock::new(|| {
    Some(Patterns {
        name_new: Regex::new(r"(?i).*(CVE-\d{4}-\d{4,})").ok()?,
        name_old: Regex::new(r"(?i).*(CVE-\d{4}-\d+)").ok()?,
        id_new: Regex::new(r"CVE-\d{4}-\d{4,}").ok()?,
        tag_old: Regex::new(r"CVE:( CVE-\d{4}-\d+)+").ok()?,
        id: Regex::new(r"^CVE-\d{4}-\d+$").ok()?,
    })
});

/// Whether `s` has the form of a CVE id (`CVE-YYYY-N...`).
pub fn is_cve_id(s: &str) -> bool {
    PATTERNS.as_ref().is_some_and(|p| p.id.is_match(s))
}

/// The CVE id in a patch's path as `get_patched_cves` finds it: the last `CVE-YYYY-N` in
/// it, in any case, returned upper case (`parse_cve_from_filename`). OE-core matches the
/// patch's local path, directories included.
pub fn cve_from_filename(path: &str, rules: CveRules) -> Option<String> {
    let p = PATTERNS.as_ref()?;
    let re = if rules.line_rule {
        &p.name_new
    } else {
        &p.name_old
    };
    re.captures(path)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_uppercase())
}

/// The CVE ids a patch's text declares: with the line rule, every `CVE-YYYY-NNNN` on each
/// line that starts with `CVE:` (`parse_cves_from_patch_contents`); else each match of
/// `CVE:( CVE-\d{4}-\d+)+` anywhere in the text, split after the `CVE:`.
pub fn cves_from_patch_text(text: &str, rules: CveRules) -> Vec<String> {
    let Some(p) = PATTERNS.as_ref() else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let mut push = |id: &str| {
        if !out.iter().any(|o| o == id) {
            out.push(id.to_string());
        }
    };
    if rules.line_rule {
        for line in text.split('\n').filter(|l| l.starts_with("CVE:")) {
            for m in p.id_new.find_iter(line) {
                push(m.as_str());
            }
        }
    } else {
        for m in p.tag_old.find_iter(text) {
            let ids = m.as_str().get(4..).unwrap_or("");
            for id in ids.split_whitespace() {
                push(id);
            }
        }
    }
    out.sort();
    out
}

/// A `CVE_STATUS[<id>]` value split as `decode_cve_status` splits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedStatus {
    /// The part before the first `:`, not stripped (OE-core looks it up as it is).
    pub detail: String,
    pub description: String,
    pub vendor: String,
    pub product: String,
    /// `detail: cpe:...` without both a vendor and a product: OE-core warns and ignores
    /// the CPE part (or, with exactly `detail: cpe:vendor:product`, fails on the missing
    /// description).
    pub malformed_cpe: bool,
}

/// `decode_cve_status` for one value. With [`CveRules::cpe`]:
/// `status.split(':', 4)`; `detail: cpe:vendor:product:description` sets vendor and
/// product; `detail: cpe...` with fewer parts is malformed; otherwise the description is
/// everything after the first `:`, stripped. Without it: `split(':', 1)`.
pub fn decode_status(value: &str, rules: CveRules) -> DecodedStatus {
    let mut out = DecodedStatus {
        detail: value.split(':').next().unwrap_or("").to_string(),
        description: value
            .split_once(':')
            .map(|(_, d)| d.trim().to_string())
            .unwrap_or_default(),
        vendor: "*".into(),
        product: "*".into(),
        malformed_cpe: false,
    };
    if !rules.cpe {
        return out;
    }
    let parts: Vec<&str> = value.splitn(5, ':').collect();
    if parts.len() >= 2 && parts[1].trim() == "cpe" {
        if parts.len() >= 5 {
            out.vendor = parts[2].trim().to_string();
            out.product = parts[3].trim().to_string();
            out.description = parts[4].trim().to_string();
        } else {
            out.malformed_cpe = true;
            out.description = String::new();
        }
    }
    out
}

/// `has_cve_product_match`: some word of `CVE_PRODUCT` (`vendor:product` or `product`)
/// matches the status's vendor and product, `*` matching anything.
pub fn product_matches(status: &DecodedStatus, products: &str) -> bool {
    products.split_whitespace().any(|p| {
        let (vendor, product) = p.split_once(':').unwrap_or(("*", p));
        (vendor == status.vendor || status.vendor == "*")
            && (product == status.product || status.product == "*")
    })
}

/// One issue of a CVE check JSON report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckIssue {
    pub id: String,
    /// `Patched`, `Unpatched`, `Ignored` (and `Unknown` on Wrynose).
    pub status: String,
    pub detail: Option<String>,
    /// `scorev2`, `scorev3`, `scorev4` as written (a number or a string).
    pub scores: Vec<(String, String)>,
}

impl CheckIssue {
    /// The scores for display, `v3 7.5, v2 5.0`, newest scheme first, without empty or zero
    /// ones; `-` when none.
    pub fn score_text(&self) -> String {
        let mut parts = Vec::new();
        for key in ["scorev4", "scorev3", "scorev2"] {
            if let Some((_, v)) = self.scores.iter().find(|(k, _)| k == key)
                && !v.is_empty()
                && v != "0.0"
                && v != "0"
            {
                parts.push(format!("{} {v}", key.trim_start_matches("score")));
            }
        }
        if parts.is_empty() {
            "-".into()
        } else {
            parts.join(", ")
        }
    }
}

/// The package of a recipe in a CVE check JSON report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckPackage {
    pub name: String,
    pub version: String,
    pub issues: Vec<CheckIssue>,
}

impl CheckPackage {
    /// How many issues have each status, by status.
    pub fn counts(&self) -> BTreeMap<String, usize> {
        let mut m = BTreeMap::new();
        for i in &self.issues {
            *m.entry(i.status.clone()).or_insert(0) += 1;
        }
        m
    }
}

fn json_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The package named `pn` in a CVE check JSON report: `{"version": "1", "package": [{"name",
/// "version", "issue": [{"id", "status", "scorev2", ...}]}]}`, as `cve-check.bbclass`
/// `cve_write_data_json` and `vex.bbclass` `cve_write_data_json` write it (one package per
/// recipe file, several in a summary). `None` when no package has that name.
pub fn read_check_json(text: &str, pn: &str) -> Result<Option<CheckPackage>> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| Error::Invalid {
        msg: format!("not a CVE check JSON report: {e}"),
    })?;
    let Some(packages) = v.get("package").and_then(|p| p.as_array()) else {
        return Err(Error::Invalid {
            msg: "not a CVE check JSON report: no \"package\" list".into(),
        });
    };
    for p in packages {
        if p.get("name").and_then(|n| n.as_str()) != Some(pn) {
            continue;
        }
        let issues = p
            .get("issue")
            .and_then(|i| i.as_array())
            .map(|list| {
                list.iter()
                    .map(|i| CheckIssue {
                        id: i.get("id").map(json_text).unwrap_or_default(),
                        status: i.get("status").map(json_text).unwrap_or_default(),
                        detail: i.get("detail").map(json_text).filter(|d| !d.is_empty()),
                        scores: ["scorev2", "scorev3", "scorev4"]
                            .iter()
                            .filter_map(|k| i.get(*k).map(|v| (k.to_string(), json_text(v))))
                            .collect(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Ok(Some(CheckPackage {
            name: pn.to_string(),
            version: p.get("version").map(json_text).unwrap_or_default(),
            issues,
        }));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: CveRules = CveRules {
        status: true,
        cpe: false,
        line_rule: false,
    };

    #[test]
    fn file_names() {
        let r = CveRules::NEWEST;
        assert_eq!(
            cve_from_filename("/a/CVE-2023-1234-and-cve-2024-56789.patch", r).as_deref(),
            Some("CVE-2024-56789")
        );
        assert_eq!(cve_from_filename("/a/CVE-2023-123.patch", r), None);
        assert_eq!(
            cve_from_filename("/a/CVE-2023-123.patch", OLD).as_deref(),
            Some("CVE-2023-123")
        );
    }

    #[test]
    fn patch_text() {
        let text = "Subject: x\nCVE: CVE-2023-0001, CVE-2023-0002 and CVE-2023-0003\n x CVE: CVE-2023-0009\n";
        assert_eq!(
            cves_from_patch_text(text, CveRules::NEWEST),
            vec!["CVE-2023-0001", "CVE-2023-0002", "CVE-2023-0003"]
        );
        // The old rule: the tag anywhere, ids separated by single spaces.
        assert_eq!(
            cves_from_patch_text(text, OLD),
            vec!["CVE-2023-0001", "CVE-2023-0009"]
        );
    }

    #[test]
    fn decode() {
        let d = decode_status(
            "fixed-version: cpe:acme:tool:fixed in 2.0",
            CveRules::NEWEST,
        );
        assert_eq!(
            (d.detail.as_str(), d.vendor.as_str(), d.product.as_str()),
            ("fixed-version", "acme", "tool")
        );
        assert_eq!(d.description, "fixed in 2.0");
        assert!(product_matches(&d, "acme:tool other"));
        assert!(!product_matches(&d, "tool"));
        let d = decode_status("fixed-version: cpe:acme:tool:fixed in 2.0", OLD);
        assert_eq!(d.description, "cpe:acme:tool:fixed in 2.0");
        assert!(decode_status("ignored: cpe:x", CveRules::NEWEST).malformed_cpe);
        let d = decode_status("disputed", CveRules::NEWEST);
        assert_eq!(
            (d.detail.as_str(), d.description.as_str()),
            ("disputed", "")
        );
    }
}
