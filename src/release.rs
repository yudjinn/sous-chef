//! Yocto releases (T1.6): which one a workspace targets, and what differs between them.
//!
//! Wrynose is the primary target, Scarthgap is fully supported and Kirkstone is the floor.
//! Nothing outside this file compares codenames or version numbers: code asks
//! [`Release::has`] for a [`Feature`]. Each feature has a row in the "Releases" table of
//! `docs/bitbake-semantics.md` naming the source that was checked.

use std::fmt;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::workspace::Layer;

/// A supported Yocto release. Ordered oldest to newest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Release {
    Kirkstone,
    Langdale,
    Mickledore,
    Nanbield,
    Scarthgap,
    Styhead,
    Walnascar,
    Whinlatter,
    Wrynose,
}

/// Codenames older than the floor. They are recognised so that a layer naming one is
/// treated as "older than supported" and not as "newer than this table".
const OLDER_THAN_FLOOR: &[&str] = &[
    "honister",
    "hardknott",
    "gatesgarth",
    "dunfell",
    "zeus",
    "warrior",
    "thud",
    "sumo",
    "rocko",
    "pyro",
    "morty",
    "krogoth",
    "jethro",
    "fido",
    "dizzy",
    "daisy",
    "dora",
    "dylan",
    "danny",
    "denzil",
];

impl Release {
    /// Every supported release, oldest first.
    pub const ALL: [Release; 9] = [
        Release::Kirkstone,
        Release::Langdale,
        Release::Mickledore,
        Release::Nanbield,
        Release::Scarthgap,
        Release::Styhead,
        Release::Walnascar,
        Release::Whinlatter,
        Release::Wrynose,
    ];
    /// The oldest supported release.
    pub const FLOOR: Release = Release::Kirkstone;
    /// The newest release this table knows.
    pub const NEWEST: Release = Release::Wrynose;
    /// Assumed when nothing can be detected: the most recent LTS.
    pub const DEFAULT: Release = Release::Wrynose;

    pub fn from_codename(s: &str) -> Option<Release> {
        let s = s.trim();
        Release::ALL
            .into_iter()
            .find(|r| r.codename().eq_ignore_ascii_case(s))
    }

    pub fn codename(self) -> &'static str {
        match self {
            Release::Kirkstone => "kirkstone",
            Release::Langdale => "langdale",
            Release::Mickledore => "mickledore",
            Release::Nanbield => "nanbield",
            Release::Scarthgap => "scarthgap",
            Release::Styhead => "styhead",
            Release::Walnascar => "walnascar",
            Release::Whinlatter => "whinlatter",
            Release::Wrynose => "wrynose",
        }
    }

    /// The Yocto Project version number.
    pub fn version(self) -> &'static str {
        match self {
            Release::Kirkstone => "4.0",
            Release::Langdale => "4.1",
            Release::Mickledore => "4.2",
            Release::Nanbield => "4.3",
            Release::Scarthgap => "5.0",
            Release::Styhead => "5.1",
            Release::Walnascar => "5.2",
            Release::Whinlatter => "5.3",
            Release::Wrynose => "6.0",
        }
    }

    pub fn is_lts(self) -> bool {
        matches!(
            self,
            Release::Kirkstone | Release::Scarthgap | Release::Wrynose
        )
    }

    /// Whether this release has `f`.
    pub fn has(self, f: Feature) -> bool {
        self >= f.first() && f.last().is_none_or(|last| self <= last)
    }

    /// A codename that names a release older than the floor.
    pub fn is_older_than_floor(codename: &str) -> bool {
        let c = codename.trim();
        OLDER_THAN_FLOOR.iter().any(|o| o.eq_ignore_ascii_case(c))
    }
}

impl fmt::Display for Release {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.codename())
    }
}

/// A behaviour that differs between supported releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Feature {
    /// `classes-recipe/` and `classes-global/` are searched before `classes/`.
    SplitClassDirs,
    /// The `inherit_defer` directive.
    InheritDefer,
    /// The `include_all` directive.
    IncludeAll,
    /// `UNPACKDIR`: sources are unpacked there and not into `WORKDIR`.
    UnpackDir,
    /// `S = "${WORKDIR}"` is accepted. Later releases reject it with an error.
    WorkdirAsSourceDir,
    /// `WORKDIR` ends in `${PV}-${PR}`; later releases end it in `${PV}`.
    PrInWorkdir,
}

impl Feature {
    pub const ALL: [Feature; 6] = [
        Feature::SplitClassDirs,
        Feature::InheritDefer,
        Feature::IncludeAll,
        Feature::UnpackDir,
        Feature::WorkdirAsSourceDir,
        Feature::PrInWorkdir,
    ];

    /// The first supported release that has the feature.
    pub fn first(self) -> Release {
        match self {
            Feature::SplitClassDirs => Release::Langdale,
            Feature::InheritDefer => Release::Scarthgap,
            Feature::IncludeAll => Release::Walnascar,
            Feature::UnpackDir => Release::Styhead,
            Feature::WorkdirAsSourceDir | Feature::PrInWorkdir => Release::FLOOR,
        }
    }

    /// The last release that has the feature; `None` when it is still present.
    pub fn last(self) -> Option<Release> {
        match self {
            Feature::WorkdirAsSourceDir => Some(Release::Scarthgap),
            Feature::PrInWorkdir => Some(Release::Kirkstone),
            _ => None,
        }
    }

    /// Whether both ends of the range were checked against a source. A feature that is not
    /// verified must not be used to report an error.
    ///
    /// `PrInWorkdir`: Kirkstone has it and Scarthgap does not (both `bitbake.conf` files
    /// were read); the releases in between were not checked.
    pub fn verified(self) -> bool {
        !matches!(self, Feature::PrInWorkdir)
    }
}

/// Where the workspace's release came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseSource {
    /// `release` in `sous-chef.toml`.
    Config,
    /// `LAYERSERIES_CORENAMES` of the core layer.
    CoreNames,
    /// `LAYERSERIES_COMPAT` of the writable layers (all layers if none is writable).
    LayerCompat,
    /// Nothing to go on: [`Release::DEFAULT`].
    Assumed,
}

impl ReleaseSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseSource::Config => "sous-chef.toml",
            ReleaseSource::CoreNames => "LAYERSERIES_CORENAMES",
            ReleaseSource::LayerCompat => "LAYERSERIES_COMPAT",
            ReleaseSource::Assumed => "assumed",
        }
    }
}

/// The release a workspace targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseInfo {
    pub release: Release,
    pub source: ReleaseSource,
    /// Set when the detected name had to be mapped onto a supported release.
    pub note: Option<String>,
}

impl Default for ReleaseInfo {
    fn default() -> ReleaseInfo {
        ReleaseInfo {
            release: Release::DEFAULT,
            source: ReleaseSource::Assumed,
            note: None,
        }
    }
}

impl ReleaseInfo {
    /// One line for `status`: `wrynose (LAYERSERIES_CORENAMES)`, plus the note if any.
    pub fn describe(&self) -> String {
        let mut s = format!("{} ({})", self.release, self.source.as_str());
        if let Some(n) = &self.note {
            s.push_str("; ");
            s.push_str(n);
        }
        s
    }
}

/// What a list of codenames says: the newest supported release in it, or a mapping of
/// names outside the table onto the nearest end of it.
fn from_names(names: &[String]) -> Option<(Release, Option<String>)> {
    if let Some(r) = names.iter().filter_map(|n| Release::from_codename(n)).max() {
        return Some((r, None));
    }
    // No supported name. A name that is not a known old release is taken to be newer than
    // this table.
    if let Some(n) = names.iter().find(|n| !Release::is_older_than_floor(n)) {
        return Some((
            Release::NEWEST,
            Some(format!(
                "release {n} is not known to this version of sous-chef; treated as {}",
                Release::NEWEST
            )),
        ));
    }
    names.first().map(|n| {
        (
            Release::FLOOR,
            Some(format!(
                "release {n} is older than the oldest supported release; treated as {}",
                Release::FLOOR
            )),
        )
    })
}

/// Detects the workspace release, in order: the configured name, `LAYERSERIES_CORENAMES`,
/// the `LAYERSERIES_COMPAT` of the writable layers (all layers if none is writable), then
/// [`Release::DEFAULT`].
///
/// A configured name that is neither a supported release nor a known older one is an
/// error (most likely a typo); the message lists the supported names.
pub fn detect(
    configured: Option<&str>,
    corenames: &[String],
    layers: &[Layer],
) -> Result<ReleaseInfo> {
    if let Some(name) = configured {
        let names = [name.trim().to_string()];
        if Release::from_codename(name).is_none() && !Release::is_older_than_floor(name) {
            let known: Vec<&str> = Release::ALL.iter().map(|r| r.codename()).collect();
            return Err(Error::Config {
                msg: format!(
                    "unknown release {name:?} in sous-chef.toml; supported releases: {}",
                    known.join(", ")
                ),
            });
        }
        if let Some((release, note)) = from_names(&names) {
            return Ok(ReleaseInfo {
                release,
                source: ReleaseSource::Config,
                note,
            });
        }
    }
    if let Some((release, note)) = from_names(corenames) {
        return Ok(ReleaseInfo {
            release,
            source: ReleaseSource::CoreNames,
            note,
        });
    }
    let any_writable = layers.iter().any(|l| l.writable);
    let claimed = layers
        .iter()
        .filter(|l| l.writable || !any_writable)
        .flat_map(|l| l.compat.iter())
        .filter_map(|n| Release::from_codename(n))
        .max();
    if let Some(release) = claimed {
        return Ok(ReleaseInfo {
            release,
            source: ReleaseSource::LayerCompat,
            note: None,
        });
    }
    Ok(ReleaseInfo::default())
}

/// The releases a layer is linted against: every supported release it claims in
/// `LAYERSERIES_COMPAT`, down to the oldest, plus the workspace's release; sorted oldest
/// first. A layer that is not writable is checked against the workspace's release alone.
pub fn lint_releases(layer: &Layer, workspace: Release) -> Vec<Release> {
    let mut out = vec![workspace];
    if layer.writable {
        out.extend(layer.compat.iter().filter_map(|n| Release::from_codename(n)));
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn layer(name: &str, compat: &[&str], writable: bool) -> Layer {
        Layer {
            name: name.to_string(),
            root: PathBuf::from("/x").join(name),
            collections: vec![name.to_string()],
            priority: 5,
            compat: compat.iter().map(|s| s.to_string()).collect(),
            depends: Vec::new(),
            bbfiles: Vec::new(),
            writable,
        }
    }

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn releases_are_ordered_and_named() {
        assert!(Release::Kirkstone < Release::Scarthgap);
        assert!(Release::Scarthgap < Release::Wrynose);
        assert!(Release::ALL.windows(2).all(|w| w[0] < w[1]));
        for r in Release::ALL {
            assert_eq!(Release::from_codename(r.codename()), Some(r));
        }
        assert_eq!(Release::from_codename(" Scarthgap "), Some(Release::Scarthgap));
        assert_eq!(Release::from_codename("dunfell"), None);
        assert_eq!(Release::Wrynose.version(), "6.0");
        let lts: Vec<Release> = Release::ALL.into_iter().filter(|r| r.is_lts()).collect();
        assert_eq!(
            lts,
            [Release::Kirkstone, Release::Scarthgap, Release::Wrynose]
        );
    }

    #[test]
    fn features_at_the_three_lts_releases() {
        use Feature::*;
        // (feature, kirkstone, scarthgap, wrynose)
        let table = [
            (SplitClassDirs, false, true, true),
            (InheritDefer, false, true, true),
            (IncludeAll, false, false, true),
            (UnpackDir, false, false, true),
            (WorkdirAsSourceDir, true, true, false),
            (PrInWorkdir, true, false, false),
        ];
        assert_eq!(table.len(), Feature::ALL.len());
        for (f, k, s, w) in table {
            assert_eq!(Release::Kirkstone.has(f), k, "{f:?} at kirkstone");
            assert_eq!(Release::Scarthgap.has(f), s, "{f:?} at scarthgap");
            assert_eq!(Release::Wrynose.has(f), w, "{f:?} at wrynose");
        }
        // The releases next to each boundary.
        assert!(Release::Langdale.has(SplitClassDirs));
        assert!(!Release::Nanbield.has(InheritDefer));
        assert!(!Release::Styhead.has(IncludeAll));
        assert!(Release::Walnascar.has(IncludeAll));
        assert!(Release::Styhead.has(UnpackDir));
        assert!(!Release::Styhead.has(WorkdirAsSourceDir));
        assert!(!PrInWorkdir.verified());
        assert!(UnpackDir.verified());
    }

    #[test]
    fn config_wins_over_everything() {
        let layers = [layer("mine", &["wrynose"], true)];
        let info = detect(Some("scarthgap"), &names(&["wrynose"]), &layers).unwrap();
        assert_eq!(info.release, Release::Scarthgap);
        assert_eq!(info.source, ReleaseSource::Config);
        assert_eq!(info.note, None);
    }

    #[test]
    fn config_typo_is_an_error_listing_the_releases() {
        let e = detect(Some("scarthgab"), &[], &[]).unwrap_err().to_string();
        assert!(e.contains("scarthgab"), "{e}");
        assert!(e.contains("kirkstone") && e.contains("wrynose"), "{e}");
    }

    #[test]
    fn corenames_come_before_layer_compat() {
        let layers = [layer("mine", &["kirkstone"], true)];
        let info = detect(None, &names(&["scarthgap", "styhead"]), &layers).unwrap();
        assert_eq!(info.release, Release::Styhead);
        assert_eq!(info.source, ReleaseSource::CoreNames);
    }

    #[test]
    fn layer_compat_uses_writable_layers_when_there_are_any() {
        let layers = [
            layer("upstream", &["wrynose"], false),
            layer("mine", &["kirkstone", "scarthgap"], true),
        ];
        let info = detect(None, &[], &layers).unwrap();
        assert_eq!(info.release, Release::Scarthgap);
        assert_eq!(info.source, ReleaseSource::LayerCompat);

        let none_writable = [
            layer("a", &["kirkstone"], false),
            layer("b", &["nanbield"], false),
        ];
        let info = detect(None, &[], &none_writable).unwrap();
        assert_eq!(info.release, Release::Nanbield);
    }

    #[test]
    fn nothing_to_go_on_assumes_the_default() {
        let info = detect(None, &[], &[layer("mine", &[], true)]).unwrap();
        assert_eq!(info, ReleaseInfo::default());
        assert_eq!(info.release, Release::Wrynose);
        assert_eq!(info.source, ReleaseSource::Assumed);
        assert_eq!(info.describe(), "wrynose (assumed)");
    }

    #[test]
    fn a_newer_codename_is_treated_as_the_newest_known() {
        let info = detect(None, &names(&["zzyzx"]), &[]).unwrap();
        assert_eq!(info.release, Release::NEWEST);
        assert_eq!(info.source, ReleaseSource::CoreNames);
        let note = info.note.unwrap();
        assert!(note.contains("zzyzx") && note.contains("wrynose"), "{note}");
    }

    #[test]
    fn an_older_codename_is_treated_as_the_floor() {
        let info = detect(None, &names(&["dunfell"]), &[]).unwrap();
        assert_eq!(info.release, Release::Kirkstone);
        assert!(info.note.unwrap().contains("older"));

        let info = detect(Some("dunfell"), &[], &[]).unwrap();
        assert_eq!(info.release, Release::Kirkstone);
        assert_eq!(info.source, ReleaseSource::Config);
        assert!(info.note.is_some());

        // An old name in a layer's compat is not a supported claim.
        let info = detect(None, &[], &[layer("legacy", &["dunfell"], true)]).unwrap();
        assert_eq!(info.source, ReleaseSource::Assumed);
    }

    #[test]
    fn lint_releases_for_writable_and_upstream_layers() {
        let mine = layer("mine", &["kirkstone", "scarthgap", "dunfell"], true);
        assert_eq!(
            lint_releases(&mine, Release::Wrynose),
            [Release::Kirkstone, Release::Scarthgap, Release::Wrynose]
        );
        let upstream = layer("upstream", &["kirkstone", "scarthgap"], false);
        assert_eq!(lint_releases(&upstream, Release::Wrynose), [Release::Wrynose]);
        let same = layer("mine", &["wrynose"], true);
        assert_eq!(lint_releases(&same, Release::Wrynose), [Release::Wrynose]);
    }
}
